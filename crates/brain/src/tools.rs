use crate::{Brain, Result, ensure, model::Choice};
use minecraft_sdk::ObservationQuery;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

fn writable(root: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    ensure(
        !relative.is_absolute()
            && !relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Write inside the assigned workspace",
    )?;
    let path = root.join(relative);
    let mut ancestor = path.parent().ok_or("File parent required")?;
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or("Parent required")?;
    }
    ensure(
        ancestor.canonicalize()?.starts_with(root.canonicalize()?),
        "Symlink leaves assigned workspace",
    )?;
    ensure(
        !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "Do not overwrite a symlink",
    )?;
    Ok(path)
}
fn edit(root: &Path, path: &str, input: &str) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Edit {
        old: String,
        new: String,
        expected_sha256: Option<String>,
    }
    ensure(
        !path.is_empty() && input.len() <= 10000,
        "Bounded exact edit and workspace path required",
    )?;
    let change: Edit = serde_json::from_str(input)?;
    ensure(
        !change.old.is_empty() && change.old != change.new,
        "Edit needs a nonempty old fragment and a changed replacement",
    )?;
    let file = writable(root, path)?;
    ensure(
        fs::metadata(&file)?.is_file() && fs::metadata(&file)?.len() <= 65536,
        "Edit an existing regular source file up to65536 bytes",
    )?;
    let mut bytes = Vec::new();
    use std::io::Read;
    fs::File::open(&file)?.take(65537).read_to_end(&mut bytes)?;
    ensure(bytes.len() <= 65536, "Source grew beyond edit limit")?;
    let before = crate::digest(&bytes);
    ensure(
        change
            .expected_sha256
            .as_ref()
            .is_none_or(|hash| hash == &before),
        "Source hash changed; inspect the current range before editing",
    )?;
    let source = String::from_utf8(bytes)?;
    let first = source
        .find(&change.old)
        .ok_or("Old edit fragment was not found; source unchanged")?;
    ensure(
        source.rfind(&change.old) == Some(first),
        "Old fragment is ambiguous; include enough surrounding source to match exactly once",
    )?;
    let after = source.replacen(&change.old, &change.new, 1);
    ensure(after.len() <= 65536, "Edited source exceeds65536 bytes")?;
    fs::write(file, &after)?;
    Ok(
        json!({"path":path,"before":before,"sha256":crate::digest(after.as_bytes()),"bytes":after.len(),"replacements":1,"next_action":"Compile/run the changed version; an edit alone is not a successful program or gameplay outcome"}),
    )
}
async fn bounded_output(mut stream: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut total = 0usize;
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        total += n;
        ensure(
            total <= crate::output::STREAM_LIMIT,
            "Terminal output flood stopped",
        )?;
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(bytes)
}
fn local_shell(command: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("/bin/sh");
    // Preserve the image's explicit toolchain PATH. A login shell replaced it
    // with /etc/profile's PATH and made the installed Cargo disappear.
    c.args(["-c", command]);
    c
}
fn run_command(bin: &str) -> Result<String> {
    ensure(
        !bin.is_empty()
            && bin.len() <= 128
            && bin
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            && bin
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
        "Use an exact Cargo binary name (letters, digits, underscore or hyphen; no leading option)",
    )?;
    Ok(format!(
        "cargo run --offline --bin {bin} --message-format=json"
    ))
}
fn observation_query(input: &str) -> Result<minecraft_sdk::ObservationQuery> {
    ensure(
        input.len() <= 10000,
        "Bounded typed observation query required",
    )?;
    let query: minecraft_sdk::ObservationQuery = serde_json::from_str(input)?;
    query.validate()?;
    Ok(query)
}
pub(crate) struct ProcessGroup(pub(crate) u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Also clean up background descendants on timeout, cancellation or
        // ordinary shell exit; a finite tool must not leave a second controller.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
pub async fn terminal(brain: &mut Brain, command: &str) -> Result<Value> {
    terminal_with_view(brain, command, false).await
}
async fn terminal_with_view(brain: &mut Brain, command: &str, cargo_run: bool) -> Result<Value> {
    ensure(
        !command.is_empty() && command.len() <= 8192,
        "Bounded terminal command required",
    )?;
    let output_root = brain.cfg.state.join("terminal-output");
    let output_budget = u64::from(brain.cfg.terminal_output_budget_mib) * 1024 * 1024;
    crate::output::prepare(&output_root)?;
    let sequence = brain
        .journal
        .event("terminal_started", json!({"command":command}))?;
    let output_id = format!("{}-{sequence}", brain.run_id);
    let mut c = local_shell(command);
    c.current_dir(&brain.cfg.workspace)
        .env("MC_SDK_PROFILE", &brain.cfg.profile)
        .env("MC_SDK_STATE", &brain.cfg.state)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "1")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CARGO_PROFILE_DEV_STRIP", "debuginfo")
        .env(
            "CARGO_TARGET_DIR",
            brain
                .cfg
                .cargo_target
                .clone()
                .unwrap_or_else(|| brain.cfg.workspace.join("target")),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(body) = &brain.body {
        c.env("MC_SDK_ARBITER", &body.socket);
    }
    // Leave scheduling room for the independent reactive controller.
    unsafe {
        c.pre_exec(|| {
            if libc::setpriority(libc::PRIO_PROCESS, 0, 10) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    c.process_group(0);
    let initial_inventory = brain.observed.player["inventory"].clone();
    let cursor = if let Some(body) = &brain.body {
        body.shared
            .feedback
            .lock()
            .map_err(|_| "Local action diagnostic trace unavailable")?
            .cursor()
    } else {
        0
    };
    let mut child = c.spawn()?;
    let pid = child.id().ok_or("Process ID unavailable")?;
    let group = ProcessGroup(pid);
    let stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("stderr unavailable")?;
    let out = tokio::spawn(bounded_output(stdout));
    let err = tokio::spawn(bounded_output(stderr));
    let started = std::time::Instant::now();
    let deadline = tokio::time::sleep(Duration::from_secs(brain.cfg.tool_timeout_secs));
    tokio::pin!(deadline);
    let mut timed_out = false;
    let status = loop {
        tokio::select! {s=child.wait()=>break s?,_=&mut deadline=>{timed_out=true;unsafe{libc::kill(-(pid as i32),libc::SIGKILL);};let _=child.kill().await;break child.wait().await?;},_=tokio::time::sleep(Duration::from_secs(1))=>{if let Err(e)=brain.refresh().await{brain.journal.event("observation_error",json!({"error":e.to_string()}))?;}}}
    };
    drop(group);
    // Bound inherited open pipes even if a subprocess detached from its parent.
    let stdout = tokio::time::timeout(Duration::from_secs(2), out).await???;
    let stderr = tokio::time::timeout(Duration::from_secs(2), err).await???;
    let output_files = match crate::output::save(
        &output_root,
        &output_id,
        &stdout,
        &stderr,
        output_budget,
    ) {
        Ok(files) => files,
        Err(error) => {
            json!({"error":error.to_string(),"complete_capture_available":false,"command_executed":true})
        }
    };
    let captured = output_files.get("error").is_none();
    brain.refresh().await?;
    let mut feedback = if let Some(body) = &brain.body {
        body.shared
            .feedback
            .lock()
            .map(|trace| trace.since(cursor))
            .unwrap_or_else(|_| json!({"error":"Local action diagnostic trace unavailable"}))
    } else {
        json!({"scope":"local arbiter disabled; no completeness claim","requests":[]})
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    for row in feedback["requests"].as_array_mut().into_iter().flatten() {
        if matches!(
            row["response"]["outcome"].as_str(),
            Some("running" | "prepared" | "dispatched" | "uncertain")
        ) && let Some(id) = row["request"]["action_id"].as_str()
        {
            match tokio::time::timeout_at(deadline, brain.client.action_status(id)).await {
                Ok(Ok(receipt)) => {
                    let delivery = row["response"]["gateway_delivery"].clone();
                    row["response"] = crate::feedback::receipt_summary(&receipt);
                    if !delivery.is_null() {
                        row["response"]["gateway_delivery"] = delivery;
                    }
                    if let Ok(request) = serde_json::from_value(row["request"].clone()) {
                        row["container_inspection"] =
                            json!(crate::containers::inspection(&request, &receipt));
                    }
                }
                Ok(Err(error)) => row["status_refresh_error"] = json!(error),
                Err(_) => {
                    row["status_refresh_error"] =
                        json!("Status refresh budget reached; inspect preserved IDs, never replay");
                    break;
                }
            }
        }
    }
    for row in feedback["requests"].as_array().into_iter().flatten() {
        brain.journal.remember_container(
            &brain.server.instance_id,
            brain.observed.player["uuid"].as_str().unwrap_or_default(),
            &row["container_inspection"],
        )?;
    }
    let (stdout_preview, cargo_view) = if cargo_run {
        let (preview, metadata) = crate::output::cargo_run_preview(&stdout);
        (preview, Some(metadata))
    } else {
        (crate::output::preview(&stdout), None)
    };
    let mut result = json!({"exit_code":status.code(),"success":status.success()&&!timed_out&&captured,"timed_out":timed_out,"elapsed_ms":started.elapsed().as_millis(),"inventory_changed":initial_inventory!=brain.observed.player["inventory"],"inventory_delta":crate::feedback::inventory_delta(&initial_inventory,&brain.observed.player["inventory"]),"local_action_feedback":feedback,"compiler_diagnostics":crate::feedback::compiler_diagnostics(&stdout),"output_truncated":stdout.len()>2000||stderr.len()>2000,"output_files":output_files,"stdout":stdout_preview,"stderr":crate::output::preview(&stderr)});
    if let Some(view) = cargo_view {
        result["cargo_output_view"] = view;
    }
    Ok(result)
}
pub async fn execute(brain: &mut Brain, choice: &Choice) -> Result<Value> {
    match choice.tool.as_str() {
        "develop" => {
            brain.journal.set("development_request", &choice.input)?;
            let available = brain.journal.available_model(true, crate::now())?;
            Ok(
                json!({"next_reasoning_model":available.map(|develop|if develop{"gpt-6.1-sol"}else{"gpt-6-luna"}),"preferred_reasoning_model":"gpt-6.1-sol","request":choice.input,"model_availability":brain.journal.model_availability(crate::now())?}),
            )
        }
        "observe" => {
            brain.refresh().await?;
            if choice.input.trim().is_empty() {
                Ok(crate::projection(&brain.observed))
            } else {
                crate::views::focused(&brain.observed, &choice.input)
            }
        }
        "query" => {
            let query = observation_query(&choice.input)?;
            let result = brain.client.read_observation(&query).await?;
            ensure(
                result.to_string().len() <= 5000,
                "Capability response exceeds the model view budget; request a smaller page or use an SDK program for the full result",
            )?;
            let mut view = json!({"view":"capability_query","capability":query.capability,"input_sha256":crate::digest(query.input.to_string().as_bytes()),"result":result,"scope":"Read-only SDK capability result for this character; query is not a gameplay mutation or evidence of task completion"});
            if let Some(recipe) = material_recipe(&query, &result) {
                brain.refresh().await?;
                let materials = minecraft_sdk::recipes::material_summary(&brain.observed, recipe);
                view["client_materials"] = match materials {
                    Ok(summary) if summary.to_string().len() <= 3500 => summary,
                    Ok(_) => {
                        json!({"unavailable":"Material summary exceeds the model view budget; use recipes::material_summary in an SDK program for the full bounded result","craftability":"unknown"})
                    }
                    Err(error) => json!({"unavailable":error,"craftability":"unknown"}),
                };
            }
            Ok(view)
        }
        "navigate" => crate::navigation::execute(brain, &choice.input).await,
        "sdk" => brain.act(serde_json::from_str(&choice.input)?).await,
        "status" => Ok(serde_json::to_value(brain.inspect(&choice.input).await?)?),
        "reconcile" => brain.settle(&choice.input).await,
        "read" => crate::workspace::read(&brain.cfg.workspace, &choice.input),
        "edit" => edit(&brain.cfg.workspace, &choice.path, &choice.input),
        "write" => {
            ensure(
                choice.input.len() <= 32768 && !choice.path.is_empty(),
                "Bounded source edit and path required",
            )?;
            let path = writable(&brain.cfg.workspace, &choice.path)?;
            fs::create_dir_all(path.parent().ok_or("Parent required")?)?;
            let before = fs::read(&path).ok().map(|b| crate::digest(&b));
            fs::write(&path, &choice.input)?;
            Ok(
                json!({"path":choice.path,"before":before,"sha256":crate::digest(choice.input.as_bytes()),"bytes":choice.input.len()}),
            )
        }
        "list" => crate::workspace::list(&brain.cfg.workspace, &choice.input),
        "search" => Ok(
            json!({"memory":brain.journal.recall(&choice.input)?,"history":brain.journal.history(&choice.input)?}),
        ),
        "terminal" => terminal(brain, &choice.input).await,
        "run" => {
            let command = run_command(&choice.input)?;
            let mut result = terminal_with_view(brain, &command, true).await?;
            result["command"] = json!(command);
            result["experiment"] = json!(true);
            Ok(result)
        }
        "remember" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Memory {
                kind: String,
                text: String,
            }
            let m: Memory = serde_json::from_str(&choice.input)?;
            brain.journal.remember(&m.kind, &m.text)?;
            Ok(
                json!({"stored":true,"kind":m.kind,"authority":"source-labelled memory; not native proof"}),
            )
        }
        "goal" => {
            ensure(
                !choice.input.trim().is_empty() && choice.input.len() <= 2048,
                "Bounded goal required",
            )?;
            brain.goal = choice.input.clone();
            brain.journal.select_goal(
                &brain.goal,
                "agent_goal_selected",
                json!({"goal":brain.goal,"reason":choice.summary}),
                None,
            )?;
            Ok(json!({"goal":brain.goal}))
        }
        "replace_worker" => {
            let path = std::path::Path::new(&choice.input);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                brain.cfg.workspace.join(path)
            };
            let selected = crate::worker::replace(&brain.cfg, &path)?;
            brain
                .journal
                .event("decision_worker_selected", selected.clone())?;
            Ok(selected)
        }
        "report" => {
            ensure(
                !choice.input.trim().is_empty() && choice.input.len() <= 2048,
                "Bounded progress report required",
            )?;
            brain.journal.event(
                "progress_report",
                json!({"goal":brain.goal,"report":choice.input,"completed":false}),
            )?;
            Ok(json!({"recorded":"progress report","goal_remains_active":true}))
        }
        "complete" => {
            let claim = crate::completion::claim(&choice.input)?;
            let crate::completion::Status::Succeeded = claim.status;
            let claimed_goal = brain.goal.clone();
            brain.refresh().await?;
            let checks = crate::completion::verify(&brain.cfg, &claimed_goal, &brain.observed)?;
            brain.journal.event("model_completion_claim",json!({"goal":claimed_goal,"claim":choice.input,"observed":crate::projection(&brain.observed),"externally_verified":false}))?;
            Ok(
                json!({"recorded":"model claim","summary":claim.summary,"configured_checks":checks,"native_observation":crate::projection(&brain.observed),"benchmark_pass_granted":false}),
            )
        }
        _ => Err("Unknown tool".into()),
    }
}

fn material_recipe<'a>(query: &ObservationQuery, result: &'a Value) -> Option<&'a Value> {
    if query.capability != "minecraft:recipes" || result["format"] != "minecraft-runtime-recipes-v1"
    {
        return None;
    }
    let rows = result["matches"].as_array()?;
    if rows.len() != 1
        || query
            .input
            .get("recipe")
            .is_some_and(|id| rows[0]["id"] != *id)
    {
        return None;
    }
    // The summary names this row only. A paged output/type search can have other
    // candidates; one returned row is not a uniqueness or completeness proof.
    Some(&rows[0])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn material_feedback_covers_single_search_rows_without_inventing_uniqueness() {
        let mut query = ObservationQuery {
            capability: "minecraft:recipes".into(),
            input: json!({"output":"fixture:result","limit":1}),
        };
        let mut result = json!({"format":"minecraft-runtime-recipes-v1","complete":false,"next_offset":7,"matches":[{"id":"fixture:recipe"}]});
        assert_eq!(
            material_recipe(&query, &result).unwrap()["id"],
            "fixture:recipe"
        );
        query.input = json!({"recipe":"fixture:other"});
        assert!(material_recipe(&query, &result).is_none());
        query.input = json!({"recipe":"fixture:recipe"});
        assert!(material_recipe(&query, &result).is_some());
        result["matches"] = json!([{"id":"fixture:recipe"},{"id":"fixture:another"}]);
        assert!(material_recipe(&query, &result).is_none());
        result["matches"] = json!([]);
        assert!(material_recipe(&query, &result).is_none());
        result["matches"] = json!([{"id":"fixture:recipe"}]);
        query.capability = "fixture:another_provider".into();
        assert!(material_recipe(&query, &result).is_none());
    }
    #[test]
    fn exact_edits_preserve_source_on_stale_ambiguous_and_escaping_requests() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        let original = "fn main() { let λ = 1; }\n";
        fs::write(&path, original).unwrap();
        let change=json!({"old":"λ = 1","new":"λ = 2","expected_sha256":crate::digest(original.as_bytes())}).to_string();
        let result = edit(dir.path(), "main.rs", &change).unwrap();
        assert_eq!(result["replacements"], 1);
        let current = fs::read(&path).unwrap();
        assert!(
            String::from_utf8(current.clone())
                .unwrap()
                .contains("λ = 2")
        );
        assert!(edit(dir.path(), "main.rs", &change).is_err());
        assert!(edit(dir.path(), "../escape.rs", &change).is_err());
        assert_eq!(fs::read(&path).unwrap(), current);
        fs::write(&path, "aaaa").unwrap();
        for change in [
            json!({"old":"aa","new":"x"}),
            json!({"old":"missing","new":"x"}),
            json!({"old":"","new":"x"}),
            json!({"old":"a","new":"a"}),
        ] {
            assert!(edit(dir.path(), "main.rs", &change.to_string()).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"aaaa");
        }
        std::os::unix::fs::symlink("main.rs", dir.path().join("link.rs")).unwrap();
        assert!(
            edit(
                dir.path(),
                "link.rs",
                &json!({"old":"aaaa","new":"x"}).to_string()
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn incomplete_reports_and_legacy_completion_cannot_finish_a_goal() {
        let (_root, mut brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
        let revision = brain
            .journal
            .select_goal("fixture", "fixture", json!({}), None)
            .unwrap();
        let mut command = Choice {
            tool: "report".into(),
            input: "Tool built; workbench still needs recovery".into(),
            path: String::new(),
            summary: "progress".into(),
            notes: String::new(),
            experiment: false,
        };
        let result = execute(&mut brain, &command).await.unwrap();
        assert_eq!(result["goal_remains_active"], true);
        assert!(!brain.journal.goal_completed("fixture").unwrap());
        command.tool = "complete".into();
        command.input = r#"{"kind":"complete","text":"Requirement is not met"}"#.into();
        assert!(
            execute(&mut brain, &command)
                .await
                .unwrap_err()
                .to_string()
                .contains("use report")
        );
        assert_eq!(
            brain.journal.get("goal_revision").unwrap().unwrap(),
            revision.to_string()
        );
        assert!(!brain.journal.goal_completed("fixture").unwrap());
    }
    #[tokio::test]
    async fn nested_listing_supplies_a_readable_path_and_errors_identify_the_target() {
        let (_root, mut brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
        let src = brain.cfg.workspace.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(
            src.join("main.rs"),
            "fn main() { println!(\"fixture\"); }\n",
        )
        .unwrap();
        let mut choice = Choice {
            tool: "list".into(),
            input: "src".into(),
            path: String::new(),
            summary: "fixture".into(),
            notes: String::new(),
            experiment: false,
        };
        let listing = execute(&mut brain, &choice).await.unwrap();
        assert_eq!(
            listing["directory"],
            fs::canonicalize(&src).unwrap().display().to_string()
        );
        let path = listing["entries"][0]["path"]
            .as_str()
            .expect("Listed child must have a directly usable path");
        choice.tool = "read".into();
        choice.input = path.into();
        let content = execute(&mut brain, &choice).await.unwrap();
        assert!(content["content"].as_str().unwrap().contains("fixture"));
        choice.tool = "write".into();
        choice.path = listing["entries"][0]["relative_path"]
            .as_str()
            .unwrap()
            .to_owned();
        choice.input = "fn main() { /* changed */ }\n".into();
        execute(&mut brain, &choice).await.unwrap();
        assert!(
            fs::read_to_string(src.join("main.rs"))
                .unwrap()
                .contains("changed")
        );
        choice.tool = "read".into();
        choice.path.clear();
        choice.input = "main.rs".into();
        let error = execute(&mut brain, &choice).await.unwrap_err().to_string();
        assert!(
            error.contains(&brain.cfg.workspace.join("main.rs").display().to_string()),
            "Missing-file feedback must reveal which base was used: {error}"
        );
    }
    #[test]
    fn read_only_queries_retain_typed_bounds_without_actor_or_endpoint_overrides() {
        let good = json!({"capability":"minecraft:block_search","input":{"tag":"minecraft:logs","radius":32,"limit":8,"offset":0}});
        let parsed = observation_query(&good.to_string()).unwrap();
        assert_eq!(parsed.input["tag"], "minecraft:logs");
        for key in ["actor", "player_uuid", "url", "path", "command", "plugin"] {
            let mut bad = good.clone();
            bad[key] = json!("override");
            assert!(observation_query(&bad.to_string()).is_err());
        }
        for bad in [
            json!({"capability":"not a registry id","input":{}}),
            json!({"capability":"minecraft:block_search","input":[]}),
            json!({"capability":"minecraft:block_search","input":{"value":"x".repeat(8192)}}),
        ] {
            assert!(observation_query(&bad.to_string()).is_err());
        }
    }
    #[test]
    fn run_selects_one_binary_without_allowing_shell_injection() {
        assert_eq!(
            run_command("candidate_2").unwrap(),
            "cargo run --offline --bin candidate_2 --message-format=json"
        );
        for bad in [
            "",
            "--all",
            "bad; touch /tmp/escape",
            "$(id)",
            "a b",
            "../other",
        ] {
            assert!(run_command(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn accidental_write_escape_is_refused() {
        let t = tempfile::tempdir().unwrap();
        assert!(writable(t.path(), "../outside.rs").is_err());
        assert!(writable(t.path(), "/etc/test").is_err());
        assert!(writable(t.path(), "src/main.rs").is_ok());
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), t.path().join("escape")).unwrap();
        assert!(writable(t.path(), "escape/file.rs").is_err());
    }
    #[tokio::test]
    async fn tool_output_is_bounded() {
        let input = std::io::Cursor::new(vec![b'x'; 20000]);
        let bytes = bounded_output(input).await.unwrap();
        assert_eq!(bytes.len(), 20000);
        assert!(bounded_output(tokio::io::repeat(b'x')).await.is_err());
    }
    #[tokio::test]
    async fn terminal_preserves_the_images_explicit_toolchain_path() {
        let t = tempfile::tempdir().unwrap();
        // Some coreutils installations reject execution under a different
        // utility name. Use the platform shell rather than renaming echo;
        // the unique name still detects a login shell overwriting PATH.
        std::os::unix::fs::symlink("/bin/sh", t.path().join("image-toolchain-probe")).unwrap();
        let result = local_shell("image-toolchain-probe -c 'printf \"ready\\n\"'")
            .env("PATH", t.path())
            .output()
            .await
            .unwrap();
        assert!(
            result.status.success(),
            "toolchain PATH probe failed: status={}, stderr={}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"ready\n");
    }
}
