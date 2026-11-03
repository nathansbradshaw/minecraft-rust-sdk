//! Local process coordination, not remote authentication or an untrusted-code boundary.
//! SDK child programs share their harness session; only Brain owners take this lease.
use crate::{Profile, Result, digest, ensure};
use std::{
    fs,
    io::ErrorKind,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

fn name(profile: &Profile) -> String {
    // These are the actual authenticated request scope. Key rotation, copied
    // profile paths, a changed UUID hint and another state directory do not
    // create another owner for the same bot's session.
    format!(
        "{}.lock",
        digest(
            serde_json::json!({
                "instance":profile.instance_id,"bot":profile.credential.bot_id
            })
            .to_string()
            .as_bytes()
        )
    )
}

pub(super) fn acquire(profile: &Profile, root: &Path) -> Result<fs::File> {
    match fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let directory = fs::symlink_metadata(root)?;
    ensure(
        directory.is_dir()
            && directory.uid() == unsafe { libc::geteuid() }
            && directory.mode() & 0o077 == 0,
        "Brain ownership directory must be private and owned by this user",
    )?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(name(profile)))?;
    let metadata = file.metadata()?;
    ensure(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "Brain ownership lease must be a private owned regular file",
    )?;
    ensure(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "Another brain owns this character; keep its current body and connection running",
    )?;
    // Rust's file descriptor is close-on-exec. A compiler/terminal child must
    // never extend this ownership after the Brain exits.
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minecraft_sdk::Credential;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn profile() -> Profile {
        Profile {
            endpoint: "https://example.invalid/".into(),
            instance_id: "instance".into(),
            credential: Credential {
                bot_id: "own".into(),
                player_uuid: "actor".into(),
                key: "private-fixture-key".into(),
            },
            session: None,
        }
    }
    #[test]
    fn actor_ownership_survives_profile_copies_and_releases_for_later_startup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("actors");
        let p = profile();
        let owner = acquire(&p, &root).unwrap();
        let mut copied = p.clone();
        copied.credential.key = "rotated-key".into();
        copied.credential.player_uuid = "changed-hint".into();
        copied.endpoint = "https://another.invalid/".into();
        assert!(
            acquire(&copied, &root)
                .unwrap_err()
                .to_string()
                .contains("Another brain")
        );
        assert!(unsafe { libc::fcntl(owner.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
        copied.credential.bot_id = "other".into();
        let other_actor = acquire(&copied, &root).unwrap();
        copied.credential.bot_id = "own".into();
        copied.instance_id = "different-instance".into();
        let other_instance = acquire(&copied, &root).unwrap();
        drop((other_actor, other_instance, owner));
        assert!(acquire(&p, &root).is_ok());
        assert!(!name(&p).contains(&p.credential.key));
    }
    #[test]
    fn ownership_refuses_shared_paths_and_symlinks_without_touching_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("actors");
        let p = profile();
        drop(acquire(&p, &root).unwrap());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(acquire(&p, &root).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join(name(&p));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(acquire(&p, &root).is_err());
        fs::remove_file(&path).unwrap();
        let target = dir.path().join("preserve");
        fs::write(&target, b"preserve unrelated data").unwrap();
        symlink(&target, &path).unwrap();
        assert!(acquire(&p, &root).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"preserve unrelated data");
    }
}
