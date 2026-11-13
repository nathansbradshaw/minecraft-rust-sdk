use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub mod fixture;

/// Diagnose recorded decisions; never infer that a non-read tool succeeded.
pub fn analyze(events: &[Value]) -> Result<Value> {
    let mut seen = HashSet::new();
    let mut choices = 0u64;
    let mut read_streak = 0u64;
    let mut max_read_streak = 0u64;
    let mut duplicates = 0u64;
    let mut model_ms = 0u64;
    let mut missing_timing = 0u64;
    let mut first_repeat = Value::Null;
    let mut errors = 0u64;
    let mut last_sequence = None;
    for row in events {
        let event = row["event"].as_str().ok_or("Event kind required")?;
        // Body/chat workers can multiplex their own sequence counters into
        // stdout. Only the main decision/tool stream is ordered here.
        if !matches!(event, "choice" | "tool_result") {
            continue;
        }
        let sequence = row["sequence"].as_u64().ok_or("Event sequence required")?;
        if last_sequence.is_some_and(|previous| sequence <= previous) {
            return Err("Event sequences must increase".into());
        }
        last_sequence = Some(sequence);
        let data = &row["data"];
        if event == "choice" {
            choices += 1;
            if let Some(ms) = data["usage"]["elapsed_ms"].as_u64() {
                model_ms = model_ms
                    .checked_add(ms)
                    .ok_or("Elapsed duration overflow")?;
            } else {
                missing_timing += 1;
            }
            let tool = data["choice"]["tool"]
                .as_str()
                .ok_or("Choice tool required")?;
            if is_read(tool) {
                read_streak += 1;
                max_read_streak = max_read_streak.max(read_streak);
            } else {
                read_streak = 0;
                seen.clear();
            }
        } else if event == "tool_result" {
            let tool = data["tool"].as_str().ok_or("Result tool required")?;
            if !is_read(tool) {
                seen.clear();
                read_streak = 0;
                continue;
            }
            if !data["error"].is_null() {
                errors += 1;
                continue;
            }
            if data["result"]["view"] == "menu" {
                let result = &data["result"];
                let menu = &result["menu"];
                let page = &result["page"];
                if !menu["menu_id"].is_u64()
                    || !menu["state_id"].is_u64()
                    || !menu["type"].is_string()
                    || !menu["slots"].is_array()
                    || !page["offset"].is_u64()
                    || !page["total_in_snapshot"].is_u64()
                {
                    return Err("Menu result lacks recorded page bindings".into());
                }
                // Advancing observation clocks are not new menu evidence. Keep
                // the page and all native menu fields: a different page, cursor,
                // permission, component or state ID can change the next action.
                let signature = digest(&serde_json::to_vec(&json!({"menu":menu,
                    "offset":page["offset"],"total":page["total_in_snapshot"],
                    "complete":page["complete"],"next_offset":page["next_offset"]}))?);
                if !seen.insert(signature) {
                    duplicates += 1;
                    if first_repeat.is_null() {
                        first_repeat = json!({"sequence":sequence,"summed_model_ms":model_ms});
                    }
                }
            }
        }
    }
    Ok(
        json!({"format":"bot-dev-diagnosis-v1","model_choices":choices,
        "summed_model_ms":model_ms,"choices_missing_timing":missing_timing,
        "max_read_only_choices":max_read_streak,"repeated_menu_results":duplicates,
        "first_repeat":first_repeat,"read_errors":errors,
        "status":if duplicates>0 {"repeated_menu_reads"} else {"no_repeated_menu_reads_detected"},
        "scope":"Recorded successful main choice events only; model-error durations and concurrent conversation inference are excluded. Repeated pages are a development diagnostic, not a command to stop a bot, a gameplay failure oracle or proof of a faster model."}),
    )
}

fn is_read(tool: &str) -> bool {
    matches!(
        tool,
        "observe" | "query" | "read" | "status" | "list" | "history" | "search"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_failure_reproduces_285_seconds_of_unchanged_menu_reads() {
        let report = analyze(&fixture::events().unwrap()).unwrap();
        assert_eq!(report["model_choices"], 20);
        assert_eq!(report["summed_model_ms"], 285186);
        assert_eq!(report["repeated_menu_results"], 17);
        assert_eq!(report["first_repeat"]["sequence"], 367);
        assert_eq!(report["first_repeat"]["summed_model_ms"], 41248);
        assert_eq!(report["choices_missing_timing"], 0);
    }

    #[test]
    fn different_pages_and_changed_menu_facts_are_new_evidence() {
        let events = fixture::events().unwrap();
        assert_eq!(analyze(&events[..4]).unwrap()["repeated_menu_results"], 0);
        for field in ["state_id", "carried", "slots"] {
            let mut pair = events[..2].to_vec();
            let mut changed = events[4..6].to_vec();
            changed[1]["data"]["result"]["menu"][field] = match field {
                "state_id" => json!(11),
                "carried" => json!({"item":"minecraft:birch_planks","count":2}),
                _ => json!([]),
            };
            pair.extend(changed);
            assert_eq!(
                analyze(&pair).unwrap()["repeated_menu_results"],
                0,
                "{field}"
            );
        }
    }

    #[test]
    fn execution_boundary_resets_read_diagnosis_without_claiming_progress() {
        let mut events = fixture::events().unwrap();
        events[2]["data"]["choice"]["tool"] = json!("run");
        events[3]["data"]["tool"] = json!("run");
        assert_eq!(analyze(&events[..6]).unwrap()["repeated_menu_results"], 0);
        assert_eq!(analyze(&events[..6]).unwrap()["max_read_only_choices"], 1);
    }

    #[test]
    fn missing_timing_errors_and_invalid_records_are_explicit() {
        let mut events = fixture::events().unwrap();
        events[0]["data"]["usage"] = Value::Null;
        events[5]["data"]["error"] = json!("read failed");
        let report = analyze(&events[..6]).unwrap();
        assert_eq!(report["choices_missing_timing"], 1);
        assert_eq!(report["read_errors"], 1);
        assert_eq!(report["repeated_menu_results"], 0);
        events[1]["data"]["result"]["menu"] = Value::Null;
        assert!(analyze(&events).is_err());
        events[1]["sequence"] = json!(0);
        assert!(analyze(&events).is_err());
    }

    #[test]
    fn multiplexed_body_sequences_do_not_corrupt_the_decision_stream() {
        let mut events = fixture::events().unwrap();
        events.insert(2, json!({"event":"body_started","sequence":1,"data":{}}));
        assert_eq!(analyze(&events).unwrap()["repeated_menu_results"], 17);
    }
}
