//! One bounded /proc scan. Never read command lines, environments or credentials.
use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

fn stat(pid: u32) -> io::Result<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    if text.len() > 16384
        || text
            .split_once('(')
            .and_then(|(p, _)| p.trim().parse::<u32>().ok())
            != Some(pid)
    {
        return Err(invalid("invalid process stat identity"));
    }
    text.rsplit_once(") ")
        .and_then(|(_, tail)| tail.split_whitespace().nth(19))
        .ok_or_else(|| invalid("process start time absent"))?
        .parse()
        .map_err(|_| invalid("invalid process start time"))
}
pub(super) fn inspect(expected: &Path) -> io::Result<serde_json::Value> {
    let started = Instant::now();
    if !expected.is_absolute() {
        return Err(invalid("absolute executable required"));
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(expected)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
        return Err(invalid("bounded regular executable required"));
    }
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    if magic != *b"\x7fELF" {
        return Err(invalid("ELF executable required"));
    }
    let mut hash = Sha256::new();
    hash.update(magic);
    let mut buffer = [0; 65536];
    loop {
        if started.elapsed() >= Duration::from_secs(2) {
            return Err(invalid("process witness deadline exceeded"));
        }
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        {
            pids.push(pid);
            if pids.len() > 64 {
                return Err(invalid("process witness inventory exceeds64PIDs"));
            }
        }
    }
    pids.sort_unstable();
    let uid = unsafe { libc::geteuid() };
    let mut records = Vec::new();
    let mut matches = Vec::new();
    let mut complete = true;
    for pid in pids {
        if started.elapsed() >= Duration::from_secs(2) {
            return Err(invalid("process witness deadline exceeded"));
        }
        let result = (|| -> io::Result<Option<(u64, PathBuf)>> {
            if fs::metadata(format!("/proc/{pid}"))?.uid() != uid {
                return Ok(None);
            }
            let before = stat(pid)?;
            let path = PathBuf::from(format!("/proc/{pid}/exe"));
            let target = fs::metadata(&path)?;
            if target.dev() != metadata.dev() || target.ino() != metadata.ino() {
                return Ok(None);
            }
            let resolved = fs::read_link(&path)?;
            if stat(pid)? != before {
                return Err(invalid("process identity changed during witness"));
            }
            Ok(Some((before, resolved)))
        })();
        match result {
            Ok(Some((start, path))) => {
                matches.push(json!({"pid":pid,"start_jiffies":start,"exe_path":path,"uid":uid}));
                records.push(json!({"pid":pid,"status":"matched"}));
            }
            Ok(None) => records.push(json!({"pid":pid,"status":"other_executable_or_uid"})),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                records.push(json!({"pid":pid,"status":"gone"}))
            }
            Err(error) => {
                complete = false;
                records.push(
                    json!({"pid":pid,"status":"unreadable_or_changed","error":error.to_string()}),
                );
            }
        }
    }
    let after = file.metadata()?;
    if after.len() != metadata.len()
        || after.dev() != metadata.dev()
        || after.ino() != metadata.ino()
        || after.mtime() != metadata.mtime()
        || after.mtime_nsec() != metadata.mtime_nsec()
        || after.ctime() != metadata.ctime()
        || after.ctime_nsec() != metadata.ctime_nsec()
    {
        return Err(invalid("expected executable changed during witness"));
    }
    Ok(
        json!({"format":"minecraft-runtime-process-witness-v1","complete":complete,
        "uid":uid,"executable":{"path":expected,"sha256":format!("{:x}",hash.finalize()),"device":metadata.dev(),"inode":metadata.ino()},
        "matches":matches,"processes":records,"elapsed_ms":started.elapsed().as_millis(),
        "scope":"Same-UID container process/file identity snapshot only; no argv/environment or gameplay calls"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_executable_matches_real_pid_and_start_without_command_line_data() {
        let expected = std::env::current_exe().unwrap();
        let value = inspect(&expected).unwrap();
        assert_eq!(value["complete"], true);
        assert!(value["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["pid"] == std::process::id()));
        assert!(value["executable"]["sha256"].as_str().unwrap().len() == 64);
        assert!(!value.to_string().contains("cmdline"));
    }
    #[test]
    fn non_executable_data_is_not_hashed_as_a_process_target() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), b"private fixture data").unwrap();
        assert!(inspect(file.path()).is_err());
    }
}
