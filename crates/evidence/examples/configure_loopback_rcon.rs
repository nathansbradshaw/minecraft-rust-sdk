//! Operator-only preparation of an existing loopback Minecraft server.
//! Requires an exact saved properties backup and a private, preallocated secret.
//! No server process control, bot mounting, password output or network listener.
use std::{
    fs,
    io::Write,
    net::IpAddr,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn render(input: &str, secret: &str) -> Result<String> {
    if input
        .bytes()
        .any(|b| b != b'\n' && !(b' '..=b'~').contains(&b))
    {
        return Err("Only LF-separated printable ASCII properties are supported".into());
    }
    if secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Secret must be exactly64 hexadecimal characters".into());
    }
    let mut counts = [0; 4];
    let mut output = String::new();
    for line in input.lines() {
        let active = line.trim_start_matches(' ');
        if active.is_empty() || active.starts_with(['#', '!']) {
            output.push_str(line);
            output.push('\n');
            continue;
        }
        // Java Properties also accepts separators, escapes and continuations.
        // This deliberately supports only the canonical generated subset;
        // accepting a second Java interpretation could change the bind address.
        let (key, value) = active
            .split_once('=')
            .ok_or("Only canonical key=value properties are supported")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            || active.contains('\\')
        {
            return Err("Escaped, continued or alternate property syntax is unsupported".into());
        }
        let replacement = match key {
            "enable-rcon" => {
                counts[0] += 1;
                Some("enable-rcon=true".to_owned())
            }
            "rcon.password" => {
                counts[1] += 1;
                Some(format!("rcon.password={secret}"))
            }
            "rcon.port" => {
                counts[2] += 1;
                if value.trim().parse::<u16>().ok().is_none_or(|v| v == 0) {
                    return Err("Invalid existing RCON port".into());
                }
                None
            }
            "server-ip" => {
                counts[3] += 1;
                if value
                    .trim()
                    .parse::<IpAddr>()
                    .ok()
                    .is_none_or(|v| !v.is_loopback())
                {
                    return Err("Explicit numeric loopback server-ip required".into());
                }
                None
            }
            _ => None,
        };
        output.push_str(replacement.as_deref().unwrap_or(line));
        output.push('\n');
    }
    if counts != [1; 4] {
        return Err("Required properties must each exist exactly once".into());
    }
    Ok(output)
}
fn configure(properties: &Path, backup: &Path, key: &Path) -> Result<()> {
    for path in [properties, backup, key] {
        if !fs::symlink_metadata(path)?.file_type().is_file() {
            return Err("Regular non-symlink input files required".into());
        }
    }
    let properties_meta = fs::metadata(properties)?;
    let backup_meta = fs::metadata(backup)?;
    if properties_meta.dev() == backup_meta.dev() && properties_meta.ino() == backup_meta.ino() {
        return Err(
            "Preserved backup must be a distinct file, not a path or hardlink alias".into(),
        );
    }
    let properties = fs::canonicalize(properties)?;
    let parent = properties.parent().ok_or("Properties parent required")?;
    if fs::metadata(key)?.permissions().mode() & 0o077 != 0 {
        return Err("Operator secret must be private".into());
    }
    let original = fs::read(&properties)?;
    if original != fs::read(backup)? {
        return Err("Properties differ from the preserved backup; refusing modification".into());
    }
    let raw = fs::read_to_string(key)?;
    let secret = raw.trim_end_matches(['\r', '\n']);
    let changed = render(std::str::from_utf8(&original)?, secret)?;
    let tmp = properties.with_extension(format!("rcon-next-{}", std::process::id()));
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let saved = (|| -> Result<()> {
        file.write_all(changed.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &properties)?;
        fs::File::open(parent).and_then(|directory| directory.sync_all()).map_err(|_| {
            "Properties were replaced; directory durability is uncertain. Inspect before any retry."
        })?;
        Ok(())
    })();
    if saved.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    saved
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("Usage: configure_loopback_rcon PROPERTIES EXACT_BACKUP PRIVATE_SECRET_FILE; stop/save server first".into());
    }
    configure(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
    )?;
    println!(
        "Configured existing loopback RCON; secret remains private; saved server restart required."
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn props() -> &'static str {
        "# keep\nonline-mode=true\nmax-players=10\nenable-rcon=false\nrcon.password=\nrcon.port=25575\nserver-ip=127.0.0.1\n"
    }
    #[test]
    fn preserve_unrelated_properties() {
        let value = render(props(), &"a".repeat(64)).unwrap();
        assert!(value.contains("# keep\nonline-mode=true\nmax-players=10\n"));
        assert!(value.contains("enable-rcon=true\n"));
        assert!(value.ends_with("server-ip=127.0.0.1\n"));
    }
    #[test]
    fn reject_missing_duplicate_or_public_bind() {
        for value in [
            props().replace("server-ip=127.0.0.1", "server-ip=0.0.0.0"),
            format!("{}enable-rcon=true\n", props()),
            props().replace("rcon.port=25575\n", ""),
        ] {
            assert!(render(&value, &"a".repeat(64)).is_err());
        }
        assert!(render(props(), "unsafe\ncommand").is_err());
    }
    #[test]
    fn reject_java_alternate_interpretations() {
        for suffix in [
            "server-ip:0.0.0.0\n",
            "server-ip 0.0.0.0\n",
            "server\\-ip=0.0.0.0\n",
            "unused=\\\nserver-ip=0.0.0.0\n",
            "server-ip\\u003d0.0.0.0\n",
            "foo=x\rserver-ip:0.0.0.0\n",
            "\u{000b}server-ip=0.0.0.0\n",
            "\u{00a0}server-ip=0.0.0.0\n",
        ] {
            assert!(render(&format!("{}{suffix}", props()), &"a".repeat(64)).is_err());
        }
        assert!(
            render(
                &props().replace("server-ip=", "\u{00a0}server-ip="),
                &"a".repeat(64)
            )
            .is_err()
        );
    }
    #[test]
    fn invalid_configuration_never_overwrites_original() {
        let dir = std::env::temp_dir().join(format!("mc-rcon-config-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("server.properties");
        let backup = dir.join("backup");
        let key = dir.join("secret");
        fs::write(&path, props()).unwrap();
        fs::write(&backup, props()).unwrap();
        fs::write(&key, "invalid\n").unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(configure(&path, &backup, &key).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), props());
        fs::write(&key, format!("{}\n", "a".repeat(64))).unwrap();
        assert!(configure(&path, &path, &key).is_err());
        let alias = dir.join("backup-alias");
        fs::hard_link(&path, &alias).unwrap();
        assert!(configure(&path, &alias, &key).is_err());
        fs::remove_file(alias).unwrap();
        configure(&path, &backup, &key).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let after = fs::read(&path).unwrap();
        assert!(configure(&path, &backup, &key).is_err());
        assert_eq!(after, fs::read(&path).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
