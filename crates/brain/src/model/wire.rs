//! Bound the actual HTTP JSON, including escaped context, prompt and schema.
use crate::{Result, digest, ensure};
use serde_json::{Value, json};

pub(super) const MAX_BYTES: usize = 65536;
const CONTENT_BYTES: usize = 32768;
// These are optional historical/retrievable views, never current native facts,
// goal/completion contracts, pending IDs, latest feedback or repair contracts.
const OPTIONAL: [&str; 9] = [
    "recent",
    "completed_tool_activity",
    "working_notes",
    "memory",
    "inspected_containers",
    "spatial_searches",
    "sdk_reference",
    "active_program_source",
    "active_plan",
];

pub(super) struct Contract<'a> {
    pub model: &'a str,
    pub purpose: &'a str,
    pub format: &'a Value,
    pub prompt: &'a str,
    pub tokens: u32,
}
pub(super) struct Prepared {
    pub context: Value,
    pub bytes: Vec<u8>,
    pub metadata: Value,
}
#[derive(Debug)]
pub(super) struct Unfit(pub Value);
impl std::fmt::Display for Unfit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model_wire_budget_unfit: mandatory frame cannot fit the allocated request contract"
        )
    }
}
impl std::error::Error for Unfit {}

fn protected(context: &Value) -> String {
    let mut value = context.clone();
    if let Some(object) = value.as_object_mut() {
        for key in OPTIONAL {
            object.remove(key);
        }
    }
    digest(value.to_string().as_bytes())
}
impl Contract<'_> {
    pub fn encode(&self, context: &Value) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(
            &json!({"model":self.model,"stream":false,
            "purpose":self.purpose,"options":{"num_predict":self.tokens},"format":self.format,
            "messages":[{"role":"system","content":self.prompt},
                {"role":"user","content":serde_json::to_string(context)?}]}),
        )?)
    }
    pub fn prepare(&self, context: &Value, budget: usize) -> Result<Prepared> {
        ensure(context.is_object(), "Model context object required")?;
        ensure(
            budget <= MAX_BYTES,
            "Cannot increase the allocated wire budget",
        )?;
        ensure(
            self.prompt.len() <= CONTENT_BYTES,
            "System content exceeds allocated contract",
        )?;
        let initial_protected = protected(context);
        let original = self.encode(context)?;
        let mut context = context.clone();
        let mut bytes = original.clone();
        let mut omitted = Vec::new();
        for key in OPTIONAL {
            if bytes.len() <= budget && context.to_string().len() <= CONTENT_BYTES {
                break;
            }
            let Some(value) = context
                .get(key)
                .filter(|v| !v.is_null() && v["omitted_for_wire_budget"] != true)
            else {
                continue;
            };
            let original_view = serde_json::to_vec(value)?;
            let mut marker = json!({"omitted_for_wire_budget":true,
                "original_bytes":original_view.len(),"original_sha256":digest(&original_view),
                "scope":"Optional view omitted whole; inspect original journal/source if available. Not new evidence."});
            if key == "active_plan" {
                marker["active"] = json!(false);
                marker["omitted_for_context_budget"] = json!(true);
                marker["authority"] = json!("model_hypothesis");
            } else if key == "active_program_source" {
                marker["content_complete"] = json!(false);
            } else if key == "sdk_reference" {
                marker["read"] = json!("sdk://reference");
            }
            if serde_json::to_vec(&marker)?.len() >= original_view.len() {
                continue;
            }
            context[key] = marker;
            omitted.push(key);
            bytes = self.encode(&context)?;
        }
        let final_protected = protected(&context);
        ensure(
            initial_protected == final_protected,
            "Wire fitting changed protected contracts",
        )?;
        let metadata = json!({"format":"model-wire-budget-v1","hard_limit_bytes":MAX_BYTES,
            "requested_budget_bytes":budget,"original_wire_bytes":original.len(),
            "wire_bytes":bytes.len(),"wire_sha256":digest(&bytes),
            "context_bytes":context.to_string().len(),"context_sha256":digest(context.to_string().as_bytes()),
            "omitted_views":omitted,"protected_context_sha256":final_protected,
            "protected_context_unchanged":true,"prompt_sha256":digest(self.prompt.as_bytes()),
            "schema_sha256":digest(self.format.to_string().as_bytes()),"allowances_changed":false});
        if bytes.len() > budget || context.to_string().len() > CONTENT_BYTES {
            return Err(Box::new(Unfit(metadata)));
        }
        Ok(Prepared {
            context,
            bytes,
            metadata,
        })
    }
}
