//! Offline filesystem check; no SDK, credentials, models or gameplay calls.
use std::{
    fs, io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
#[path = "runtime_check/dependencies.rs"]
mod dependency_witness;
#[cfg(target_os = "linux")]
#[path = "runtime_check/process.rs"]
mod process_witness;

fn invalid(message: &'static str) -> io::Error {
    io::Error::other(message)
}

struct ProbeDirectory {
    path: PathBuf,
    cleanup: bool,
}
impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        // These exact entries are created by this invocation. Never recurse.
        if self.cleanup {
            let _ = fs::remove_file(self.path.join("actions.sock"));
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn check(state: &Path) -> io::Result<()> {
    if !state.exists() {
        let parent = state
            .parent()
            .ok_or_else(|| invalid("state parent required"))?;
        let name = state
            .file_name()
            .ok_or_else(|| invalid("state name required"))?;
        if !state.is_absolute() || fs::canonicalize(parent)?.join(name) != state {
            return Err(invalid("canonical existing state parent required"));
        }
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(state)?;
    }
    if !state.is_absolute() || fs::canonicalize(state)? != state {
        return Err(invalid("canonical absolute state directory required"));
    }
    let metadata = fs::symlink_metadata(state)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid(
            "private state directory owned by the effective user required",
        ));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("clock precedes epoch"))?
        .as_nanos();
    let path = state.join(format!(".mc-sp-{:x}-{nonce:x}", std::process::id()));
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    let mut probe = ProbeDirectory {
        path,
        cleanup: true,
    };
    let socket = probe.path.join("actions.sock");
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(&socket)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(invalid("private Unix socket metadata differs"));
    }
    drop(listener);
    fs::remove_file(&socket)?;
    fs::remove_dir(&probe.path)?;
    probe.cleanup = false;
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "dependency-witness" {
        match dependency_witness::inspect(Path::new(&args[1])) {
            Ok(value) => println!("{value}"),
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"format":"minecraft-runtime-dependency-witness-v1","complete":false,"error":error.to_string()})
                );
                std::process::exit(1);
            }
        }
        return;
    }
    if args.len() == 2 && args[0] == "process-witness" {
        #[cfg(target_os = "linux")]
        let result = process_witness::inspect(Path::new(&args[1]));
        #[cfg(not(target_os = "linux"))]
        let result: io::Result<serde_json::Value> = Err(invalid("process witness requires Linux"));
        match result {
            Ok(value) => println!("{value}"),
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"format":"minecraft-runtime-process-witness-v1","complete":false,"error":error.to_string()})
                );
                std::process::exit(1);
            }
        }
        return;
    }
    if args.len() != 1 {
        eprintln!("Usage: mc-sdk-runtime-check PRIVATE_STATE_DIR");
        std::process::exit(2);
    }
    match check(Path::new(&args[0])) {
        Ok(()) => println!(
            "{}",
            serde_json::json!({
                "format":"minecraft-runtime-filesystem-check-v1",
                "unix_socket_bind":true,"socket_mode_0600":true,
                "probe_removed":true,"sdk_or_gameplay_calls":false,
                "scope":"Offline filesystem capability only; not actor or survival evidence"
            })
        ),
        Err(error) => {
            // OS errors contain no input files or credentials.
            eprintln!("runtime filesystem check failed: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> tempfile::TempDir {
        let dir = tempfile::Builder::new()
            .prefix("mc-socket-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    #[test]
    fn private_socket_probe_removes_only_its_own_entries() {
        let dir = state();
        let path = fs::canonicalize(dir.path()).unwrap();
        fs::write(path.join("existing-journal"), b"preserve").unwrap();
        check(&path).unwrap();
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
        assert_eq!(
            fs::read(path.join("existing-journal")).unwrap(),
            b"preserve"
        );
    }
    #[test]
    fn missing_state_is_provisioned_private_without_creating_parent_paths() {
        let dir = state();
        let parent = fs::canonicalize(dir.path()).unwrap();
        let path = parent.join("runtime");
        check(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        assert!(check(&parent.join("absent").join("nested")).is_err());
        assert!(!parent.join("absent").exists());
    }
    #[test]
    fn public_or_symlinked_state_is_rejected_without_mutation() {
        let dir = state();
        let path = fs::canonicalize(dir.path()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check(&path).is_err());
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = path.join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(check(&alias).is_err());
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    }
}
