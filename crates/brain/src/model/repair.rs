//! A coding transition, not a source patch or a gameplay policy.
use super::{Choice, Value};
use serde_json::json;
use std::{fs, path::Path};

pub(super) struct Transition {
    gameplay: Option<Value>,
    path: String,
    source_sha256: String,
    failed_workspace_sha256: String,
    source: Option<Value>,
}
/// Definite unsuccessful intended controls, rather than an exit-code heuristic.
fn gameplay_failure(experiment: &Value) -> Option<Value> {
    if !matches!(experiment["tool"].as_str(), Some("run" | "terminal")) {
        return None;
    }
    let trace = &experiment["result"]["local_action_feedback"];
    if trace["omitted_records"] != 0 {
        return None;
    }
    let rows = trace["requests"].as_array()?;
    if trace["through_sequence"]
        .as_u64()?
        .checked_sub(trace["from_sequence"].as_u64()?)?
        != rows.len() as u64
    {
        return None;
    }
    let mut mutations = Vec::new();
    for row in rows {
        let request: minecraft_sdk::ActionRequest =
            serde_json::from_value(row["request"].clone()).ok()?;
        let response = &row["response"];
        let outcome =
            serde_json::from_value::<minecraft_sdk::Outcome>(response["outcome"].clone()).ok();
        let refused = response["outcome"].is_null()
            && matches!(
                response["error"]["code"].as_str(),
                Some("conflict" | "invalid_request" | "unsupported_capability")
            );
        if !(outcome.is_some_and(|o| o.terminal()) || refused)
            || !row["status_refresh_error"].is_null()
        {
            return None;
        }
        if matches!(
            request.action,
            minecraft_sdk::Action::Look { .. }
                | minecraft_sdk::Action::Chat { .. }
                | minecraft_sdk::Action::CancelControl { .. }
                | minecraft_sdk::Action::CancelUseItem { .. }
        ) {
            continue;
        }
        mutations.push(row);
    }
    if mutations.is_empty() {
        return None;
    }
    let failed = mutations.iter().any(|row| {
        matches!(
            row["response"]["outcome"].as_str(),
            Some("rejected" | "expired")
        ) || (row["response"]["outcome"].is_null() && !row["response"]["error"].is_null())
    });
    let historical = mutations.iter().all(|row| {
        row["response"]["gateway_delivery"]["action_id_already_recorded"] == true
            && row["response"]["gateway_delivery"]["new_intention_dispatched"] == false
    });
    if !failed && !historical {
        return None;
    }
    Some(
        json!({"format":"gameplay-repair-evidence-v1","definite_failure":failed,"all_mutations_historical":historical,
        "mutation_requests":mutations.len(),"requests":mutations.iter().take(4).map(|row| json!({
            "action_id":row["request"]["action_id"],"kind":row["request"]["action"]["kind"],
            "outcome":row["response"]["outcome"],"error":row["response"]["error"],"gateway_delivery":row["response"]["gateway_delivery"]})).collect::<Vec<_>>(),
        "scope":"Client diagnostic evidence, not authoritative effects. Process exit zero does not establish gameplay success. Pending/uncertain mutations must be inspected or reconciled, never replayed."}),
    )
}
fn run_source(root: &Path, experiment: &Value) -> Option<Value> {
    let bin = experiment["command"].as_str()?;
    if experiment["tool"] != "terminal"
        && (bin.is_empty()
            || bin.len() > 128
            || !bin
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
    {
        return None;
    }
    // Recover Cargo's actual compiled entry from a hash-bound retained stream.
    // Never guess src/main.rs or execute Cargo/build scripts to find the target.
    let output = &experiment["result"]["output_files"]["stdout"];
    let path = Path::new(output["path"].as_str()?);
    if path.parent()?.file_name()?.to_str()? != "terminal-output"
        || !path.file_name()?.to_str()?.ends_with(".stdout.txt")
    {
        return None;
    }
    let bytes = crate::private_read(path, crate::output::STREAM_LIMIT as u64).ok()?;
    if output["sha256"] != crate::digest(&bytes) || output["bytes"] != bytes.len() {
        return None;
    }
    let mut source = None;
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-artifact"
            || (experiment["tool"] == "run" && value["target"]["name"] != bin)
            || !value["target"]["kind"]
                .as_array()?
                .iter()
                .any(|v| v == "bin")
        {
            continue;
        }
        let file = fs::canonicalize(value["target"]["src_path"].as_str()?).ok()?;
        if !file.starts_with(root) || file.extension()?.to_str()? != "rs" {
            return None;
        }
        if source.as_ref().is_some_and(|old| old != &file) {
            return None;
        }
        source = Some(file);
    }
    let file = source?;
    let bytes = crate::private_read(&file, 4096).ok().or_else(|| {
        // Public Rust source need not be mode0600; keep the same strict byte cap.
        use std::io::Read;
        let mut bytes = Vec::new();
        fs::File::open(&file)
            .ok()?
            .take(4097)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= 4096).then_some(bytes)
    })?;
    Some(
        json!({"view":"file","path":file,"source_sha256":crate::digest(&bytes),"source_bytes":bytes.len(),
        "content_complete":true,"content":std::str::from_utf8(&bytes).ok()?}),
    )
}
fn compiler_source(root: &Path, compiler: &Value) -> Option<Value> {
    use std::io::Read;
    for error in compiler["errors"].as_array()?.iter().take(4) {
        for line in error["rendered"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .take(12)
        {
            let Some(location) = line.trim().strip_prefix("--> ") else {
                continue;
            };
            let Some((prefix, column)) = location.rsplit_once(':') else {
                continue;
            };
            let Some((path, line)) = prefix.rsplit_once(':') else {
                continue;
            };
            if column.parse::<u32>().ok().is_none_or(|n| n == 0)
                || line.parse::<u32>().ok().is_none_or(|n| n == 0)
            {
                continue;
            }
            let path = Path::new(path);
            let file = fs::canonicalize(if path.is_absolute() {
                path.to_owned()
            } else {
                root.join(path)
            })
            .ok()?;
            let relative = file.strip_prefix(root).ok()?;
            if relative.extension()? != "rs" {
                return None;
            }
            let mut bytes = Vec::new();
            fs::File::open(&file)
                .ok()?
                .take(4097)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() > 4096 {
                return None;
            }
            let source = std::str::from_utf8(&bytes).ok()?;
            return Some(
                json!({"view":"file","path":file,"source_sha256":crate::digest(&bytes),"source_bytes":bytes.len(),"content_complete":true,"content":source}),
            );
        }
    }
    None
}
impl Transition {
    /// Incomplete, changed or foreign source keeps the ordinary tool interface.
    /// Files are read by the container-local worker, never the inference host.
    pub fn from_context(context: &Value) -> Option<Self> {
        let compiler = &context["compiler_feedback"];
        let experiment = &context["last_experiment"];
        let latest = &context["last_tool_result"];
        let compiler_failed = compiler["format"] == "compiler-feedback-v1"
            && compiler["success"] == false
            && compiler["errors"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
            && compiler["source_sha256"].is_string()
            && compiler["source_sha256"] == experiment["source_sha256"]
            && compiler["goal_revision"] == experiment["goal_revision"];
        let gameplay = (!compiler_failed)
            .then(|| gameplay_failure(experiment))
            .flatten();
        if (!compiler_failed && gameplay.is_none())
            || !experiment["source_sha256"].is_string()
            || experiment["goal_revision"]
                .as_str()
                .is_none_or(str::is_empty)
            || experiment["goal_revision"] != latest["goal_revision"]
        {
            return None;
        }
        let root = fs::canonicalize(context["workspace"].as_str()?).ok()?;
        let explicit_source = latest["tool"] == "read"
            && matches!(
                latest["result"]["view"].as_str(),
                Some("file" | "file_range")
            );
        let automatic = if gameplay.is_some() {
            !explicit_source && latest["tool"] != "report"
        } else {
            latest["tool"] == "run"
                && (latest["result"]["success"] == false
                    && latest["result"]["compiler_diagnostics"]["errors"]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty()))
        };
        let read = if explicit_source || (gameplay.is_none() && latest["tool"] == "read") {
            latest["result"].clone()
        } else if automatic {
            if gameplay.is_some() {
                if latest["tool"] == "run"
                    && latest["result"]["local_action_feedback"]
                        != experiment["result"]["local_action_feedback"]
                {
                    return None;
                }
                run_source(&root, experiment)?
            } else {
                compiler_source(&root, compiler)?
            }
        } else {
            return None;
        };
        let complete = match read["view"].as_str()? {
            "file" => read["content_complete"] == true,
            "file_range" => {
                read["start_line"] == 1
                    && read["end_line"] == read["total_lines"]
                    && read["range_complete"] == true
                    && read["next_line"].is_null()
            }
            _ => false,
        };
        if !complete {
            return None;
        }
        let requested = Path::new(read["path"].as_str()?);
        let file = fs::canonicalize(if requested.is_absolute() {
            requested.to_owned()
        } else {
            root.join(requested)
        })
        .ok()?;
        let relative = file.strip_prefix(&root).ok()?;
        if relative.extension()? != "rs" || relative.as_os_str().len() > 256 {
            return None;
        }
        if !fs::metadata(&file).ok()?.is_file() || fs::metadata(&file).ok()?.len() > 65536 {
            return None;
        }
        let bytes = fs::read(&file).ok()?;
        let hash = crate::digest(&bytes);
        if read["source_sha256"] != hash || read["source_bytes"] != bytes.len() {
            return None;
        }
        let text = std::str::from_utf8(&bytes).ok()?;
        if text.trim_end_matches('\n') != read["content"].as_str()?.trim_end_matches('\n') {
            return None;
        }
        // The engine's fingerprint excludes build output. When state is nested
        // in the project and cannot be reconstructed here, a mismatch safely
        // leaves ordinary tools available rather than forcing a stale repair.
        let fingerprint = crate::progress::workspace_fingerprint(&root).ok()?;
        if experiment["source_sha256"] != fingerprint {
            return None;
        }
        Some(Self {
            gameplay,
            path: relative.to_str()?.into(),
            source_sha256: hash,
            failed_workspace_sha256: fingerprint,
            source: automatic.then_some(read),
        })
    }
    pub fn prepare(&self, context: &mut Value, schema: &mut Value) {
        let mode = if self.gameplay.is_some() {
            "verified_gameplay_repair"
        } else {
            "verified_compiler_repair"
        };
        let source_view = if self.gameplay.is_some() {
            "gameplay_repair_source"
        } else {
            "compiler_repair_source"
        };
        context["decision_session"]["mode"] = json!(mode);
        context["decision_session"]["develop_available"] = json!(false);
        if let Some(source) = &self.source {
            context[source_view] = source.clone();
        }
        context["coding_transition"] = json!({"mode":mode,
            "gameplay_failure":self.gameplay,
            "path":self.path,"expected_sha256":self.source_sha256,
            "failed_workspace_sha256":self.failed_workspace_sha256,
            "source_view":if self.source.is_some(){source_view}else{"last_tool_result.result"},
            "task":"The complete unchanged failed Rust source and primary compiler errors are already supplied. Return an actual edit with exact old/new fragments, or report the specific missing fact/interface that prevents a repair. Do not reread the same source/reference. The model authors all replacement code. After editing, compile/run the changed version; compilation and gameplay remain unproved.",
            "scope":"Container-local source verification and coding workflow only. No gameplay mutation, allowance refill or inference-host execution."});
        if self.gameplay.is_some() {
            context["coding_transition"]["task"] = json!(
                "The complete unchanged compiled Rust entry and concrete failed/historical gameplay outcomes are supplied. Repair the cause with an exact old/new source edit, or report the precise missing fact/interface. Diagnose before trying another intention: merely changing IDs cannot fix an invalid route or expired interaction. Gather fresh state in the repaired program, preserve pending/uncertain IDs and reconcile rather than blindly replaying. Read-only status/history runs are valid and require no mutation. The model authors all code; compile and execute a changed version, then verify actual retained progress. Do not reread the same source or rerun the unchanged failed program."
            );
        }
        schema["properties"]["tool"]["enum"] = json!(["edit", "report"]);
        schema["properties"]["path"]["enum"] = json!([self.path, ""]);
        schema["properties"]["input"]["maxLength"] = json!(9000);
    }
    pub fn guard(&self, choice: &mut Choice) -> crate::Result<()> {
        crate::ensure(
            matches!(choice.tool.as_str(), "edit" | "report"),
            "Verified coding repair requires an edit or a concrete blocked report",
        )?;
        if choice.tool == "edit" {
            crate::ensure(
                choice.path == self.path,
                "Coding repair destination differs from verified source",
            )?;
            let mut edit: Value = serde_json::from_str(&choice.input)?;
            crate::ensure(edit.is_object(), "Coding repair requires an edit object")?;
            if edit["expected_sha256"].is_null() {
                edit["expected_sha256"] = json!(self.source_sha256);
            }
            choice.input = serde_json::to_string(&edit)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn gameplay_fixture() -> (tempfile::TempDir, Value) {
        let (dir, mut context) = fixture();
        let log = json!({"reason":"compiler-artifact","target":{"name":"candidate","kind":["bin"],"src_path":dir.path().join("src/main.rs")}}).to_string();
        let output = crate::output::save(
            &dir.path().join("target/terminal-output"),
            "test-1",
            log.as_bytes(),
            b"",
            1024 * 1024,
        )
        .unwrap();
        let result = json!({"success":true,"exit_code":0,"experiment":true,"inventory_changed":false,
            "output_files":output,"local_action_feedback":{"from_sequence":0,"through_sequence":1,"omitted_records":0,"requests":[{
                "request":{"action_id":"old-finish","action":{"kind":"dig","block":{"x":1,"y":2,"z":3},"face":1,"stage":"finish"}},
                "response":{"outcome":"expired","native_effect_verified":false,"gateway_delivery":{"action_id_already_recorded":true,"new_intention_dispatched":false}}}]}});
        context["compiler_feedback"] = Value::Null;
        context["last_experiment"]["tool"] = json!("run");
        context["last_experiment"]["command"] = json!("candidate");
        context["last_experiment"]["result"] = result.clone();
        context["last_tool_result"] = json!({"tool":"run","goal_revision":"goal","result":result});
        (dir, context)
    }
    #[test]
    fn successful_process_with_expired_gameplay_selects_actual_compiled_source_for_repair() {
        let (_dir, mut context) = gameplay_fixture();
        let transition = Transition::from_context(&context).unwrap();
        let mut schema = super::super::schema();
        transition.prepare(&mut context, &mut schema);
        assert_eq!(
            context["coding_transition"]["mode"],
            "verified_gameplay_repair"
        );
        assert_eq!(
            context["coding_transition"]["gameplay_failure"]["definite_failure"],
            true
        );
        assert_eq!(
            context["gameplay_repair_source"]["content"],
            "fn main() { missing(); }\n"
        );
        assert_eq!(
            schema["properties"]["tool"]["enum"],
            json!(["edit", "report"])
        );
        crate::fit_context(&mut context).unwrap();
        assert_eq!(context["gameplay_repair_source"]["content_complete"], true);
    }
    #[test]
    fn historical_observed_mutations_are_not_new_work_but_successful_fresh_work_is_allowed() {
        let (_dir, mut context) = gameplay_fixture();
        context["last_experiment"]["result"]["local_action_feedback"]["requests"][0]["response"]
            ["outcome"] = json!("observed");
        context["last_tool_result"]["result"] = context["last_experiment"]["result"].clone();
        assert!(Transition::from_context(&context).is_some());
        context["last_experiment"]["result"]["local_action_feedback"]["requests"][0]["response"]
            ["gateway_delivery"] = Value::Null;
        context["last_tool_result"]["result"] = context["last_experiment"]["result"].clone();
        assert!(Transition::from_context(&context).is_none());
    }
    #[test]
    fn uncertain_incomplete_and_read_only_runs_do_not_force_another_mutation() {
        for mode in [
            "uncertain",
            "running",
            "unknown",
            "omitted",
            "count",
            "read-only",
            "chat",
        ] {
            let (_dir, mut context) = gameplay_fixture();
            let trace = &mut context["last_experiment"]["result"]["local_action_feedback"];
            match mode {
                "omitted" => trace["omitted_records"] = json!(1),
                "count" => trace["through_sequence"] = json!(2),
                "read-only" => trace["requests"] = json!([]),
                "chat" => {
                    trace["requests"][0]["request"]["action"] =
                        json!({"kind":"chat","message":"hello"})
                }
                _ => trace["requests"][0]["response"]["outcome"] = json!(mode),
            }
            context["last_tool_result"]["result"] = context["last_experiment"]["result"].clone();
            assert!(Transition::from_context(&context).is_none(), "{mode}");
        }
    }
    #[test]
    fn gameplay_repair_refuses_changed_source_goal_or_log_and_accepts_a_complete_bound_read() {
        let (dir, context) = gameplay_fixture();
        for pointer in [
            "/last_tool_result/goal_revision",
            "/last_experiment/result/output_files/stdout/sha256",
            "/last_experiment/command",
        ] {
            let mut bad = context.clone();
            *bad.pointer_mut(pointer).unwrap() = json!("other");
            assert!(Transition::from_context(&bad).is_none(), "{pointer}");
        }
        let mut read = context.clone();
        read["last_tool_result"] = json!({"tool":"read","goal_revision":"goal","result":crate::workspace::read(dir.path(),"src/main.rs").unwrap()});
        assert!(Transition::from_context(&read).is_some());
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        assert!(Transition::from_context(&context).is_none());
        assert!(Transition::from_context(&read).is_none());
    }
    #[test]
    fn a_cargo_artifact_cannot_select_foreign_source_for_gameplay_repair() {
        let (dir, mut context) = gameplay_fixture();
        let foreign = tempfile::tempdir().unwrap();
        let path = foreign.path().join("foreign.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        let stdout = json!({"reason":"compiler-artifact","target":{"name":"candidate","kind":["bin"],"src_path":path}}).to_string();
        context["last_experiment"]["result"]["output_files"] = crate::output::save(
            &dir.path().join("target/terminal-output"),
            "foreign-2",
            stdout.as_bytes(),
            b"",
            1024 * 1024,
        )
        .unwrap();
        context["last_tool_result"]["result"] = context["last_experiment"]["result"].clone();
        assert!(Transition::from_context(&context).is_none());
    }
    #[test]
    fn a_later_sdk_inspection_does_not_hide_a_failed_unchanged_program() {
        let (_dir, mut context) = gameplay_fixture();
        context["last_tool_result"] = json!({"tool":"read","goal_revision":"goal","result":{"view":"reference","path":"sdk://reference"}});
        assert!(Transition::from_context(&context).is_some());
        context["last_tool_result"]["tool"] = json!("query");
        assert!(Transition::from_context(&context).is_some());
        context["last_tool_result"]["tool"] = json!("report");
        assert!(Transition::from_context(&context).is_none());
    }
    #[test]
    fn conflicting_intent_is_repairable_but_transport_uncertainty_is_not() {
        let (_dir, mut context) = gameplay_fixture();
        for code in ["conflict", "invalid_request", "internal"] {
            context["last_experiment"]["result"]["local_action_feedback"]["requests"][0]["response"] =
                json!({"error":{"code":code,"message":"test"}});
            context["last_tool_result"]["result"] = context["last_experiment"]["result"].clone();
            assert_eq!(
                Transition::from_context(&context).is_some(),
                code != "internal"
            );
        }
    }
    #[test]
    fn terminal_gameplay_uses_the_unique_compiled_entry_without_parsing_shell_commands() {
        let (_dir, mut context) = gameplay_fixture();
        context["last_experiment"]["tool"] = json!("terminal");
        context["last_experiment"]["command"] =
            json!("CARGO_BUILD_JOBS=1 cargo run --offline --bin candidate");
        context["last_tool_result"]["tool"] = json!("terminal");
        assert!(Transition::from_context(&context).is_some());
    }
    fn fixture() -> (tempfile::TempDir, Value) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() { missing(); }\n").unwrap();
        let fingerprint = crate::progress::workspace_fingerprint(dir.path()).unwrap();
        let read = crate::workspace::read(dir.path(), "src/main.rs").unwrap();
        (
            dir,
            json!({"workspace":read["workspace"],"compiler_feedback":{"format":"compiler-feedback-v1","success":false,"errors":[{"message":"unknown function"}],"source_sha256":fingerprint,"goal_revision":"goal"},"last_experiment":{"source_sha256":fingerprint,"goal_revision":"goal"},"last_tool_result":{"tool":"read","goal_revision":"goal","result":read}}),
        )
    }
    #[test]
    fn complete_unchanged_failed_source_selects_a_guarded_edit_not_another_inspection() {
        let (_dir, mut context) = fixture();
        let transition = Transition::from_context(&context).unwrap();
        let mut schema = super::super::schema();
        transition.prepare(&mut context, &mut schema);
        assert_eq!(
            schema["properties"]["tool"]["enum"],
            json!(["edit", "report"])
        );
        let mut choice = Choice {
            tool: "edit".into(),
            input: json!({"old":"missing();","new":"","expected_sha256":null}).to_string(),
            path: "src/main.rs".into(),
            summary: "repair".into(),
            notes: String::new(),
            experiment: false,
        };
        transition.guard(&mut choice).unwrap();
        let edit: Value = serde_json::from_str(&choice.input).unwrap();
        assert_eq!(edit["expected_sha256"], transition.source_sha256);
        choice.path = "another.rs".into();
        assert!(transition.guard(&mut choice).is_err());
        choice.tool = "read".into();
        assert!(transition.guard(&mut choice).is_err());
    }
    #[test]
    fn changed_partial_or_mismatched_failure_context_does_not_force_a_patch() {
        let (dir, context) = fixture();
        for (path, value) in [
            ("/compiler_feedback/source_sha256", json!("other")),
            ("/compiler_feedback/goal_revision", json!("other")),
            ("/last_tool_result/goal_revision", json!("other")),
            ("/last_tool_result/result/content_complete", json!(false)),
            ("/last_tool_result/result/source_sha256", json!("other")),
            ("/last_tool_result/result/content", json!("incomplete")),
        ] {
            let mut bad = context.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(Transition::from_context(&bad).is_none(), "{path}");
        }
        fs::write(dir.path().join("Cargo.toml"), "changed project").unwrap();
        assert!(Transition::from_context(&context).is_none());
    }
    #[test]
    fn a_reference_or_file_outside_the_workspace_is_not_a_repair_target() {
        let (dir, mut context) = fixture();
        context["last_tool_result"]["result"]["path"] = json!("sdk://reference");
        assert!(Transition::from_context(&context).is_none());
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("foreign.rs"), "fn main() {}\n").unwrap();
        context["last_tool_result"]["result"]["path"] = json!(other.path().join("foreign.rs"));
        assert!(Transition::from_context(&context).is_none());
        std::os::unix::fs::symlink(
            other.path().join("foreign.rs"),
            dir.path().join("src/linked.rs"),
        )
        .unwrap();
        context["last_tool_result"]["result"]["path"] = json!(dir.path().join("src/linked.rs"));
        assert!(Transition::from_context(&context).is_none());
    }
    #[tokio::test]
    async fn actual_model_request_uses_repair_schema_and_preserves_luna_allocation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (_dir, mut context) = fixture();
        context["recent"] = json!(vec!["history".repeat(4000); 12]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
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
                        .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut bytes = vec![0; size];
            socket.read_exact(&mut bytes).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["model"], "gpt-6-luna");
            assert_eq!(body["purpose"], "develop");
            assert_eq!(
                body["format"]["properties"]["tool"]["enum"],
                json!(["edit", "report"])
            );
            let supplied = body["messages"][1]["content"].as_str().unwrap();
            assert!(supplied.len() <= 32768);
            let supplied: Value = serde_json::from_str(supplied).unwrap();
            assert_eq!(
                supplied["coding_transition"]["mode"],
                "verified_compiler_repair"
            );
            let choice = json!({"tool":"edit","input":json!({"old":"missing();","new":"","expected_sha256":null}).to_string(),"path":"src/main.rs","summary":"repair","notes":"","experiment":false});
            let response=json!({"done":true,"reasoning_effort":"high","message":{"content":choice.to_string()}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
        });
        let model = super::super::Model {
            http: reqwest::Client::new(),
            endpoint,
        };
        let (choice, usage) = model.choose_for(&context, false, false).await.unwrap();
        assert_eq!(usage["model"], "gpt-6-luna");
        assert_eq!(usage["purpose"], "develop");
        assert_eq!(usage["provider_reasoning_effort"], "high");
        assert_eq!(
            usage["coding_transition"]["mode"],
            "verified_compiler_repair"
        );
        let edit: Value = serde_json::from_str(&choice.input).unwrap();
        assert!(
            edit["expected_sha256"]
                .as_str()
                .is_some_and(|s| s.len() == 64)
        );
        server.await.unwrap();
    }
    #[test]
    fn a_failed_compile_supplies_current_source_without_another_model_read() {
        let (_dir, mut context) = fixture();
        context["compiler_feedback"]["errors"][0]["rendered"] =
            json!("error: missing function\n --> src/main.rs:1:12\n");
        context["last_tool_result"] = json!({"tool":"run","goal_revision":"goal","result":{"success":false,"compiler_diagnostics":{"errors":[{"message":"missing function"}]}}});
        let transition = Transition::from_context(&context).unwrap();
        let mut schema = super::super::schema();
        transition.prepare(&mut context, &mut schema);
        assert_eq!(
            context["compiler_repair_source"]["content"],
            "fn main() { missing(); }\n"
        );
        assert_eq!(
            context["coding_transition"]["source_view"],
            "compiler_repair_source"
        );
        context["last_tool_result"]["result"]["success"] = json!(true);
        assert!(Transition::from_context(&context).is_none());
        context["last_tool_result"]["result"]["success"] = json!(false);
        context["last_tool_result"]["result"]["compiler_diagnostics"]["errors"] = json!([]);
        assert!(
            Transition::from_context(&context).is_none(),
            "runtime failures are not compiler repairs"
        );
    }
}
