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
}
pub const TOOLS: [&str; 13] = [
    "develop",
    "observe",
    "sdk",
    "status",
    "reconcile",
    "read",
    "write",
    "list",
    "search",
    "terminal",
    "remember",
    "goal",
    "complete",
];
pub fn schema() -> Value {
    json!({"type":"object","properties":{"tool":{"type":"string","enum":TOOLS},"input":{"type":"string","maxLength":10000},"path":{"type":"string","maxLength":256},"summary":{"type":"string","maxLength":512},"notes":{"type":"string","maxLength":2048}},"required":["tool","input","path","summary","notes"],"additionalProperties":false})
}
pub struct Model {
    http: reqwest::Client,
    endpoint: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub message: String,
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
        let model = if develop { "gpt-6.1-sol" } else { "gpt-6-luna" };
        let (content, usage) = self
            .request(
                context,
                model,
                if develop { "develop" } else { "plan" },
                schema(),
                include_str!("system.md"),
                4096,
            )
            .await?;
        let choice: Choice = serde_json::from_str(&content)?;
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
        let format = json!({"type":"object","properties":{"message":{"type":"string","maxLength":256},"goal":{"type":"string","maxLength":2048},"reason":{"type":"string","maxLength":512}},"required":["message","goal","reason"],"additionalProperties":false});
        let (content, usage) = self
            .request(
                context,
                "gpt-6-luna",
                "plan",
                format,
                include_str!("conversation.md"),
                1024,
            )
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
        let envelope: Value = serde_json::from_slice(&bytes)?;
        if status != 200 {
            return Err(Box::new(http_failure(status, &envelope, crate::now())));
        }
        ensure(envelope["done"] == true, "Incomplete model response")?;
        let content = envelope["message"]["content"]
            .as_str()
            .ok_or("Model content missing")?;
        Ok((
            content.to_owned(),
            json!({"model":model,"elapsed_ms":started.elapsed().as_millis(),"response_bytes":bytes.len(),"host_tools_executed":false}),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_bounded_explicit_allocation_clock_events_allow_retry() {
        let daily = http_failure(
            429,
            &json!({"error":"allocation_daily_limit","retry_at_utc":200}),
            100,
        );
        assert_eq!(retry_at(&daily), Some(200));
        for (status, class, at) in [
            (429, "allocation_daily_limit", 100),
            (429, "allocation_daily_limit", 86501),
            (429, "allocation_clock_rollback", 200),
            (503, "model_queue_timeout", 200),
            (401, "unauthorized", 200),
            (429, "allocation_limit", 200),
        ] {
            assert_eq!(
                retry_at(&http_failure(
                    status,
                    &json!({"error":class,"retry_at_utc":at}),
                    100
                )),
                None
            );
        }
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
