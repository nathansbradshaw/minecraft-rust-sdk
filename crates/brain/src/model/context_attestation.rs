//! Whitelisted metadata for the exact final context, never prompt/source text.
use serde_json::{Value, json};

fn hash(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn menu_binding(menu: &Value) -> Value {
    let id = menu["menu_id"]
        .as_i64()
        .filter(|n| (0..=i32::MAX as i64).contains(n));
    let state = menu["state_id"]
        .as_i64()
        .filter(|n| (0..=i32::MAX as i64).contains(n));
    let kind = menu["type"]
        .as_str()
        .filter(|s| s.len() <= 128 && minecraft_sdk::registry_id(s));
    json!({"available":id.is_some()&&state.is_some()&&kind.is_some(),
        "menu_id":id,"state_id":state,"type":kind})
}
fn active_plan(context: &Value) -> Value {
    let plan = &context["active_plan"];
    let content = plan["plan"]
        .is_object()
        .then(|| serde_json::to_vec(&plan["plan"]).ok())
        .flatten();
    let actual_hash = content.as_ref().map(|v| crate::digest(v));
    let bytes = content.as_ref().map(Vec::len);
    let hash_matches = hash(&plan["content_sha256"])
        .zip(actual_hash.as_deref())
        .is_some_and(|(a, b)| a == b);
    let bytes_match = plan["content_bytes"]
        .as_u64()
        .zip(bytes)
        .is_some_and(|(a, b)| a == b as u64);
    let revision = plan["goal_revision"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()));
    json!({"present":plan.is_object(),"active":plan["active"]==true,
        "omitted_for_context_budget":plan["omitted_for_context_budget"]==true,
        "goal_revision":revision,"actual_content_sha256":actual_hash,"actual_content_bytes":bytes,
        "hash_matches":hash_matches,"bytes_match":bytes_match,
        "complete_content":plan["active"]==true&&hash_matches&&bytes_match&&bytes.is_some_and(|n|n<=crate::goal_plan::CONTENT_BYTES),
        "authority":"model_hypothesis"})
}
pub(super) fn summarize(context: &Value, context_sha256: &str) -> Value {
    let source = &context["active_program_source"];
    let content = source["content"].as_str();
    let actual_hash = content.map(|s| crate::digest(s.as_bytes()));
    let actual_bytes = content.map(str::len);
    let declared_hash = hash(&source["source_sha256"]);
    let declared_bytes = source["source_bytes"].as_u64();
    let hash_matches = declared_hash
        .zip(actual_hash.as_deref())
        .is_some_and(|(a, b)| a == b);
    let bytes_match = declared_bytes
        .zip(actual_bytes)
        .is_some_and(|(a, b)| a == b as u64);
    let format_valid = source["format"] == "active-program-source-v1";
    let complete = source["content_complete"] == true;
    let result = &context["last_tool_result"]["result"];
    let sdk_read = context["last_tool_result"]["tool"] == "read"
        && result["path"] == "sdk://reference"
        && matches!(result["view"].as_str(), Some("file" | "file_range"));
    let reference = sdk_read.then(|| result["content"].as_str()).flatten();
    let start = sdk_read.then(|| result["start_line"].as_u64()).flatten();
    let end = sdk_read.then(|| result["end_line"].as_u64()).flatten();
    let range_valid =
        result["view"] == "file_range" && start.zip(end).is_some_and(|(a, b)| a > 0 && b >= a);
    json!({"format":"final-model-context-attestation-v1","context_sha256":context_sha256,
        "active_plan":active_plan(context),
        "completion_evaluation_phase":(context["completion_requirements"]["evaluation_phase"]=="final_success_claim_only").then_some("final_success_claim_only"),
        "observation_menu":menu_binding(&context["observation"]["menu"]),
        "last_tool_after_menu":menu_binding(&context["last_tool_result"]["result"]["after"]["menu"]),
        "active_source":{"present":source.is_object(),"format_valid":format_valid,"content_complete":complete,
            "declared_source_sha256":declared_hash,"actual_content_sha256":actual_hash,
            "declared_source_bytes":declared_bytes,"actual_content_bytes":actual_bytes,
            "hash_matches":hash_matches,"bytes_match":bytes_match},
        "sdk_reference_read":{"present":sdk_read,"actual_content_sha256":reference.map(|s|crate::digest(s.as_bytes())),
            "actual_content_bytes":reference.map(str::len),"start_line":start,"end_line":end,
            "range_valid":range_valid,"range_complete":if sdk_read{result["range_complete"].as_bool()}else{None}},
        "complete_source_with_reference_page":format_valid && complete && hash_matches && bytes_match
            && sdk_read && range_valid && reference.is_some_and(|s|!s.is_empty()),
        "scope":"Final serialized request-context metadata associated with this accepted response; no source/prompt/path text. This attests context supply, not model understanding, tool execution or native gameplay."})
}

#[cfg(test)]
pub(super) fn fixture() -> Value {
    let content = "fn main() { let _ = \"é\"; } // SOURCE_SENTINEL";
    json!({"active_program_source":{"format":"active-program-source-v1","content_complete":true,
            "content":content,"source_sha256":crate::digest(content.as_bytes()),"source_bytes":content.len(),
            "path":"/workspace/PRIVATE_PATH_SENTINEL.rs"},
        "last_tool_result":{"tool":"read","result":{"view":"file_range","path":"sdk://reference",
            "content":"SDK_REFERENCE_SENTINEL","start_line":62,"end_line":64,"range_complete":true}},
        "private_ignored_field":"SECRET_SENTINEL"})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivered_plan_and_menu_metadata_never_echo_content_or_accept_partial_content() {
        let plan = json!({"next_step":"PRIVATE_PLAN_SENTINEL","dependencies":[],"resources":[]});
        let bytes = serde_json::to_vec(&plan).unwrap();
        let mut context = json!({"active_plan":{"active":true,"goal_revision":"7","plan":plan,
            "content_sha256":crate::digest(&bytes),"content_bytes":bytes.len()},
            "completion_requirements":{"evaluation_phase":"final_success_claim_only"},
            "observation":{"menu":{"menu_id":1,"state_id":7,"type":"minecraft:crafting"}},
            "last_tool_result":{"result":{"after":{"menu":{"menu_id":1,"state_id":1,"type":"minecraft:crafting"}}}}});
        let view = summarize(&context, &"a".repeat(64));
        assert_eq!(view["active_plan"]["complete_content"], true);
        assert_eq!(view["observation_menu"]["state_id"], 7);
        assert_eq!(view["last_tool_after_menu"]["state_id"], 1);
        assert!(!view.to_string().contains("PRIVATE_"));
        context["active_plan"]["plan"]["next_step"] = json!("changed");
        context["active_plan"]["goal_revision"] = json!("PRIVATE_REVISION_SENTINEL");
        context["observation"]["menu"]["type"] = json!("/PRIVATE_PATH_SENTINEL");
        let bad = summarize(&context, &"a".repeat(64));
        assert_eq!(bad["active_plan"]["complete_content"], false);
        assert_eq!(bad["active_plan"]["goal_revision"], Value::Null);
        assert_eq!(bad["observation_menu"]["available"], false);
        assert!(!bad.to_string().contains("PRIVATE_"));
        context["active_plan"] = json!({"active":false,"omitted_for_context_budget":true});
        let omitted = summarize(&context, &"a".repeat(64));
        assert_eq!(omitted["active_plan"]["omitted_for_context_budget"], true);
        assert_eq!(omitted["active_plan"]["complete_content"], false);
        assert!(omitted["active_plan"]["actual_content_sha256"].is_null());
        context.as_object_mut().unwrap().remove("active_plan");
        let absent = summarize(&context, &"a".repeat(64));
        assert_eq!(absent["active_plan"]["present"], false);
        assert_eq!(absent["active_plan"]["complete_content"], false);
    }
    #[test]
    fn final_metadata_binds_complete_source_and_actual_reference_without_leaking_text() {
        let context = fixture();
        let final_hash = crate::digest(serde_json::to_string(&context).unwrap().as_bytes());
        let view = summarize(&context, &final_hash);
        assert_eq!(view["context_sha256"], final_hash);
        assert_eq!(
            view["active_source"]["actual_content_sha256"],
            context["active_program_source"]["source_sha256"]
        );
        assert_eq!(
            view["active_source"]["actual_content_bytes"],
            context["active_program_source"]["source_bytes"]
        );
        assert_eq!(
            view["sdk_reference_read"]["actual_content_sha256"],
            crate::digest(b"SDK_REFERENCE_SENTINEL")
        );
        assert_eq!(view["sdk_reference_read"]["start_line"], 62);
        assert_eq!(view["complete_source_with_reference_page"], true);
        let text = view.to_string();
        assert!(text.len() <= 2048);
        for private in [
            "SOURCE_SENTINEL",
            "SDK_REFERENCE_SENTINEL",
            "PRIVATE_PATH_SENTINEL",
            "SECRET_SENTINEL",
        ] {
            assert!(!text.contains(private), "Never log {private}");
        }
    }
    #[test]
    fn incomplete_mismatched_or_private_reads_cannot_claim_coexistence() {
        for change in [
            "incomplete",
            "bad_hash",
            "bad_bytes",
            "private_read",
            "bad_range",
            "no_source",
        ] {
            let mut context = fixture();
            match change {
                "incomplete" => context["active_program_source"]["content_complete"] = json!(false),
                "bad_hash" => {
                    context["active_program_source"]["source_sha256"] =
                        json!("PRIVATE_HASH_SENTINEL")
                }
                "bad_bytes" => context["active_program_source"]["source_bytes"] = json!(999),
                "private_read" => {
                    context["last_tool_result"]["result"]["path"] =
                        json!("/workspace/PRIVATE_PATH_SENTINEL")
                }
                "bad_range" => context["last_tool_result"]["result"]["end_line"] = json!(1),
                "no_source" => context["active_program_source"] = Value::Null,
                _ => unreachable!(),
            }
            let view = summarize(&context, &"a".repeat(64));
            assert_eq!(
                view["complete_source_with_reference_page"], false,
                "{change}"
            );
            assert!(!view.to_string().contains("PRIVATE_"));
            if change == "private_read" {
                assert!(view["sdk_reference_read"]["actual_content_sha256"].is_null());
            }
        }
    }
}
