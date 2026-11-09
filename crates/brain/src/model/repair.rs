//! A coding transition, not a source patch or a gameplay policy.
use super::{Choice, Value};
use serde_json::json;
use std::{fs, path::Path};

pub(super) struct Transition {
    path: String,
    source_sha256: String,
    failed_workspace_sha256: String,
    source: Option<Value>,
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
        if compiler["format"] != "compiler-feedback-v1"
            || compiler["success"] != false
            || compiler["errors"].as_array().is_none_or(Vec::is_empty)
            || !compiler["source_sha256"].is_string()
            || compiler["source_sha256"] != experiment["source_sha256"]
            || !compiler["goal_revision"].is_string()
            || compiler["goal_revision"] != experiment["goal_revision"]
            || compiler["goal_revision"] != latest["goal_revision"]
        {
            return None;
        }
        let root = fs::canonicalize(context["workspace"].as_str()?).ok()?;
        let automatic = latest["tool"] == "run"
            && latest["result"]["success"] == false
            && latest["result"]["compiler_diagnostics"]["errors"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty());
        let read = if latest["tool"] == "read" {
            latest["result"].clone()
        } else if automatic {
            compiler_source(&root, compiler)?
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
        if compiler["source_sha256"] != fingerprint {
            return None;
        }
        Some(Self {
            path: relative.to_str()?.into(),
            source_sha256: hash,
            failed_workspace_sha256: fingerprint,
            source: automatic.then_some(read),
        })
    }
    pub fn prepare(&self, context: &mut Value, schema: &mut Value) {
        context["decision_session"]["mode"] = json!("verified_compiler_repair");
        context["decision_session"]["develop_available"] = json!(false);
        if let Some(source) = &self.source {
            context["compiler_repair_source"] = source.clone();
        }
        context["coding_transition"] = json!({"mode":"verified_compiler_repair",
            "path":self.path,"expected_sha256":self.source_sha256,
            "failed_workspace_sha256":self.failed_workspace_sha256,
            "source_view":if self.source.is_some(){"compiler_repair_source"}else{"last_tool_result.result"},
            "task":"The complete unchanged failed Rust source and primary compiler errors are already supplied. Return an actual edit with exact old/new fragments, or report the specific missing fact/interface that prevents a repair. Do not reread the same source/reference. The model authors all replacement code. After editing, compile/run the changed version; compilation and gameplay remain unproved.",
            "scope":"Container-local source verification and coding workflow only. No gameplay mutation, allowance refill or inference-host execution."});
        schema["properties"]["tool"]["enum"] = json!(["edit", "report"]);
        schema["properties"]["path"]["enum"] = json!([self.path, ""]);
        schema["properties"]["input"]["maxLength"] = json!(9000);
    }
    pub fn guard(&self, choice: &mut Choice) -> crate::Result<()> {
        crate::ensure(
            matches!(choice.tool.as_str(), "edit" | "report"),
            "Verified compiler repair requires an edit or a concrete blocked report",
        )?;
        if choice.tool == "edit" {
            crate::ensure(
                choice.path == self.path,
                "Compiler repair destination differs from verified source",
            )?;
            let mut edit: Value = serde_json::from_str(&choice.input)?;
            crate::ensure(edit.is_object(), "Compiler repair requires an edit object")?;
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
