//! Carry one verified client source across reference reads without extra turns.
use serde_json::{Value, json};
use std::fs;

const FIELD: &str = "active_program_source";
const VIEW_BUDGET: usize = 6000;

fn source(context: &Value) -> Option<Value> {
    let experiment = &context["last_experiment"];
    let revision = context["goal_revision"]
        .as_str()
        .filter(|s| !s.is_empty())?;
    if !matches!(experiment["tool"].as_str(), Some("run" | "terminal"))
        || experiment["goal_revision"] != revision
        || context["last_tool_result"]["goal_revision"] != revision
    {
        return None;
    }
    let root = fs::canonicalize(context["workspace"].as_str()?).ok()?;
    // Match the existing repair guard: changed or unreconstructable workspaces
    // retain ordinary tools, never an apparently current old source view.
    let fingerprint = crate::progress::workspace_fingerprint(&root).ok()?;
    if experiment["source_sha256"] != fingerprint {
        return None;
    }
    // Reuse the single Cargo-entry resolver and its private raw stdout hash,
    // exact binary/unique-entry, workspace containment and4096-byte guards.
    let mut view = super::repair::run_source(&root, experiment)?;
    view["format"] = json!("active-program-source-v1");
    view["goal_revision"] = json!(revision);
    view["workspace_sha256"] = json!(fingerprint);
    view["compiled_entry_stdout"] = experiment["result"]["output_files"]["stdout"].clone();
    view["scope"] = json!(
        "Complete current client source identified by a hash-bound retained Cargo stream and unchanged workspace/goal. Available across reference reads during this coding session. This supplies editable source, not a compile, gameplay or completion claim, and grants no model turns."
    );
    (serde_json::to_vec(&view).ok()?.len() <= VIEW_BUDGET).then_some(view)
}

/// The original frame is already fitted. Optional source cannot make an
/// otherwise usable frame fail its budget or override an existing repair view.
pub(super) fn prepare(context: &mut Value, enabled: bool) -> bool {
    let Some(object) = context.as_object_mut() else {
        return false;
    };
    object.remove(FIELD);
    if !enabled {
        return false;
    }
    let Some(view) = source(context) else {
        return false;
    };
    if [
        "compiler_repair_source",
        "gameplay_repair_source",
        "coding_candidate_source",
    ]
    .iter()
    .any(|key| {
        context[*key]["content_complete"] == true
            && context[*key]["source_sha256"] == view["source_sha256"]
            && context[*key]["content"] == view["content"]
    }) {
        return false;
    }
    let previous = context.clone();
    context[FIELD] = view;
    if crate::fit_context(context).is_err() {
        *context = previous;
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;
    use std::path::Path;

    struct Fixture {
        dir: tempfile::TempDir,
        root: std::path::PathBuf,
        file: std::path::PathBuf,
        context: Value,
        text: String,
    }
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("program");
        fs::create_dir_all(root.join("src/bin")).unwrap();
        let file = root.join("src/bin/wood_crafting.rs");
        let text = format!(
            "// Bounded source/context fixture, never executed.\n{}\nfn main() {{ println!(\"preparation only; goal remains incomplete\"); }}\n",
            "// retained source line\n".repeat(100)
        );
        fs::write(&file, &text).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"fixture\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        let raw=json!({"reason":"compiler-artifact","target":{"name":"wood_crafting","kind":["bin"],"src_path":file}}).to_string()+"\n";
        let files = crate::output::save(
            &dir.path().join("terminal-output"),
            "run-1",
            raw.as_bytes(),
            b"",
            1_048_576,
        )
        .unwrap();
        let fingerprint = crate::progress::workspace_fingerprint(&root).unwrap();
        let context = json!({"workspace":root,"goal_revision":"goal-42","goal":"an unfinished task",
            "decision_session":{"mode":"investigation","following_development_decisions":2},
            "last_experiment":{"tool":"run","command":"wood_crafting","goal_revision":"goal-42",
                "source_sha256":fingerprint,"result":{"success":true,"exit_code":0,
                    "local_action_feedback":{"requests":[]},"inventory_delta":[],"output_files":files}},
            "last_tool_result":{"tool":"run","goal_revision":"goal-42","result":{"success":true}},
            "sdk_reference":"API reference"});
        Fixture {
            dir,
            root,
            file,
            context,
            text,
        }
    }
    fn reference_read(context: &mut Value) {
        context["last_tool_result"] = json!({"tool":"read","goal_revision":"goal-42",
            "result":{"view":"file_range","path":"sdk://reference","content":"current SDK API section",
                "start_line":62,"end_line":133}});
    }
    #[test]
    fn unchanged_compiled_source_survives_program_then_reference_reads_without_new_turns() {
        let mut f = fixture();
        let role = f.context["decision_session"].clone();
        let schema = super::super::schema();
        assert!(prepare(&mut f.context, true));
        assert_eq!(f.context[FIELD]["content"], f.text);
        assert_eq!(f.context[FIELD]["source_sha256"], digest(f.text.as_bytes()));
        reference_read(&mut f.context);
        let reference = f.context["last_tool_result"].clone();
        assert!(prepare(&mut f.context, true));
        assert_eq!(f.context[FIELD]["content"], f.text);
        assert_eq!(f.context[FIELD]["content_complete"], true);
        assert_eq!(f.context["last_tool_result"], reference);
        assert_eq!(f.context["decision_session"], role);
        assert_eq!(super::super::schema(), schema);
        assert_eq!(
            f.context["last_experiment"]["result"]["inventory_delta"],
            json!([]),
            "source availability must not turn a no-op into a gameplay failure or success"
        );
    }
    #[test]
    fn an_edit_goal_change_or_other_workspace_change_removes_a_stale_carried_view() {
        for change in ["edit", "goal", "latest_goal", "other_source"] {
            let mut f = fixture();
            assert!(prepare(&mut f.context, true));
            match change {
                "edit" => fs::write(&f.file, "fn main() { println!(\"changed\"); }").unwrap(),
                "goal" => f.context["goal_revision"] = json!("another-goal"),
                "latest_goal" => {
                    f.context["last_tool_result"]["goal_revision"] = json!("another-goal")
                }
                "other_source" => {
                    fs::write(f.root.join("src/bin/other.rs"), "fn main() {}").unwrap()
                }
                _ => unreachable!(),
            }
            assert!(!prepare(&mut f.context, true), "{change}");
            assert!(
                f.context.get(FIELD).is_none(),
                "{change} must not leave stale source attached"
            );
        }
    }
    #[test]
    fn raw_output_tampering_and_ambiguous_or_foreign_entries_are_not_source_authority() {
        for change in ["raw_hash", "raw_bytes", "ambiguous", "foreign"] {
            let mut f = fixture();
            let raw = &f.context["last_experiment"]["result"]["output_files"]["stdout"];
            let path = Path::new(raw["path"].as_str().unwrap()).to_path_buf();
            match change {
                "raw_hash" => fs::write(&path, b"tampered retained output").unwrap(),
                "raw_bytes" => {
                    f.context["last_experiment"]["result"]["output_files"]["stdout"]["bytes"] =
                        json!(1)
                }
                "ambiguous" | "foreign" => {
                    let other = if change == "ambiguous" {
                        f.root.join("src/bin/other.rs")
                    } else {
                        f.dir.path().join("foreign.rs")
                    };
                    fs::write(&other, "fn main() {}").unwrap();
                    let mut raw = fs::read(&path).unwrap();
                    raw.extend_from_slice((json!({"reason":"compiler-artifact","target":{"name":"wood_crafting","kind":["bin"],"src_path":other}}).to_string()+"\n").as_bytes());
                    fs::write(&path, &raw).unwrap();
                    f.context["last_experiment"]["result"]["output_files"]["stdout"]["sha256"] =
                        json!(digest(&raw));
                    f.context["last_experiment"]["result"]["output_files"]["stdout"]["bytes"] =
                        json!(raw.len());
                    f.context["last_experiment"]["source_sha256"] =
                        json!(crate::progress::workspace_fingerprint(&f.root).unwrap());
                }
                _ => unreachable!(),
            }
            assert!(!prepare(&mut f.context, true), "{change}");
            assert!(f.context.get(FIELD).is_none());
        }
    }
    #[test]
    fn bounded_fit_keeps_complete_source_and_latest_reference_page_under_history_pressure() {
        let mut f = fixture();
        reference_read(&mut f.context);
        let reference = f.context["last_tool_result"].clone();
        f.context["sdk_reference"] = json!(include_str!("../sdk.md"));
        f.context["recent"] = json!(vec!["old output ".repeat(3000); 10]);
        f.context["completed_tool_activity"] = json!(vec!["history ".repeat(2000); 10]);
        assert!(prepare(&mut f.context, true));
        assert_eq!(f.context[FIELD]["content"], f.text);
        assert_eq!(f.context["last_tool_result"], reference);
        assert!(serde_json::to_vec(&f.context).unwrap().len() <= 32768);
        assert!(serde_json::to_vec(&f.context[FIELD]).unwrap().len() <= VIEW_BUDGET);
    }
    #[test]
    fn optional_source_never_replaces_existing_repair_or_forces_a_budget_failure() {
        let mut f = fixture();
        assert!(prepare(&mut f.context, true));
        let source = f.context[FIELD].clone();
        f.context["compiler_repair_source"] = source.clone();
        assert!(!prepare(&mut f.context, true));
        assert!(f.context.get(FIELD).is_none());
        assert_eq!(f.context["compiler_repair_source"], source);
        f.context
            .as_object_mut()
            .unwrap()
            .remove("compiler_repair_source");
        f.context["irreducible_extra_field"] = json!("x".repeat(40000));
        let before = f.context.clone();
        assert!(!prepare(&mut f.context, true));
        assert_eq!(f.context, before);
        f.context
            .as_object_mut()
            .unwrap()
            .remove("irreducible_extra_field");
        assert!(prepare(&mut f.context, true));
        assert!(!prepare(&mut f.context, false));
        assert!(f.context.get(FIELD).is_none());
    }
}
