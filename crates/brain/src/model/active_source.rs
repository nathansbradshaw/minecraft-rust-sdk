//! Carry one verified client source across reference reads without extra turns.
use serde_json::{Value, json};
use std::{fs, path::Path};

const FIELD: &str = "active_program_source";
const SOURCE_BUDGET: usize = 8192;
const VIEW_BUDGET: usize = 12000;

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
    let scope = &context["workspace_source_scope"];
    if scope["format"] != "workspace-excluding-state-v1" {
        return None;
    }
    let state = fs::canonicalize(scope["excluded_state"].as_str()?).ok()?;
    let is_lua = experiment["tool"] == "run" && experiment["result"]["language"] == "lua";
    if !is_lua {
        let output = Path::new(experiment["result"]["output_files"]["stdout"]["path"].as_str()?);
        if output.parent()?.file_name()?.to_str()? != "terminal-output"
            || fs::canonicalize(output.parent()?.parent()?).ok()? != state
        {
            return None;
        }
    }
    // Use exactly the engine's exclusion scope. Output/journal churn within a
    // nested state directory is not a source edit; a real source edit still is.
    let fingerprint = crate::progress::workspace_fingerprint_excluding_state(&root, &state).ok()?;
    if experiment["source_sha256"] != fingerprint {
        return None;
    }
    // Reuse the single Cargo-entry resolver and its private raw stdout hash,
    // exact binary/unique-entry and workspace containment guards. Existing
    // failure repairs retain their4096-byte default; this optional view is8KiB.
    let mut view = super::repair::run_source_bounded(&root, experiment, SOURCE_BUDGET)?;
    let entry = Path::new(view["path"].as_str()?);
    if state.starts_with(&root) && entry.starts_with(&state) {
        return None;
    }
    view["format"] = json!("active-program-source-v1");
    view["goal_revision"] = json!(revision);
    view["workspace_sha256"] = json!(fingerprint);
    view["compiled_entry_stdout"] = experiment["result"]["output_files"]["stdout"].clone();
    view["scope"] = json!(
        "Complete current client source identified by a hash-bound retained Cargo stream and unchanged workspace/goal. Available across reference reads during this coding session. This supplies editable source, not a compile, gameplay or completion claim, and grants no model turns."
    );
    if is_lua {
        view["scope"] = json!(
            "Complete current Lua source hash-bound to a recorded run and unchanged workspace/goal. An invalid update may have been rejected; this is editable source, not a gameplay or activation claim."
        );
    }
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
            && context[*key]["path"] == view["path"]
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

    #[test]
    fn lua_source_is_retained_only_for_the_recorded_current_hash_and_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("program");
        let state = dir.path().join("state");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&state).unwrap();
        let text = "return {version=1,step=function(ctx) return {state=ctx.state,done=true} end}";
        fs::write(root.join("behavior.lua"), text).unwrap();
        let fingerprint =
            crate::progress::workspace_fingerprint_excluding_state(&root, &state).unwrap();
        let mut context = json!({"workspace":root,"workspace_source_scope":{"format":"workspace-excluding-state-v1","excluded_state":state},"goal_revision":"goal","last_tool_result":{"goal_revision":"goal"},"last_experiment":{"tool":"run","command":"behavior.lua","goal_revision":"goal","source_sha256":fingerprint,"result":{"language":"lua","lua_source":{"source_sha256":digest(text.as_bytes())}}}});
        assert_eq!(source(&context).unwrap()["content"], text);
        context["last_experiment"]["result"]["lua_source"]["source_sha256"] = json!("stale");
        assert!(source(&context).is_none());
        context["last_experiment"]["result"]["lua_source"]["source_sha256"] =
            json!(digest(text.as_bytes()));
        context["last_experiment"]["command"] = json!("../program/behavior.lua");
        assert!(source(&context).is_none());
    }

    struct Fixture {
        dir: tempfile::TempDir,
        root: std::path::PathBuf,
        file: std::path::PathBuf,
        state: std::path::PathBuf,
        context: Value,
        text: String,
    }
    fn fixture() -> Fixture {
        fixture_with(2537, false)
    }
    fn fixture_with(source_bytes: usize, nested: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("program");
        fs::create_dir_all(root.join("src/bin")).unwrap();
        let file = root.join("src/bin/wood_crafting.rs");
        let prefix = "fn main() { println!(\"bounded fixture; goal remains incomplete\"); }\n// ";
        let text = format!("{prefix}{}\n", "c".repeat(source_bytes - prefix.len() - 1));
        fs::write(&file, &text).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"fixture\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        let raw=json!({"reason":"compiler-artifact","target":{"name":"wood_crafting","kind":["bin"],"src_path":file}}).to_string()+"\n";
        let state = if nested {
            root.join(".state")
        } else {
            dir.path().join("state")
        };
        let files = crate::output::save(
            &state.join("terminal-output"),
            "run-1",
            raw.as_bytes(),
            b"",
            1_048_576,
        )
        .unwrap();
        let fingerprint =
            crate::progress::workspace_fingerprint_excluding_state(&root, &state).unwrap();
        let context = json!({"workspace":root,"goal_revision":"goal-42","goal":"an unfinished task",
            "workspace_source_scope":{"format":"workspace-excluding-state-v1","excluded_state":state},
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
            state,
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
        let mut f = fixture_with(6521, false);
        reference_read(&mut f.context);
        let reference = f.context["last_tool_result"].clone();
        f.context["sdk_reference"] = json!(include_str!("../sdk.md"));
        f.context["recent"] = json!(vec!["old output ".repeat(3000); 10]);
        f.context["completed_tool_activity"] = json!(vec!["history ".repeat(2000); 10]);
        assert!(prepare(&mut f.context, true));
        assert_eq!(f.context[FIELD]["content"], f.text);
        assert_eq!(f.context[FIELD]["source_bytes"], 6521);
        assert!(
            super::super::repair::run_source(
                &fs::canonicalize(&f.root).unwrap(),
                &f.context["last_experiment"]
            )
            .is_none(),
            "existing failure repair remains capped at4KiB"
        );
        assert_eq!(f.context["last_tool_result"], reference);
        assert!(serde_json::to_vec(&f.context).unwrap().len() <= 32768);
        assert!(serde_json::to_vec(&f.context[FIELD]).unwrap().len() <= VIEW_BUDGET);
    }
    #[test]
    fn nested_state_churn_preserves_source_but_an_edit_still_invalidates_it() {
        let mut f = fixture_with(6521, true);
        reference_read(&mut f.context);
        assert!(prepare(&mut f.context, true));
        let current = f.context[FIELD].clone();
        fs::write(f.state.join("brain.sqlite"), b"fixture journal grew").unwrap();
        crate::output::save(
            &f.state.join("terminal-output"),
            "another-read",
            b"reference output",
            b"",
            1_048_576,
        )
        .unwrap();
        assert!(prepare(&mut f.context, true));
        assert_eq!(
            f.context[FIELD], current,
            "state churn is excluded exactly as it is in the engine"
        );
        fs::write(&f.file, "fn main() { println!(\"real source edit\"); }").unwrap();
        assert!(!prepare(&mut f.context, true));
        assert!(f.context.get(FIELD).is_none());
    }
    #[test]
    fn missing_or_foreign_state_scopes_do_not_hide_workspace_changes() {
        for change in ["missing", "format", "capture_mismatch"] {
            let mut f = fixture();
            assert!(prepare(&mut f.context, true));
            match change {
                "missing" => {
                    f.context
                        .as_object_mut()
                        .unwrap()
                        .remove("workspace_source_scope");
                }
                "format" => f.context["workspace_source_scope"]["format"] = json!("unknown-scope"),
                "capture_mismatch" => {
                    f.context["workspace_source_scope"]["excluded_state"] = json!(f.root)
                }
                _ => unreachable!(),
            }
            assert!(!prepare(&mut f.context, true), "{change}");
            assert!(f.context.get(FIELD).is_none());
        }
    }
    #[test]
    fn active_source_byte_and_serialized_budgets_remain_hard_bounds() {
        let mut f = fixture_with(8193, false);
        assert!(!prepare(&mut f.context, true));
        let text = format!("// {}", "\"".repeat(7500));
        fs::write(&f.file, text).unwrap();
        f.context["last_experiment"]["source_sha256"] = json!(
            crate::progress::workspace_fingerprint_excluding_state(&f.root, &f.state).unwrap()
        );
        assert!(
            !prepare(&mut f.context, true),
            "valid8KiBsource may still exceed its serializedview budget"
        );
        assert!(f.context.get(FIELD).is_none());
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
        let mut another_entry = f.context.clone();
        another_entry["compiler_repair_source"]["path"] = json!(f.root.join("src/bin/another.rs"));
        assert!(
            prepare(&mut another_entry, true),
            "identical helper bytes must not hide the active entry's distinct path"
        );
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
