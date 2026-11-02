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

#[cfg(test)]
mod tests {
    use super::*;
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
