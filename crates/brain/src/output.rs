//! Private, bounded terminal feedback; never authoritative gameplay evidence.
use crate::{Result, digest, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub const STREAM_LIMIT: usize = 4 * 1024 * 1024;

fn used(root: &Path) -> Result<u64> {
    fs::create_dir_all(root)?;
    let meta = fs::symlink_metadata(root)?;
    ensure(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Terminal output directory must be regular",
    )?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    let mut bytes = 0u64;
    for row in fs::read_dir(root)? {
        let meta = fs::symlink_metadata(row?.path())?;
        ensure(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Unexpected terminal output entry",
        )?;
        bytes = bytes
            .checked_add(meta.len())
            .ok_or("Terminal output size overflow")?;
    }
    Ok(bytes)
}

pub fn prepare(root: &Path) -> Result<()> {
    used(root)?;
    Ok(())
}

pub fn save(root: &Path, id: &str, stdout: &[u8], stderr: &[u8], budget: u64) -> Result<Value> {
    ensure(
        !id.is_empty()
            && id.len() <= 80
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "Invalid terminal output ID",
    )?;
    ensure(
        stdout.len() <= STREAM_LIMIT && stderr.len() <= STREAM_LIMIT,
        "Terminal stream exceeds output budget",
    )?;
    ensure(
        used(root)?
            .checked_add((stdout.len() + stderr.len()) as u64)
            .is_some_and(|n| n <= budget),
        "Terminal output storage budget exhausted",
    )?;
    let mut files = serde_json::Map::new();
    for (name, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        let path = root.join(format!("{id}.{name}.txt"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        files.insert(
            name.into(),
            json!({"path":path,"bytes":bytes.len(),"sha256":digest(bytes)}),
        );
    }
    Ok(Value::Object(files))
}

pub fn preview(bytes: &[u8]) -> String {
    if bytes.len() <= 2000 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    format!(
        "{}\n[preview omits {} bytes; inspect saved output]\n{}",
        String::from_utf8_lossy(&bytes[..1000]),
        bytes.len() - 2000,
        String::from_utf8_lossy(&bytes[bytes.len() - 1000..])
    )
}

/// A view for the engine's exact `cargo run --message-format=json` command.
/// Never alter the saved streams: repair binds Cargo artifacts from those bytes.
pub fn cargo_run_preview(bytes: &[u8]) -> (String, Value) {
    let mut remaining = Vec::new();
    let mut build_finished = None;
    let mut omitted_records = 0usize;
    let mut warning_count = 0usize;
    let mut warnings = Vec::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if build_finished.is_none()
            && let Ok(value) = serde_json::from_slice::<Value>(line)
        {
            let cargo_record = match value["reason"].as_str() {
                Some("compiler-artifact") => {
                    value["package_id"].is_string()
                        && value["target"].is_object()
                        && value["filenames"].is_array()
                }
                Some("build-script-executed") => {
                    value["package_id"].is_string()
                        && value["linked_libs"].is_array()
                        && value["linked_paths"].is_array()
                }
                Some("compiler-message") => {
                    let valid = value["package_id"].is_string()
                        && value["target"].is_object()
                        && value["message"]["level"].is_string()
                        && value["message"]["message"].is_string();
                    if valid && value["message"]["level"] == "warning" {
                        warning_count += 1;
                        if warnings.len() < 2 {
                            warnings.push(json!({
                                "message":value["message"]["message"].as_str().unwrap_or_default().chars().take(400).collect::<String>(),
                                "rendered":value["message"]["rendered"].as_str().unwrap_or_default().chars().take(800).collect::<String>()
                            }));
                        }
                    }
                    valid
                }
                Some("build-finished") => {
                    if let Some(success) = value["success"].as_bool() {
                        build_finished = Some(success);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if cargo_record {
                omitted_records += 1;
                continue;
            }
        }
        // After Cargo finishes, every byte belongs to the remaining stream.
        // A program can print JSON with arbitrary Cargo-like reason fields.
        remaining.extend_from_slice(line);
    }
    let metadata = json!({
        "format":"cargo-run-preview-v1","build_finished":build_finished,
        "omitted_cargo_records":omitted_records,"warning_count":warning_count,"warnings":warnings,
        "remaining_stdout_bytes":remaining.len(),"preview_truncated":remaining.len()>2000,
        "scope":"Recognized Cargo prelude omitted from this view only. Primary errors remain in compiler_diagnostics; complete raw stdout/hash remain in output_files. A successful build or process does not verify gameplay."
    });
    (preview(&remaining), metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cargo_prelude_does_not_obscure_gameplay_and_raw_capture_is_unchanged() {
        let artifact = json!({"reason":"compiler-artifact","package_id":"fixture","target":{"name":"candidate"},"filenames":["x".repeat(12000)]});
        let program = "inventory-confirmed: oak_log=1\n{\"reason\":\"compiler-artifact\",\"package_id\":\"program-owned\",\"target\":{},\"filenames\":[]}\n";
        let raw =
            format!("{artifact}\n{{\"reason\":\"build-finished\",\"success\":true}}\n{program}")
                .into_bytes();
        let before = digest(&raw);
        let (view, metadata) = cargo_run_preview(&raw);
        assert_eq!(view, program);
        assert_eq!(metadata["omitted_cargo_records"], 2);
        assert_eq!(metadata["build_finished"], true);
        assert_eq!(metadata["preview_truncated"], false);
        let dir = tempfile::tempdir().unwrap();
        let files = save(dir.path(), "cargo-view", &raw, b"", 32 * 1024 * 1024).unwrap();
        assert_eq!(files["stdout"]["sha256"], before);
        assert_eq!(
            fs::read(files["stdout"]["path"].as_str().unwrap()).unwrap(),
            raw
        );
        assert!(
            preview(&raw).contains("compiler-artifact"),
            "Generic terminal preview remains raw"
        );
    }
    #[test]
    fn failed_build_keeps_primary_diagnostics_unknown_lines_and_bounded_warnings() {
        let warning = json!({"reason":"compiler-message","package_id":"fixture","target":{},"message":{"level":"warning","message":"check the ignored result","rendered":"w".repeat(4000)}});
        let error = json!({"reason":"compiler-message","package_id":"fixture","target":{},"message":{"level":"error","message":"missing helper","rendered":"error: missing helper"}});
        let unknown = "{broken JSON\n{\"reason\":\"compiler-artifact\",\"note\":\"incomplete record stays visible\"}\n";
        let raw = format!(
            "{warning}\n{warning}\n{warning}\n{error}\n{unknown}{{\"reason\":\"build-finished\",\"success\":false}}\n"
        );
        let (view, metadata) = cargo_run_preview(raw.as_bytes());
        assert_eq!(view, unknown);
        assert_eq!(metadata["build_finished"], false);
        assert_eq!(metadata["warning_count"], 3);
        assert_eq!(metadata["warnings"].as_array().unwrap().len(), 2);
        assert!(metadata["warnings"][0]["rendered"].as_str().unwrap().len() <= 800);
        assert_eq!(
            crate::feedback::compiler_diagnostics(raw.as_bytes())["total_errors"],
            1
        );
        let text = "ordinary program output without Cargo records\n";
        assert_eq!(cargo_run_preview(text.as_bytes()).0, text);
        let after = format!(
            "{{\"reason\":\"build-finished\",\"success\":true}}\n{}tail",
            "p".repeat(5000)
        );
        let (view, metadata) = cargo_run_preview(after.as_bytes());
        assert!(view.ends_with("tail"));
        assert_eq!(metadata["preview_truncated"], true);
    }
    #[test]
    fn late_errors_remain_retrievable_and_outputs_cannot_be_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = vec![b'x'; 20000];
        data.extend_from_slice(b"compiler failure beyond the old preview");
        let files = save(
            dir.path(),
            "run-1",
            b"ordinary output",
            &data,
            32 * 1024 * 1024,
        )
        .unwrap();
        let path = Path::new(files["stderr"]["path"].as_str().unwrap());
        assert_eq!(fs::read(path).unwrap(), data);
        assert!(preview(&data).contains("compiler failure beyond the old preview"));
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        assert!(save(dir.path(), "run-1", b"replacement", b"", 32 * 1024 * 1024).is_err());
        assert_eq!(fs::read(path).unwrap(), data);
        assert!(save(dir.path(), "run-2", b"not retained", b"", 1).is_err());
        assert_eq!(fs::read(path).unwrap(), data);
        // A full capture budget must not prevent a container-local command
        // from explicitly archiving/managing its older outputs.
        assert!(prepare(dir.path()).is_ok());
    }
}
