//! Cost-control hints, never authority for gameplay success or skill reliability.
use crate::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

/// Excludes receipt IDs, timing and output text: rerunning the same failed
/// mutation under another ID is not a new experiment or evidence of progress.
pub fn experiment_signature(source: &str, result: &Value) -> String {
    let actions:Vec<_>=result["local_action_feedback"]["requests"].as_array().into_iter().flatten().map(|row|json!({"action":row["request"]["action"],"outcome":row["response"]["outcome"],"error_code":row["response"]["error"]["code"],"verified":row["response"]["native_effect_verified"]})).collect();
    let mut semantic = json!({"source":source,"exit_code":result["exit_code"],"success":result["success"],"timed_out":result["timed_out"],"error":result["error"],"compiler_errors":result["compiler_diagnostics"]["errors"],"actions":actions,"inventory_delta":result["inventory_delta"]});
    if result["navigation"].is_object() {
        // A different typed boundary matters. Clocks, paths, cumulative counters
        // and radius/target jitter cannot turn a stalled call into new evidence.
        semantic["navigation"] = json!({"status":result["navigation"]["report"]["status"],
            "pending":result["navigation"]["report"]["pending_action"].is_string(),
            "error_code":result["navigation"]["error"]["code"]});
    }
    crate::digest(semantic.to_string().as_bytes())
}
/// Changes to clocks, delivered chat or moving entity coordinates do not reset
/// diagnosis. Changed terrain, owned inventory, position or needs can matter.
pub fn environment_signature(observed: &crate::Observation) -> String {
    let p = &observed.player;
    let position: Vec<_> = p["position"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| n.as_f64().map(f64::floor))
        .collect();
    let blocks: Vec<_> = observed.world["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| json!({"position":b["position"],"block":b["block"],"state":b["state"]}))
        .collect();
    crate::digest(json!({"dimension":p["dimension"],"position":position,"health":p["health"],"food":p["food"],"inventory":p["inventory"],"blocks":blocks,"menu":observed.menu}).to_string().as_bytes())
}
#[derive(Debug, PartialEq, Eq)]
pub enum Investigation {
    Start,
    Continue,
    Exhausted,
}
pub fn investigation(subject: &str, previous: &str, remaining: u32) -> Investigation {
    if subject != previous {
        Investigation::Start
    } else if remaining > 0 {
        Investigation::Continue
    } else {
        Investigation::Exhausted
    }
}

pub fn workspace_fingerprint(root: &Path) -> Result<String> {
    fingerprint(root, None)
}

pub fn workspace_fingerprint_excluding_state(root: &Path, state: &Path) -> Result<String> {
    fingerprint(&fs::canonicalize(root)?, Some(&fs::canonicalize(state)?))
}

fn fingerprint(root: &Path, state: Option<&Path>) -> Result<String> {
    fn visit(
        root: &Path,
        path: &Path,
        state: Option<&Path>,
        depth: u8,
        remaining: &mut usize,
        hash: &mut Sha256,
    ) -> Result<()> {
        ensure(
            depth < 12,
            "Workspace source nesting exceeds inspection budget",
        )?;
        let mut rows: Vec<_> = fs::read_dir(path)?.collect::<std::io::Result<_>>()?;
        rows.sort_by_key(|r| r.file_name());
        for row in rows {
            if matches!(row.file_name().to_str(), Some("target" | ".git" | ".cache")) {
                continue;
            }
            let path = row.path();
            if state == Some(path.as_path()) {
                continue;
            }
            let meta = fs::symlink_metadata(&path)?;
            hash.update(path.strip_prefix(root)?.to_string_lossy().as_bytes());
            if meta.is_dir() {
                visit(root, &path, state, depth + 1, remaining, hash)?;
            } else if meta.is_file() {
                let size = usize::try_from(meta.len())?;
                ensure(
                    size <= *remaining,
                    "Workspace source exceeds fingerprint budget",
                )?;
                *remaining -= size;
                hash.update(fs::read(&path)?);
            } else if meta.file_type().is_symlink() {
                hash.update(fs::read_link(&path)?.to_string_lossy().as_bytes());
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    visit(root, root, state, 0, &mut (16 * 1024 * 1024), &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

pub fn executed(tool: &str, result: &serde_json::Value, before: &str, after: &str) -> bool {
    let navigation_moved = tool == "navigate"
        && result["position_before"]
            .as_array()
            .zip(result["position_after"].as_array())
            .is_some_and(|(before, after)| {
                before.len() == 3
                    && after.len() == 3
                    && (0..3)
                        .map(|i| {
                            before[i]
                                .as_f64()
                                .zip(after[i].as_f64())
                                .map(|(a, b)| (a - b).powi(2))
                                .unwrap_or(0.)
                        })
                        .sum::<f64>()
                        > 0.04
            })
        && result["local_action_feedback"]["requests"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| {
                row["request"]["action"]["kind"] == "start_move"
                    && row["response"]["outcome"] == "observed"
                    && row["response"]["native_effect_verified"] == true
            });
    before != after
        || navigation_moved
        || tool == "complete"
        || (tool == "sdk" && !crate::tool_failed(result))
        || result["inventory_changed"] == true
}

/// Count the first inspection of a new goal as well as subsequent inspections.
/// This is a deployment cost guard, never evidence of gameplay progress.
pub fn next_inspection_count(previous: u32, same_goal: bool, executed: bool) -> u32 {
    if executed {
        0
    } else if same_goal {
        previous.saturating_add(1)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_cost_guard_requires_observed_own_movement_not_exit_zero_or_body_motion() {
        let mut result = json!({"success":true,"position_before":[0,64,0],"position_after":[2,64,0],"local_action_feedback":{"requests":[]}});
        assert!(!executed("navigate", &result, "same", "same"));
        result["local_action_feedback"]["requests"] = json!([{"request":{"action":{"kind":"start_move"}},"response":{"outcome":"running","native_effect_verified":false}}]);
        assert!(!executed("navigate", &result, "same", "same"));
        result["local_action_feedback"]["requests"][0]["response"] =
            json!({"outcome":"observed","native_effect_verified":true});
        assert!(executed("navigate", &result, "same", "same"));
        result["position_after"] = json!([0, 64, 0]);
        assert!(!executed("navigate", &result, "same", "same"));
    }
    #[test]
    fn new_receipt_ids_and_wall_clocks_do_not_masquerade_as_new_information() {
        let mut result = json!({"success":true,"exit_code":0,"local_action_feedback":{"requests":[{"request":{"action_id":"first","action":{"kind":"dig","block":{"x":1,"y":2,"z":3},"face":1,"stage":"start"}},"response":{"action_id":"first","outcome":"uncertain","native_effect_verified":false}}]}});
        let first = experiment_signature("same source", &result);
        result["local_action_feedback"]["requests"][0]["request"]["action_id"] = json!("new-id");
        result["local_action_feedback"]["requests"][0]["response"]["action_id"] = json!("new-id");
        result["elapsed_ms"] = json!(10000);
        result["stdout"] = json!("a changed game clock");
        assert_eq!(first, experiment_signature("same source", &result));
        result["local_action_feedback"]["requests"][0]["response"]["outcome"] = json!("observed");
        assert_ne!(first, experiment_signature("same source", &result));
        assert_ne!(first, experiment_signature("repaired source", &result));
    }
    #[test]
    fn diagnostic_budget_is_per_changed_subject_not_per_restart() {
        assert_eq!(investigation("new", "", 0), Investigation::Start);
        for left in (1..=4).rev() {
            assert_eq!(investigation("new", "new", left), Investigation::Continue);
        }
        assert_eq!(investigation("new", "new", 0), Investigation::Exhausted);
        assert_eq!(
            investigation("changed source or facts", "new", 0),
            Investigation::Start
        );
    }
    #[test]
    fn saved_feedback_in_nested_state_is_not_a_source_edit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        let state = dir.path().join("state");
        fs::create_dir(&state).unwrap();
        let before = workspace_fingerprint_excluding_state(dir.path(), &state).unwrap();
        fs::write(
            state.join("terminal-result.txt"),
            "new output, unchanged source",
        )
        .unwrap();
        assert_eq!(
            before,
            workspace_fingerprint_excluding_state(dir.path(), &state).unwrap()
        );
        fs::write(dir.path().join("main.rs"), "fn main() { todo!() }").unwrap();
        assert_ne!(
            before,
            workspace_fingerprint_excluding_state(dir.path(), &state).unwrap()
        );
    }
    #[test]
    fn the_eighth_unchanged_inspection_reaches_the_configured_limit() {
        let mut count = next_inspection_count(63, false, false);
        assert_eq!(count, 1);
        for expected in 2..=8 {
            count = next_inspection_count(count, true, false);
            assert_eq!(count, expected);
        }
        assert_eq!(next_inspection_count(count, true, true), 0);
        assert_eq!(next_inspection_count(count, false, false), 1);
    }
    #[test]
    fn inspection_output_and_build_cache_do_not_masquerade_as_source_progress() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        let a = workspace_fingerprint(dir.path()).unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/compiler-cache"), "cache").unwrap();
        assert_eq!(a, workspace_fingerprint(dir.path()).unwrap());
        assert!(!executed(
            "terminal",
            &serde_json::json!({"success":true,"stdout":"read another file"}),
            &a,
            &a
        ));
        fs::write(
            dir.path().join("main.rs"),
            "fn main() { println!(\"experiment\"); }",
        )
        .unwrap();
        let b = workspace_fingerprint(dir.path()).unwrap();
        assert!(executed("write", &serde_json::Value::Null, &a, &b));
        assert!(!executed(
            "sdk",
            &serde_json::json!({"receipt":{"outcome":"rejected"}}),
            &b,
            &b
        ));
    }
}
