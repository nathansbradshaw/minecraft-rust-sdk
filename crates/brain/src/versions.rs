//! Content-addressed client programs. Restoring source never runs or replays it.
use crate::{Journal, Result, digest, ensure, model::Choice, private_read};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub files: BTreeMap<String, String>,
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn valid_path(p: &str) -> bool {
    if p.len() > 512 || p.chars().any(char::is_control) {
        return false;
    }
    let p = Path::new(p);
    !p.as_os_str().is_empty()
        && !p.is_absolute()
        && p.components().all(|c| matches!(c, Component::Normal(_)))
        && (p.extension().is_some_and(|e| e == "rs")
            || matches!(
                p.file_name().and_then(|v| v.to_str()),
                Some("Cargo.toml" | "Cargo.lock")
            ))
}
fn prepare(state: &Path) -> Result<std::path::PathBuf> {
    let root = state.join("skills");
    for dir in [&root, &root.join("blobs"), &root.join("versions")] {
        fs::create_dir_all(dir)?;
        let m = fs::symlink_metadata(dir)?;
        ensure(
            m.is_dir() && !m.file_type().is_symlink(),
            "Skill storage must be ordinary directories",
        )?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(root)
}
fn immutable(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        ensure(
            private_read(path, 1_048_576)? == bytes,
            "Retained skill source was modified",
        )?;
        return Ok(());
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::File::open(path.parent().ok_or("Parent required")?)?.sync_all()?;
    Ok(())
}
fn retain(root: &Path, files: BTreeMap<String, String>) -> Result<String> {
    let manifest = Manifest {
        format: "rust-source-version-v1".into(),
        files,
    };
    let bytes = serde_json::to_vec(&manifest)?;
    let id = digest(&bytes);
    immutable(&root.join("versions").join(format!("{id}.json")), &bytes)?;
    Ok(id)
}
/// Snapshot the editable Rust crate, excluding caches, state and symlinks.
/// Identical file bytes are shared across all versions; executables are rebuilt.
pub fn snapshot(workspace: &Path, state: &Path) -> Result<Option<String>> {
    let workspace = workspace.canonicalize()?;
    let state = state.canonicalize()?;
    let root = prepare(&state)?;
    fn visit(
        base: &Path,
        path: &Path,
        state: &Path,
        root: &Path,
        depth: usize,
        remaining: &mut usize,
        files: &mut BTreeMap<String, String>,
    ) -> Result<()> {
        ensure(depth < 12, "Program source nesting exceeds version budget")?;
        let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if matches!(
                entry.file_name().to_str(),
                Some("target" | ".git" | ".cache" | "skills")
            ) {
                continue;
            }
            let p = entry.path();
            if p == state {
                continue;
            }
            let m = fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() {
                continue;
            }
            if m.is_dir() {
                visit(base, &p, state, root, depth + 1, remaining, files)?;
            } else if m.is_file() {
                let relative = p
                    .strip_prefix(base)?
                    .to_str()
                    .ok_or("UTF-8 source path required")?;
                if !valid_path(relative) {
                    continue;
                }
                ensure(
                    files.len() < 128 && m.len() <= 65536,
                    "Bounded Rust source file required",
                )?;
                let bytes = fs::read(&p)?;
                ensure(
                    bytes.len() <= *remaining,
                    "Program source exceeds 1 MiB version budget",
                )?;
                std::str::from_utf8(&bytes)?;
                *remaining -= bytes.len();
                let hash = digest(&bytes);
                immutable(&root.join("blobs").join(&hash), &bytes)?;
                files.insert(relative.to_owned(), hash);
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(
        &workspace,
        &workspace,
        &state,
        &root,
        0,
        &mut 1_048_576,
        &mut files,
    )?;
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(retain(&root, files)?))
}

pub fn inspect(state: &Path, id: &str) -> Result<Value> {
    ensure(valid_hash(id), "Exact source-version SHA256 required")?;
    let root = prepare(state)?;
    let bytes = private_read(&root.join("versions").join(format!("{id}.json")), 65536)?;
    ensure(digest(&bytes) == id, "Version manifest hash mismatch")?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure(
        manifest.format == "rust-source-version-v1"
            && !manifest.files.is_empty()
            && manifest.files.len() <= 128,
        "Unsupported source manifest",
    )?;
    for (path, hash) in &manifest.files {
        ensure(
            valid_path(path) && valid_hash(hash),
            "Invalid version source reference",
        )?;
    }
    Ok(
        json!({"version":id,"manifest":manifest,"blob_directory":root.join("blobs"),"authority":"retained container-local source; inspect and recompile; no gameplay success granted"}),
    )
}

pub fn restore(state: &Path, workspace: &Path, id: &str) -> Result<Value> {
    let found = inspect(state, id)?;
    let manifest: Manifest = serde_json::from_value(found["manifest"].clone())?;
    let root = prepare(state)?;
    let workspace = workspace.canonicalize()?;
    // Verify the entire input before changing a single current source file.
    let mut content = Vec::new();
    let mut total = 0;
    for (path, hash) in &manifest.files {
        let bytes = private_read(&root.join("blobs").join(hash), 65536)?;
        total += bytes.len();
        ensure(
            total <= 1_048_576 && digest(&bytes) == *hash,
            "Retained source hash/budget mismatch",
        )?;
        content.push((path, bytes));
    }
    let previous = snapshot(&workspace, state)?;
    for (path, bytes) in content {
        let target = workspace.join(path);
        let mut parent = target.parent().ok_or("Parent required")?;
        while !parent.exists() {
            parent = parent.parent().ok_or("Workspace ancestor required")?;
        }
        ensure(
            parent.canonicalize()?.starts_with(&workspace),
            "Source parent symlink leaves workspace",
        )?;
        ensure(
            !fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()),
            "Do not replace a source symlink",
        )?;
        fs::create_dir_all(target.parent().ok_or("Parent required")?)?;
        crate::private_write(&target, &bytes)?;
    }
    Ok(
        json!({"restored_version":id,"previous_version":previous,"files":manifest.files.len(),"executed":false,"next":"Inspect dependencies, compile, observe current state and use fresh intention IDs; old menu/entity/session handles are invalid."}),
    )
}

impl Journal {
    pub fn remember_program(
        &self,
        state: &Path,
        version: &str,
        goal: &str,
        choice: &Choice,
        result: &Value,
    ) -> Result<()> {
        let files = inspect(state, version)?;
        let filenames = files["manifest"]["files"]
            .as_object()
            .ok_or("Manifest files required")?
            .keys()
            .take(2)
            .cloned()
            .collect::<Vec<_>>();
        // Lead with semantic results. Cargo's large stdout preview can otherwise
        // occupy the entire recalled excerpt and hide every gameplay outcome.
        let actions: Vec<_> = result["local_action_feedback"]["requests"]
            .as_array()
            .into_iter()
            .flatten()
            .take(4)
            .map(|row| {
                let action = &row["request"]["action"];
                json!({"kind":action["kind"],"stage":action["stage"],
                    "block":action["block"],"outcome":row["response"]["outcome"],
                    "verified":row["response"]["native_effect_verified"],
                    "reused_action_id":row["response"]["gateway_delivery"]["action_id_already_recorded"]})
            })
            .collect();
        let inventory = result["inventory_delta"].as_array().map(|rows|
            json!({"changes":rows.iter().take(4).collect::<Vec<_>>(),"omitted":rows.len().saturating_sub(4)}));
        let errors: Vec<_> = result["compiler_diagnostics"]["errors"]
            .as_array().into_iter().flatten().take(2).map(|error|
                json!({"code":error["code"],"message":error["message"].as_str().unwrap_or_default().chars().take(160).collect::<String>()})).collect();
        let text = format!(
            "Program {}. Exit {}. Retained inventory delta {}. Action sample {}. Error sample {}. Goal: {}. Files: {}. Block change is not pickup; exit zero is not task success.",
            choice.input.chars().take(80).collect::<String>(),
            result["exit_code"],
            json!(inventory),
            json!(actions),
            json!(errors),
            goal,
            filenames.join(", "),
        );
        let effects:Vec<_>=result["local_action_feedback"]["requests"].as_array().into_iter().flatten().filter(|row|row["response"]["outcome"]=="observed"&&row["response"]["native_effect_verified"]==true&&row["response"]["gateway_delivery"]["action_id_already_recorded"]!=true).take(4).map(|row|json!({"action_id":row["request"]["action_id"],"kind":row["request"]["action"]["kind"]})).collect();
        let gain = result["inventory_delta"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row["change"].as_i64().is_some_and(|n| n > 0));
        let status = if result["success"] == true
            && !crate::tool_failed(result)
            && gain
            && !effects.is_empty()
        {
            "experimental; client-observed gain and native receipt references, not independent reliability"
        } else {
            "candidate; successful exit alone is not task success"
        };
        self.remember_from("skill",&text,"program_experiment",&json!({"version":version,"file_preview":filenames,"restore":"minecraft-sdk-brain skill-restore STATE WORKSPACE VERSION","execution":"source captured before experiment; restore never runs code","status":status,"exit_code":result["exit_code"],"inventory_changed":result["inventory_changed"],"native_receipt_references":effects,"action_bindings":"Historical action/menu/entity IDs must not replay; inspect/reconcile old intentions and observe new state."}))?;
        Ok(())
    }

    /// Recover the agent's own successfully written older files. Historical
    /// fragments remain candidates, never retroactively verified whole skills.
    pub fn import_program_history(&self, state: &Path) -> Result<u64> {
        let mut cursor = self
            .get("program_history_cursor")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let root = prepare(state)?;
        let mut imported = 0;
        for _ in 0..64 {
            let rows: Vec<(i64, String, Option<String>)> = {
                let mut q=self.db.prepare("SELECT e.id,e.data,(SELECT json_extract(g.data,'$.goal') FROM events g WHERE g.id<e.id AND g.kind IN ('brain_started','player_goal_selected','agent_goal_selected') ORDER BY g.id DESC LIMIT 1) FROM events e WHERE e.kind='choice' AND e.id>?1 ORDER BY e.id LIMIT 64")?;
                q.query_map([cursor], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<std::result::Result<_, _>>()?
            };
            if rows.is_empty() {
                break;
            }
            for (sequence, bytes, goal) in rows {
                let value: Value = serde_json::from_str(&bytes)?;
                let choice = &value["choice"];
                if choice["tool"] == "write"
                    && let (Some(path), Some(source)) =
                        (choice["path"].as_str(), choice["input"].as_str())
                    && valid_path(path)
                    && source.len() <= 65536
                {
                    let hash = digest(source.as_bytes());
                    let completed=self.db.query_row("SELECT data FROM events WHERE kind='tool_result' AND id>?1 AND id<coalesce((SELECT min(id) FROM events WHERE kind='choice' AND id>?1),9223372036854775807) AND json_extract(data,'$.tool')='write' ORDER BY id LIMIT 1",[sequence],|r|r.get::<_,String>(0));
                    if let Ok(bytes) = completed {
                        let completed: Value = serde_json::from_str(&bytes)?;
                        if completed["error"].is_null()
                            && completed["result"]["sha256"] == hash
                            && completed["result"]["path"] == path
                        {
                            immutable(&root.join("blobs").join(&hash), source.as_bytes())?;
                            let version = retain(&root, BTreeMap::from([(path.to_owned(), hash)]))?;
                            let text = format!(
                                "Historical generated program file {}. Goal: {}. Agent summary: {}. Source version {}",
                                path,
                                goal.as_deref().unwrap_or("not recorded"),
                                choice["summary"].as_str().unwrap_or("not recorded"),
                                version
                            );
                            self.remember_from("skill",&text,"historical_program_file",&json!({"version":version,"choice_sequence":sequence,"status":"legacy file candidate; successful write, not proof of compilation or gameplay","completeness":"One retained historical file; inspect imports and current manifest before use","action_bindings":"Reobserve; historical entity/menu/session IDs cannot be reused as live handles"}))?;
                            imported += 1;
                        }
                    }
                }
                cursor = sequence;
                self.set("program_history_cursor", &cursor.to_string())?;
            }
        }
        Ok(imported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_native_receipt_cannot_promote_a_new_program_experiment() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let state = dir.path().join("state");
        fs::create_dir(&work).unwrap();
        fs::create_dir(&state).unwrap();
        fs::write(work.join("main.rs"), "fn main() {}").unwrap();
        let version = snapshot(&work, &state).unwrap().unwrap();
        let j = Journal::open(&state.join("brain.sqlite")).unwrap();
        let choice = Choice {
            tool: "run".into(),
            input: "gather".into(),
            path: String::new(),
            summary: String::new(),
            notes: String::new(),
            experiment: true,
        };
        j.remember_program(&state,&version,"gather materials",&choice,&json!({"success":true,"exit_code":0,"inventory_changed":true,
            "inventory_delta":[{"item":"fixture:material","change":1}],
            "local_action_feedback":{"requests":[{"request":{"action_id":"old-id","action":{"kind":"dig"}},
                "response":{"outcome":"observed","native_effect_verified":true,"gateway_delivery":{"action_id_already_recorded":true,"new_intention_dispatched":false}}}]}})).unwrap();
        let found = j.recall("gather materials").unwrap();
        assert!(
            found[0]["provenance"]["status"]
                .as_str()
                .unwrap()
                .starts_with("candidate")
        );
        assert_eq!(
            found[0]["provenance"]["native_receipt_references"],
            json!([])
        );
        assert!(
            found[0]["text"]
                .as_str()
                .unwrap()
                .contains("\"reused_action_id\":true")
        );
    }
    #[test]
    fn program_outcomes_and_source_versions_are_retrievable_without_promoting_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let state = dir.path().join("state");
        fs::create_dir(&work).unwrap();
        fs::create_dir(&state).unwrap();
        fs::write(work.join("main.rs"), "fn main() {}").unwrap();
        let version = snapshot(&work, &state).unwrap().unwrap();
        let choice = Choice {
            tool: "run".into(),
            input: "craft_tools".into(),
            path: String::new(),
            summary: "Try crafting a wooden pickaxe".into(),
            notes: String::new(),
            experiment: true,
        };
        {
            let j = Journal::open(&state.join("brain.sqlite")).unwrap();
            j.remember_program(&state,&version,"Craft a wooden pickaxe",&choice,&json!({"success":true,"exit_code":0,"stdout":"compiler-artifact boilerplate".repeat(1000),"inventory_changed":false,"inventory_delta":[],"local_action_feedback":{"requests":[{"request":{"action_id":"own-finish","action":{"kind":"dig","stage":"finish","block":{"x":17,"y":64,"z":-9}}},"response":{"outcome":"observed","native_effect_verified":true}}]}})).unwrap();
        }
        let j = Journal::open(&state.join("brain.sqlite")).unwrap();
        let found = j.recall("Resume crafting the wooden pickaxe").unwrap();
        assert_eq!(found[0]["kind"], "skill");
        assert_eq!(found[0]["provenance"]["version"], version);
        assert!(
            found[0]["provenance"]["status"]
                .as_str()
                .unwrap()
                .starts_with("candidate")
        );
        assert_eq!(found[0]["native_authority"], false);
        let excerpt = found[0]["text"].as_str().unwrap();
        assert!(excerpt.contains("Retained inventory delta {\"changes\":[],\"omitted\":0}"));
        assert!(excerpt.contains("\"stage\":\"finish\""));
        assert!(excerpt.contains("\"z\":-9"));
        assert!(!excerpt.contains("compiler-artifact"));
        assert!(inspect(&state, &version).is_ok());
    }
    #[test]
    fn own_historical_files_are_recovered_once_without_inventing_gameplay_success() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        fs::create_dir(&state).unwrap();
        let db = state.join("brain.sqlite");
        let source = "fn main() { println!(\"retained log program\"); }";
        {
            let j = Journal::open(&db).unwrap();
            j.event("brain_started", json!({"goal":"collect a spruce log"}))
                .unwrap();
            j.event("choice",json!({"choice":{"tool":"write","path":"src/bin/collect_log.rs","input":source,"summary":"Collect a spruce log using SDK observations"}})).unwrap();
            j.event("tool_result",json!({"tool":"write","result":{"path":"src/bin/collect_log.rs","sha256":digest(source.as_bytes())}})).unwrap();
            j.event("choice",json!({"choice":{"tool":"write","path":"src/bin/failed.rs","input":"not saved","summary":"Failed candidate"}})).unwrap();
            j.event(
                "tool_result",
                json!({"tool":"write","error":"write failed"}),
            )
            .unwrap();
            assert_eq!(j.import_program_history(&state).unwrap(), 1);
        }
        let j = Journal::open(&db).unwrap();
        assert_eq!(j.import_program_history(&state).unwrap(), 0);
        let found = j.recall("Please gather spruce logs").unwrap();
        assert_eq!(found.as_array().unwrap().len(), 1);
        let version = found[0]["provenance"]["version"].as_str().unwrap();
        assert_eq!(found[0]["origin"], "historical_program_file");
        assert!(
            found[0]["provenance"]["status"]
                .as_str()
                .unwrap()
                .contains("not proof")
        );
        let inspected = inspect(&state, version).unwrap();
        let hash = inspected["manifest"]["files"]["src/bin/collect_log.rs"]
            .as_str()
            .unwrap();
        assert_eq!(
            fs::read(state.join("skills/blobs").join(hash)).unwrap(),
            source.as_bytes()
        );
        assert!(
            !dir.path().join("src/bin/collect_log.rs").exists(),
            "indexing does not restore or execute source"
        );
    }
    #[test]
    fn retained_program_can_be_restored_after_restart_and_recompiled_without_regeneration() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("work");
        let state = dir.path().join("state");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::create_dir(&state).unwrap();
        let source = "fn main(){println!(\"remembered-program\");}";
        fs::write(workspace.join("src/main.rs"), source).unwrap();
        fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname='memory-fixture'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        let version = snapshot(&workspace, &state).unwrap().unwrap();
        assert_eq!(snapshot(&workspace, &state).unwrap(), Some(version.clone()));
        fs::write(
            workspace.join("src/main.rs"),
            "fn main(){println!(\"different\");}",
        )
        .unwrap();
        let restored = restore(&state, &workspace, &version).unwrap();
        assert_ne!(restored["previous_version"], version);
        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            source
        );
        let binary = dir.path().join("reused");
        let built = std::process::Command::new("rustc")
            .arg(workspace.join("src/main.rs"))
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        assert_eq!(
            std::process::Command::new(&binary).output().unwrap().stdout,
            b"remembered-program\n"
        );
        let previous = restored["previous_version"].as_str().unwrap();
        assert!(
            inspect(&state, previous).is_ok(),
            "unsaved current source remains recoverable"
        );
    }
    #[test]
    fn tampered_source_and_escaping_restore_paths_cannot_overwrite_current_code() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("work");
        let state = dir.path().join("state");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&state).unwrap();
        fs::write(workspace.join("main.rs"), "old").unwrap();
        let id = snapshot(&workspace, &state).unwrap().unwrap();
        let info = inspect(&state, &id).unwrap();
        let blob = info["manifest"]["files"]["main.rs"].as_str().unwrap();
        fs::write(state.join("skills/blobs").join(blob), "tampered").unwrap();
        fs::write(workspace.join("main.rs"), "keep").unwrap();
        assert!(restore(&state, &workspace, &id).is_err());
        assert_eq!(
            fs::read_to_string(workspace.join("main.rs")).unwrap(),
            "keep"
        );
        for path in [
            "../main.rs",
            "/main.rs",
            "src/../../main.rs",
            "credentials.key",
        ] {
            assert!(!valid_path(path));
        }
    }
}
