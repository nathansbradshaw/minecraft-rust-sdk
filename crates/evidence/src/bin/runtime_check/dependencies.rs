//! Read-only bounded dependency-tree identity. Never execute or copy its contents.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

const MAX_ENTRIES: u64 = 65_536;
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
struct Scan {
    hash: Sha256,
    entries: u64,
    bytes: u64,
    started: Instant,
}
fn unchanged(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
impl Scan {
    fn deadline(&self) -> io::Result<()> {
        if self.started.elapsed() >= Duration::from_secs(15) {
            return Err(invalid("dependency witness deadline exceeded"));
        }
        Ok(())
    }
    fn walk(&mut self, root: &Path, path: &Path, depth: usize) -> io::Result<()> {
        self.deadline()?;
        if depth > 32 {
            return Err(invalid("dependency directory depth exceeds32"));
        }
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err(invalid("dependency tree exceeds65536entries"));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| invalid("dependency path escaped root"))?
            .to_str()
            .ok_or_else(|| invalid("dependency paths must be UTF8"))?;
        if relative.len() > 4096 {
            return Err(invalid("dependency relative path exceeds bound"));
        }
        let before = fs::symlink_metadata(path)?;
        if before.file_type().is_symlink() {
            return Err(invalid("dependency symlinks forbidden"));
        }
        self.hash.update((relative.len() as u64).to_le_bytes());
        self.hash.update(relative.as_bytes());
        if before.is_dir() {
            self.hash.update(b"directory\0");
            self.hash.update((before.mode() & 0o777).to_le_bytes());
            let mut children = Vec::new();
            for entry in fs::read_dir(path)? {
                self.deadline()?;
                children.push(entry?.file_name());
                if children.len() as u64 > MAX_ENTRIES {
                    return Err(invalid("dependency directory exceeds entry bound"));
                }
            }
            children.sort_unstable();
            for child in children {
                self.walk(root, &path.join(child), depth + 1)?;
            }
        } else if before.is_file() {
            if before.nlink() != 1 {
                return Err(invalid("dependency hard-linked files forbidden"));
            }
            self.bytes = self
                .bytes
                .checked_add(before.len())
                .ok_or_else(|| invalid("dependency byte count overflow"))?;
            if self.bytes > MAX_BYTES {
                return Err(invalid("dependency tree exceeds2GiB"));
            }
            let mut file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            if !unchanged(&before, &file.metadata()?) {
                return Err(invalid("dependency file changed before read"));
            }
            self.hash.update(b"file\0");
            self.hash.update(before.len().to_le_bytes());
            self.hash.update((before.mode() & 0o777).to_le_bytes());
            let mut buffer = [0; 65536];
            let mut read_bytes = 0u64;
            loop {
                self.deadline()?;
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                read_bytes += count as u64;
                if read_bytes > before.len() {
                    return Err(invalid("dependency file grew during read"));
                }
                self.hash.update(&buffer[..count]);
            }
            if read_bytes != before.len() || !unchanged(&before, &file.metadata()?) {
                return Err(invalid("dependency file changed during read"));
            }
        } else {
            return Err(invalid("dependency special files forbidden"));
        }
        if !unchanged(&before, &fs::symlink_metadata(path)?) {
            return Err(invalid("dependency entry changed during witness"));
        }
        Ok(())
    }
}
pub(super) fn inspect(root: &Path) -> io::Result<serde_json::Value> {
    if !root.is_absolute()
        || fs::canonicalize(root)? != root
        || !fs::symlink_metadata(root)?.is_dir()
    {
        return Err(invalid("canonical absolute dependency directory required"));
    }
    let before = fs::symlink_metadata(root)?;
    let mut scan = Scan {
        hash: Sha256::new(),
        entries: 0,
        bytes: 0,
        started: Instant::now(),
    };
    scan.hash.update(b"minecraft-runtime-dependency-tree-v1\0");
    scan.walk(root, root, 0)?;
    scan.deadline()?;
    if !unchanged(&before, &fs::symlink_metadata(root)?) {
        return Err(invalid("dependency root changed during witness"));
    }
    Ok(
        serde_json::json!({"format":"minecraft-runtime-dependency-witness-v1","complete":true,
        "path":root,"sha256":format!("{:x}",scan.hash.finalize()),"root_device":before.dev(),"root_inode":before.ino(),
        "entries":scan.entries,"bytes":scan.bytes,"elapsed_ms":scan.started.elapsed().as_millis(),
        "scope":"Read-only bounded tree snapshot; no execution, argv/environment or gameplay calls. Symlinks, hardlinks and special files rejected. Not continuous host immutability or content certification."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_digest_is_path_independent_and_detects_content_and_membership_changes() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let a = fs::canonicalize(first.path()).unwrap();
        let b = fs::canonicalize(second.path()).unwrap();
        for root in [&a, &b] {
            fs::create_dir(root.join("crate-a")).unwrap();
            fs::write(root.join("crate-a/lib.rs"), b"reviewed dependency").unwrap();
        }
        let original = inspect(&a).unwrap();
        assert_eq!(original["sha256"], inspect(&b).unwrap()["sha256"]);
        fs::write(a.join("crate-a/lib.rs"), b"modified dependency").unwrap();
        assert_ne!(original["sha256"], inspect(&a).unwrap()["sha256"]);
        fs::write(a.join("crate-a/lib.rs"), b"reviewed dependency").unwrap();
        fs::write(a.join("new-source.rs"), b"preserve me").unwrap();
        assert_ne!(original["sha256"], inspect(&a).unwrap()["sha256"]);
    }
    #[test]
    fn symlinks_and_expired_scans_are_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        std::os::unix::fs::symlink("missing", root.join("alias")).unwrap();
        assert!(inspect(&root).is_err());
        let mut scan = Scan {
            hash: Sha256::new(),
            entries: 0,
            bytes: 0,
            started: Instant::now() - Duration::from_secs(16),
        };
        assert!(scan.walk(&root, &root, 0).is_err());
    }
    #[test]
    fn hardlinks_are_not_a_readonly_escape_and_file_mode_changes_affect_pin() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let path = root.join("dependency.rs");
        fs::write(&path, b"reviewed dependency").unwrap();
        let before = inspect(&root).unwrap();
        let mode = fs::metadata(&path).unwrap().mode();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode ^ 0o100)).unwrap();
        assert_ne!(before["sha256"], inspect(&root).unwrap()["sha256"]);
        fs::hard_link(&path, root.join("alias.rs")).unwrap();
        assert!(inspect(&root).is_err());
    }
}
