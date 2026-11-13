use super::*;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

pub fn ensure(ok: bool, message: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
pub fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    ensure(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0
            && meta.len() <= limit,
        "private bounded regular file required",
    )?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure(bytes.len() as u64 <= limit, "file grew beyond budget")?;
    Ok(bytes)
}
pub fn json(path: &Path, limit: u64) -> Result<Value> {
    Ok(serde_json::from_slice(&read(path, limit)?)?)
}
pub fn create(path: &Path) -> Result<fs::File> {
    Ok(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}
pub fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = create(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}
pub fn directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "private operator-owned directory required",
    )
}
pub fn canonical(path: &Path) -> Result<PathBuf> {
    ensure(path.is_absolute(), "absolute path required")?;
    let full = fs::canonicalize(path)?;
    ensure(full == path, "canonical nonsymlink path required")?;
    Ok(full)
}
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    ensure(file.metadata()?.is_file(), "regular artifact required")?;
    let mut digest = sha2::Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn free_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    ensure(
        unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } == 0,
        "disk measurement failed",
    )?;
    let stat = unsafe { stat.assume_init() };
    // libc's statvfs integer widths vary across supported Unix targets.
    #[allow(clippy::unnecessary_cast)]
    let bytes = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
    Ok(bytes)
}
fn replace_private_json(path: &Path, value: &Value) -> Result<()> {
    canonical(path)?;
    read(path, 16384)?;
    let temporary = path.with_extension("bench-session.new");
    let mut out = create(&temporary)?;
    let result = (|| -> Result<()> {
        out.write_all(&serde_json::to_vec(value)?)?;
        out.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(path.parent().ok_or("profile parent absent")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
/// Keep each endpoint/credential unchanged; synchronize only the session field.
pub fn sync_session(f: &Fixture, clear: bool) -> Result<()> {
    let mut host = json(f.host_profile()?, 16384)?;
    let mut internal = json(&f.profile(), 16384)?;
    ensure(
        host["instance_id"] == f.instance_id
            && host["credential"] == internal["credential"]
            && internal["instance_id"] == f.instance_id,
        "profile identity differs during session sync",
    )?;
    internal["session"] = if clear {
        Value::Null
    } else {
        host["session"].clone()
    };
    replace_private_json(&f.profile(), &internal)?;
    if clear {
        host["session"] = Value::Null;
        replace_private_json(f.host_profile()?, &host)?;
    }
    Ok(())
}
pub fn utc_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}
pub fn valid_hash(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
pub fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
/// Credential values are never copied into reports. Redaction covers private child output too.
pub struct Secrets(pub Vec<String>);
impl Secrets {
    pub fn scrub(&self, bytes: &[u8]) -> Vec<u8> {
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        for secret in &self.0 {
            if !secret.is_empty() {
                text = text.replace(secret, "[REDACTED]");
            }
        }
        text.into_bytes()
    }
}
