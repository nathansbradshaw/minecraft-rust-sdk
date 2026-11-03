use crate::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Debug)]
struct HttpFailure {
    status: u16,
    class: String,
    retry_at: Option<i64>,
}
impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "model_http_{}: {}", self.status, self.class)
    }
}
impl std::error::Error for HttpFailure {}
fn http_failure(status: u16, envelope: &Value, now: i64) -> HttpFailure {
    let class = envelope["error"]
        .as_str()
        .filter(|s| {
            s.len() <= 96
                && s.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        })
        .unwrap_or("response_unavailable")
        .to_owned();
    let retry_at = if status == 429
        && matches!(
            class.as_str(),
            "allocation_daily_limit" | "allocation_pacing"
        ) {
        envelope["retry_at_utc"]
            .as_i64()
            .filter(|at| *at > now && *at <= now.saturating_add(86400))
    } else {
        None
    };
    HttpFailure {
        status,
        class,
        retry_at,
    }
}
/// Only an explicit, bounded daily-reset or pacing timestamp permits a retry.
/// Authentication/schema/queue/provider failures still require investigation.
pub fn retry_at(error: &(dyn std::error::Error + Send + Sync + 'static)) -> Option<i64> {
    error.downcast_ref::<HttpFailure>().and_then(|e| e.retry_at)
}

/// A daily allocation rejection applies only to the requested model. Queue,
/// pacing, authentication and schema errors do not authorize model hopping.
pub(crate) fn daily_reset(error: &(dyn std::error::Error + Send + Sync + 'static)) -> Option<i64> {
    error.downcast_ref::<HttpFailure>().and_then(|e| {
        (e.status == 429 && e.class == "allocation_daily_limit")
            .then_some(e.retry_at)
            .flatten()
    })
}

/// Container-local coordination for the shared allocated model credentials.
/// The broker remains authoritative for global jobs and daily budgets.
#[derive(Clone)]
pub struct Lanes([std::sync::Arc<tokio::sync::Semaphore>; 2]);
impl Default for Lanes {
    fn default() -> Self {
        Self(std::array::from_fn(|_| {
            std::sync::Arc::new(tokio::sync::Semaphore::new(1))
        }))
    }
}
impl Lanes {
    pub async fn acquire(&self, develop: bool) -> Result<tokio::sync::OwnedSemaphorePermit> {
        Ok(self.0[usize::from(develop)].clone().acquire_owned().await?)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub tool: String,
    pub input: String,
    #[serde(default)]
    pub path: String,
    pub summary: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub experiment: bool,
}
pub const TOOLS: [&str; 16] = [
    "develop",
    "observe",
    "query",
    "navigate",
    "sdk",
    "status",
    "reconcile",
    "read",
    "write",
    "list",
    "search",
    "terminal",
    "run",
    "remember",
    "goal",
    "complete",
];
pub fn schema() -> Value {
    json!({"type":"object","description":"One command for the remote bot-container executor. Returning this record schedules its tool after inference; no inference-host tool is needed.","properties":{"tool":{"type":"string","enum":TOOLS,"description":"Container instruction to execute next: gameplay, workspace editing, compilation, investigation or goal management."},"input":{"type":"string","maxLength":10000,"description":"Actual tool input: Rust source for write, command for terminal, binary name for run, serialized typed action for sdk."},"path":{"type":"string","maxLength":256,"description":"Relative destination for write; empty for other instructions."},"summary":{"type":"string","maxLength":512},"notes":{"type":"string","maxLength":2048},"experiment":{"type":"boolean"}},"required":["tool","input","path","summary","notes","experiment"],"additionalProperties":false})
}
pub struct Model {
    http: reqwest::Client,
    endpoint: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalAction {
    Continue,
    Replace,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub message: String,
    pub goal_action: GoalAction,
    pub goal: String,
    pub reason: String,
}
impl Model {
    pub fn new(endpoint: &str, key_file: &Path) -> Result<Self> {
        let url = reqwest::Url::parse(endpoint)?;
        ensure(
            url.scheme() == "https"
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "HTTPS model origin required",
        )?;
        let key = crate::private_read(key_file, 256)?;
        let key = std::str::from_utf8(&key)?.trim();
        ensure(
            key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid allocated model key",
        )?;
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        Ok(Self {
            http: reqwest::Client::builder()
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                // Includes the compatible broker's bounded FIFO queue and its
                // inference deadline, rather than abandoning an admitted job.
                .timeout(Duration::from_secs(125))
                .build()?,
            endpoint: endpoint.into(),
        })
    }
    pub async fn choose(&self, context: &Value, develop: bool) -> Result<(Choice, Value)> {
        self.choose_for(context, develop, develop).await
    }
    /// Work purpose survives allocation fallback; it does not select a provider
    /// or change the character's allowances. Conversation remains independent.
    pub async fn choose_for(
        &self,
        context: &Value,
        use_sol: bool,
        development: bool,
    ) -> Result<(Choice, Value)> {
        let model = if use_sol { "gpt-6.1-sol" } else { "gpt-6-luna" };
        let (content, usage) = self
            .request(
                context,
                model,
                if development { "develop" } else { "plan" },
                schema(),
                include_str!("system.md"),
                4096,
            )
            .await?;
        let choice: Choice =
            serde_json::from_str(&content).map_err(|e| format!("Invalid model tool JSON: {e}"))?;
        ensure(
            TOOLS.contains(&choice.tool.as_str())
                && choice.input.len() <= 10000
                && choice.path.len() <= 256
                && choice.summary.len() <= 1024,
            "Invalid tool choice",
        )?;
        ensure(choice.notes.len() <= 4096, "Working notes exceed budget")?;
        Ok((choice, usage))
    }
    pub async fn converse(&self, context: &Value) -> Result<(Conversation, Value)> {
        let path = std::env::var_os("MC_SDK_CONVERSATION_PROMPT");
        let prompt = conversation_prompt(path.as_deref().map(Path::new))?;
        let format = json!({"type":"object","properties":{"message":{"type":"string","maxLength":256},"goal_action":{"type":"string","enum":["continue","replace"]},"goal":{"type":"string","maxLength":2048},"reason":{"type":"string","maxLength":512}},"required":["message","goal_action","goal","reason"],"additionalProperties":false});
        let (content, usage) = self
            .request(context, "gpt-6-luna", "plan", format, &prompt, 1024)
            .await?;
        let reply: Conversation = serde_json::from_str(&content)?;
        ensure(
            reply.message.len() <= 256 && reply.goal.len() <= 2048 && reply.reason.len() <= 1024,
            "Conversation exceeds its output budget",
        )?;
        if !reply.message.is_empty() {
            minecraft_sdk::Action::Chat {
                message: reply.message.clone(),
            }
            .validate()?;
        }
        Ok((reply, usage))
    }
    async fn request(
        &self,
        context: &Value,
        model: &str,
        purpose: &str,
        format: Value,
        prompt: &str,
        tokens: u32,
    ) -> Result<(String, Value)> {
        let context = serde_json::to_string(context)?;
        ensure(
            context.len() <= 32768,
            "Model context exceeds its declared budget",
        )?;
        let body = json!({"model":model,"stream":false,"purpose":purpose,"options":{"num_predict":tokens},"format":format,"messages":[{"role":"system","content":prompt},{"role":"user","content":context}]});
        let started = std::time::Instant::now();
        let mut response = self.http.post(&self.endpoint).json(&body).send().await?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure(
                bytes.len() + chunk.len() <= 32768,
                "Model response exceeds budget",
            )?;
            bytes.extend(chunk);
        }
        let envelope = response_envelope(status, &bytes)?;
        if status != 200 {
            return Err(Box::new(http_failure(status, &envelope, crate::now())));
        }
        ensure(envelope["done"] == true, "Incomplete model response")?;
        let content = envelope["message"]["content"]
            .as_str()
            .ok_or("Model content missing")?;
        Ok((
            content.to_owned(),
            json!({"model":model,"purpose":purpose,"provider_reasoning_effort":envelope["reasoning_effort"].as_str().filter(|s|matches!(*s,"low"|"medium")),"provider_usage":{"input_tokens":envelope["usage"]["input_tokens"].as_u64(),"cached_input_tokens":envelope["usage"]["cached_input_tokens"].as_u64(),"output_tokens":envelope["usage"]["output_tokens"].as_u64()},"elapsed_ms":started.elapsed().as_millis(),"response_bytes":bytes.len(),"prompt_sha256":crate::digest(prompt.as_bytes()),"host_tools_executed":false}),
        ))
    }
}
fn response_envelope(status: u16, bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|_| {
        // Proxy error pages may contain deployment information. Retain the
        // HTTP boundary without copying untrusted response bodies into context.
        Box::new(http_failure(
            status,
            &json!({"error":"non_json_response"}),
            crate::now(),
        )) as Box<dyn std::error::Error + Send + Sync>
    })
}
fn conversation_prompt(path: Option<&Path>) -> Result<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let Some(path) = path else {
        return Ok(include_str!("conversation.md").into());
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure(
        file.metadata()?.is_file(),
        "Conversation prompt must be a regular file",
    )?;
    let mut prompt = String::new();
    file.take(16385).read_to_string(&mut prompt)?;
    ensure(
        !prompt.trim().is_empty() && prompt.len() <= 16384,
        "Conversation prompt must contain at most 16384 UTF-8 bytes",
    )?;
    Ok(prompt)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn development_fallback_keeps_luna_allocation_and_development_purpose() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            for (purpose, effort) in [("plan", "low"), ("develop", "medium")] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    header.push(socket.read_u8().await.unwrap());
                    assert!(header.len() < 8192);
                }
                let header = String::from_utf8(header).unwrap();
                let size = header
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(size <= 65536);
                let mut body = vec![0; size];
                socket.read_exact(&mut body).await.unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["model"], "gpt-6-luna");
                assert_eq!(body["purpose"], purpose);
                let answer=json!({"tool":"observe","input":"","path":"","summary":"Inspect","notes":"","experiment":false}).to_string();
                let reply=json!({"done":true,"reasoning_effort":effort,"usage":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":5,"private":"must not be copied"},"message":{"content":answer}}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.unwrap();
            }
        });
        let model = Model {
            http: reqwest::Client::new(),
            endpoint,
        };
        for (development, purpose, effort) in [(false, "plan", "low"), (true, "develop", "medium")]
        {
            let (_, usage) = model
                .choose_for(&json!({"goal":"same context"}), false, development)
                .await
                .unwrap();
            assert_eq!(usage["model"], "gpt-6-luna");
            assert_eq!(usage["purpose"], purpose);
            assert_eq!(usage["provider_reasoning_effort"], effort);
            assert_eq!(
                usage["provider_usage"],
                json!({"input_tokens":12,"cached_input_tokens":3,"output_tokens":5})
            );
        }
        upstream.await.unwrap();
    }
    #[test]
    fn proxy_error_pages_preserve_status_without_leaking_the_response_or_retrying() {
        let error = response_envelope(502, b"<html>private upstream detail</html>").unwrap_err();
        assert_eq!(error.to_string(), "model_http_502: non_json_response");
        assert_eq!(retry_at(error.as_ref()), None);
        assert_eq!(
            response_envelope(200, br#"{"done":true}"#).unwrap()["done"],
            true
        );
    }
    #[test]
    fn conversation_prompt_can_change_atomically_without_reloading_the_player() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.md");
        std::fs::write(&path, "first prompt").unwrap();
        let before = conversation_prompt(Some(&path)).unwrap();
        let next = dir.path().join("next.md");
        std::fs::write(&next, "new prompt with UTF-8: east →").unwrap();
        std::fs::rename(next, &path).unwrap();
        let after = conversation_prompt(Some(&path)).unwrap();
        assert_ne!(before, after);
        assert_eq!(after, "new prompt with UTF-8: east →");
        std::fs::write(&path, vec![b'x'; 16385]).unwrap();
        assert!(conversation_prompt(Some(&path)).is_err());
        std::fs::write(&path, "  ").unwrap();
        assert!(conversation_prompt(Some(&path)).is_err());
        assert!(conversation_prompt(Some(dir.path())).is_err());
        assert!(conversation_prompt(None).unwrap().contains("goal_action"));
    }
    #[test]
    fn only_bounded_explicit_allocation_clock_events_allow_retry() {
        let daily = http_failure(
            429,
            &json!({"error":"allocation_daily_limit","retry_at_utc":200}),
            100,
        );
        assert_eq!(retry_at(&daily), Some(200));
        assert_eq!(daily_reset(&daily), Some(200));
        let dir = tempfile::tempdir().unwrap();
        let j = crate::Journal::open(&dir.path().join("limits.sqlite")).unwrap();
        assert!(j.remember_daily_reset(true, &daily, 100).unwrap());
        assert!(!j.model_available(true, 100).unwrap());
        assert!(j.model_available(false, 100).unwrap());
        assert!(!j.remember_daily_reset(true, &daily, 200).unwrap());
        for (status, class, at) in [
            (429, "allocation_daily_limit", 100),
            (429, "allocation_daily_limit", 86501),
            (429, "allocation_clock_rollback", 200),
            (503, "model_queue_timeout", 200),
            (401, "unauthorized", 200),
            (429, "allocation_limit", 200),
        ] {
            let failure = http_failure(status, &json!({"error":class,"retry_at_utc":at}), 100);
            assert_eq!(retry_at(&failure), None);
            assert_eq!(daily_reset(&failure), None);
            assert!(!j.remember_daily_reset(false, &failure, 100).unwrap());
        }
        assert_eq!(
            daily_reset(&http_failure(
                429,
                &json!({"error":"allocation_pacing","retry_at_utc":101}),
                100
            )),
            None
        );
        assert_eq!(
            retry_at(&http_failure(
                429,
                &json!({"error":"allocation_pacing","retry_at_utc":101}),
                100
            )),
            Some(101)
        );
    }
    #[tokio::test]
    async fn same_model_callers_wait_while_the_other_model_remains_independent() {
        let lanes = Lanes::default();
        let routine = lanes.acquire(false).await.unwrap();
        let development = tokio::time::timeout(Duration::from_millis(100), lanes.acquire(true))
            .await
            .expect("Luna must not reserve Sol")
            .unwrap();
        let conversation = {
            let lanes = lanes.clone();
            tokio::spawn(async move { lanes.acquire(false).await })
        };
        tokio::task::yield_now().await;
        assert!(
            !conversation.is_finished(),
            "same-model work must wait locally"
        );
        drop(routine);
        let conversation = tokio::time::timeout(Duration::from_millis(100), conversation)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(conversation);
        let reused = tokio::time::timeout(Duration::from_millis(100), lanes.acquire(false))
            .await
            .expect("completed or failed caller must release its slot")
            .unwrap();
        drop((development, reused));
    }
    #[test]
    fn tool_contract_allows_real_coding_and_gameplay() {
        let s = schema();
        assert_eq!(s["additionalProperties"], false);
        for t in ["terminal", "write", "sdk", "remember"] {
            assert!(
                s["properties"]["tool"]["enum"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == t)
            );
        }
        assert!(
            serde_json::from_str::<Choice>(
                r#"{"tool":"observe","input":"","summary":"Inspect","actor":"other"}"#
            )
            .is_err()
        );
    }
    #[test]
    fn source_is_one_json_string_without_nested_encoding() {
        let source = "fn main() {\n    println!(\"a\\nb\");\n}\n";
        let record =
            json!({"tool":"write","input":source,"path":"src/main.rs","summary":"Write Rust"});
        let choice: Choice = serde_json::from_str(&record.to_string()).unwrap();
        assert_eq!(choice.input, source);
        assert_eq!(choice.path, "src/main.rs");
    }
}
