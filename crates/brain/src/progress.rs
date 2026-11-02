//! Cost-control hints, never authority for gameplay success or skill reliability.
use crate::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub fn workspace_fingerprint(root: &Path) -> Result<String> {
    fn visit(
        root: &Path,
        path: &Path,
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
            let meta = fs::symlink_metadata(&path)?;
            hash.update(path.strip_prefix(root)?.to_string_lossy().as_bytes());
            if meta.is_dir() {
                visit(root, &path, depth + 1, remaining, hash)?;
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
    visit(root, root, 0, &mut (16 * 1024 * 1024), &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

pub fn executed(tool: &str, result: &serde_json::Value, before: &str, after: &str) -> bool {
    before != after
        || tool == "complete"
        || (tool == "sdk" && !crate::tool_failed(result))
        || result["inventory_changed"] == true
}

#[cfg(test)]
mod tests {
    use super::*;
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
