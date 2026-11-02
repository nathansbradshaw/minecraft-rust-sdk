use crate::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

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
        ensure(
            status == 200,
            &format!(
                "model_http_{status}: {}",
                envelope["error"].as_str().unwrap_or("response unavailable")
            ),
        )?;
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
