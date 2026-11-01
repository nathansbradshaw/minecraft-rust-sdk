use serde::Deserialize;
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    process::Command,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProxyConfig {
    listen: SocketAddr,
    allow: Vec<String>,
    #[serde(default)]
    sdk_exception: Option<SocketAddr>,
}
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || b == 51 && c == 100))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            ip.to_ipv4_mapped()
                .map(|v| public(IpAddr::V4(v)))
                .unwrap_or_else(|| {
                    segments[0] & 0xe000 == 0x2000
                        && !(segments[0] == 0x2001
                            && (segments[1] <= 0x1ff || segments[1] == 0xdb8))
                        && segments[0] != 0x2002
                })
        }
    }
}
async fn proxy(config: ProxyConfig) -> Result<()> {
    let listener = TcpListener::bind(config.listen).await?;
    let config = std::sync::Arc::new(config);
    let budget = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = budget.clone().try_acquire_owned() else {
            continue;
        };
        let config = config.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(Duration::from_secs(600), tunnel(stream, &config)).await;
        });
    }
}
async fn tunnel(mut client: TcpStream, config: &ProxyConfig) -> Result<()> {
    let mut header = Vec::new();
    loop {
        let byte = tokio::time::timeout(Duration::from_secs(5), client.read_u8()).await??;
        header.push(byte);
        if header.len() > 8192 {
            return Err("Proxy header too large".into());
        }
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = std::str::from_utf8(&header)?;
    let line = text.lines().next().ok_or("Proxy request missing")?;
    let parts: Vec<_> = line.split(' ').collect();
    if parts.len() != 3 || parts[0] != "CONNECT" || parts[2] != "HTTP/1.1" {
        client
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }
    let authority = parts[1].to_ascii_lowercase();
    let allowed: HashSet<_> = config
        .allow
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if !allowed.contains(&authority) || authority.contains(['@', '/', '\\', '%']) {
        client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }
    let (host, port) = authority.rsplit_once(':').ok_or("Port required")?;
    let port: u16 = port.parse()?;
    // Resolve once, validate every answer, then connect the exact checked numeric address.
    let addresses: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host, port)),
    )
    .await??
    .collect();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|a| !public(a.ip()) && Some(*a) != config.sdk_exception)
    {
        client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }
    let mut remote =
        tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addresses[0])).await??;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    tokio::io::copy_bidirectional(&mut client, &mut remote).await?;
    Ok(())
}
fn run(program: &str, args: &[&str]) -> Result<()> {
    if !Command::new(program).args(args).status()?.success() {
        return Err(format!("{program} failed").into());
    }
    Ok(())
}
fn init_host(manager: Option<&str>) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("Run init-host as root; it creates only the dedicated runner account and its user manager".into());
    }
    if !Command::new("id")
        .args(["-u", "mc-bots"])
        .output()?
        .status
        .success()
    {
        run(
            "useradd",
            &[
                "--create-home",
                "--home-dir",
                "/srv/mc-bots",
                "--shell",
                "/bin/bash",
                "mc-bots",
            ],
        )?;
    }
    let passwd = String::from_utf8(
        Command::new("getent")
            .args(["passwd", "mc-bots"])
            .output()?
            .stdout,
    )?;
    let fields: Vec<_> = passwd.trim().split(':').collect();
    if fields.len() != 7 || fields[5] != "/srv/mc-bots" || fields[2] == "0" {
        return Err("Existing mc-bots identity does not match the dedicated runner layout".into());
    }
    let groups = String::from_utf8(Command::new("id").args(["-nG", "mc-bots"]).output()?.stdout)?;
    if groups.split_whitespace().any(|g| g != "mc-bots") {
        return Err("mc-bots must have no additional groups".into());
    }
    let uid = fields[2];
    for file in ["/etc/subuid", "/etc/subgid"] {
        let contents = std::fs::read_to_string(file)?;
        if !contents.lines().any(|line| {
            line.starts_with("mc-bots:")
                && line
                    .rsplit(':')
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_some_and(|n| n >= 65536)
        }) {
            return Err("Assign mc-bots an unused range of 65536 subordinate UIDs/GIDs with usermod before retrying".into());
        }
    }
    if let Some(manager) = manager {
        if manager.is_empty()
            || !manager
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || !Command::new("id")
                .args(["-u", manager])
                .output()?
                .status
                .success()
        {
            return Err("Existing non-root manager username required".into());
        }
        if manager == "root" || manager == "mc-bots" {
            return Err("Choose the existing personal administrator account".into());
        }
        let contents = format!("{manager} ALL=(mc-bots) NOPASSWD: ALL\n");
        let path = std::path::Path::new("/etc/sudoers.d/minecraft-sdk-runner");
        if path.exists() {
            if std::fs::read_to_string(path)? != contents {
                return Err("Existing runner sudo rule differs; review it manually".into());
            }
        } else {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let temporary = path.with_extension("candidate");
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o440)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            run(
                "visudo",
                &["-cf", temporary.to_str().ok_or("Invalid sudoers path")?],
            )?;
            std::fs::rename(temporary, path)?;
        }
    }
    run("chmod", &["0700", "/srv/mc-bots"])?;
    let slice = format!("user-{uid}.slice");
    run(
        "systemctl",
        &[
            "set-property",
            &slice,
            "MemoryMax=3G",
            "MemorySwapMax=0",
            "CPUQuota=200%",
            "TasksMax=2048",
        ],
    )?;
    run("loginctl", &["enable-linger", "mc-bots"])?;
    run("systemctl", &["start", &format!("user@{uid}.service")])?;
    let runtime = format!("XDG_RUNTIME_DIR=/run/user/{uid}");
    let bus = format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus");
    run(
        "runuser",
        &[
            "-u",
            "mc-bots",
            "--",
            "env",
            "HOME=/srv/mc-bots",
            &runtime,
            &bus,
            "dockerd-rootless-setuptool.sh",
            "install",
            "--force",
        ],
    )?;
    println!(
        "Rootless Docker installed for mc-bots (UID {uid}). Existing rootful Docker is unchanged. No bot was started."
    );
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    name: String,
    image: String,
    proxy_image: String,
    proxy_config: std::path::PathBuf,
    memory_mb: u32,
    cpus: f32,
    pids: u32,
    workspace_mb: u32,
    cache_mb: u32,
    log_mb: u32,
}
fn launch(config: Launch) -> Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        return Err("Run as the dedicated mc-bots user, never root".into());
    }
    let user = String::from_utf8(Command::new("id").args(["-un"]).output()?.stdout)?;
    if user.trim() != "mc-bots" {
        return Err("Dedicated mc-bots account required".into());
    }
    if !config
        .name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || config.name.is_empty()
        || config.name.len() > 32
    {
        return Err("Invalid bot deployment name".into());
    }
    if !pinned_image(&config.image) || !pinned_image(&config.proxy_image) {
        return Err("Pin reviewed container images by digest".into());
    }
    if config.memory_mb < 256
        || config.workspace_mb + config.cache_mb > config.memory_mb
        || !config.cpus.is_finite()
        || config.cpus <= 0.
        || config.pids == 0
        || config.log_mb == 0
    {
        return Err(
            "Explicit nonzero CPU, memory, process, storage and log budgets required".into(),
        );
    }
    let info = Command::new("docker")
        .args(["info", "--format", "{{json .}} "])
        .output()?;
    if !info.status.success() {
        return Err("Rootless Docker unavailable".into());
    }
    let info: serde_json::Value = serde_json::from_slice(&info.stdout)?;
    if !info["SecurityOptions"].as_array().is_some_and(|a| {
        a.iter()
            .any(|s| s.as_str().is_some_and(|s| s.contains("rootless")))
    }) || info["CgroupVersion"] != "2"
        || info["CgroupDriver"] == "none"
    {
        return Err("Rootless Docker with cgroup v2 resource delegation required".into());
    }
    let network = format!("sdk-{}", config.name);
    let proxy_name = format!("{}-egress", config.name);
    // One internal network per bot prevents direct access to other bot workspaces/services.
    run("docker", &["network", "create", "--internal", &network])?;
    let proxy_config = std::fs::canonicalize(&config.proxy_config)?;
    if !proxy_config.is_file() {
        return Err("Proxy configuration must be a regular operator-owned file".into());
    }
    let mount = format!(
        "type=bind,src={},dst=/etc/egress.json,readonly",
        proxy_config.display()
    );
    run(
        "docker",
        &[
            "create",
            "--name",
            &proxy_name,
            "--network",
            "bridge",
            "--read-only",
            "--user",
            "1000:1000",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--pids-limit",
            "64",
            "--memory",
            "128m",
            "--memory-swap",
            "128m",
            "--cpus",
            "0.25",
            "--log-opt",
            "max-size=5m",
            "--log-opt",
            "max-file=2",
            "--mount",
            &mount,
            &config.proxy_image,
            "proxy",
            "/etc/egress.json",
        ],
    )?;
    run(
        "docker",
        &[
            "network",
            "connect",
            "--alias",
            "egress",
            &network,
            &proxy_name,
        ],
    )?;
    run("docker", &["start", &proxy_name])?;
    let memory = format!("{}m", config.memory_mb);
    let cpus = config.cpus.to_string();
    let pids = config.pids.to_string();
    let workspace = format!(
        "/workspace:rw,nosuid,nodev,exec,size={}m,uid=1000,gid=1000,mode=0700",
        config.workspace_mb
    );
    let cache = format!(
        "/cache:rw,nosuid,nodev,exec,size={}m,uid=1000,gid=1000,mode=0700",
        config.cache_mb
    );
    let logs = format!("max-size={}m", config.log_mb);
    run(
        "docker",
        &[
            "run",
            "--detach",
            "--name",
            &config.name,
            "--network",
            &network,
            "--dns",
            "127.0.0.1",
            "--read-only",
            "--user",
            "1000:1000",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--pids-limit",
            &pids,
            "--memory",
            &memory,
            "--memory-swap",
            &memory,
            "--cpus",
            &cpus,
            "--tmpfs",
            &workspace,
            "--tmpfs",
            &cache,
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,noexec,size=32m,mode=1777",
            "--log-opt",
            &logs,
            "--log-opt",
            "max-file=2",
            "--env",
            "HTTPS_PROXY=http://egress:8080",
            "--env",
            "HTTP_PROXY=http://egress:8080",
            "--env",
            "ALL_PROXY=http://egress:8080",
            "--env",
            "CARGO_HOME=/cache/cargo",
            "--env",
            "HOME=/workspace",
            "--workdir",
            "/workspace",
            &config.image,
        ],
    )?;
    println!(
        "Started {}. Workspace/cache are bounded tmpfs; export source before removing the container.",
        config.name
    );
    Ok(())
}
fn pinned_image(value: &str) -> bool {
    let digest = value
        .strip_prefix("sha256:")
        .or_else(|| value.rsplit_once("@sha256:").map(|(_, d)| d));
    !value.starts_with('-')
        && !value.chars().any(char::is_whitespace)
        && digest.is_some_and(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn checkpoint(name: &str, restore: bool, max_mib: u64) -> Result<()> {
    use std::{
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        process::Stdio,
    };
    let user = String::from_utf8(Command::new("id").arg("-un").output()?.stdout)?;
    if user.trim() != "mc-bots"
        || name.is_empty()
        || name.len() > 32
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || !(1..=2048).contains(&max_mib)
    {
        return Err(
            "Dedicated runner, valid bot name and 1–2048 MiB checkpoint budget required".into(),
        );
    }
    let inspection = Command::new("docker")
        .args(["inspect", name, "--format", "{{json .HostConfig}} "])
        .output()?;
    if !inspection.status.success() {
        return Err("Container unavailable".into());
    }
    let host: serde_json::Value = serde_json::from_slice(&inspection.stdout)?;
    if host["ReadonlyRootfs"] != true
        || !host["Binds"].is_null()
        || host["Tmpfs"]["/workspace"].is_null()
    {
        return Err(
            "Checkpointing requires an isolated coding container with a bounded workspace".into(),
        );
    }
    let root = std::path::Path::new("/srv/mc-bots/checkpoints");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    let path = root.join(format!("{name}.tar"));
    let bound = max_mib * 1024 * 1024;
    if restore {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        if file.metadata()?.len() > bound {
            return Err("Checkpoint exceeds configured budget".into());
        }
        let status = Command::new("docker")
            .args([
                "exec",
                "-i",
                "--user",
                "1000:1000",
                name,
                "/usr/bin/tar",
                "--no-same-owner",
                "--no-same-permissions",
                "-xf",
                "-",
                "-C",
                "/workspace",
            ])
            .stdin(file)
            .status()?;
        if !status.success() {
            return Err(
                "Workspace restore failed; inspect the disposable container before using it".into(),
            );
        }
    } else {
        let temporary = path.with_extension("candidate");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        // Docker's archive API sees the image filesystem, not live tmpfs data.
        // Read using the immutable archiver inside the constrained container.
        // Invoke while the bot program is idle; changed files make tar fail.
        let mut child = Command::new("timeout")
            .args([
                "30s",
                "docker",
                "exec",
                "--user",
                "1000:1000",
                name,
                "/usr/bin/tar",
                "-cf",
                "-",
                "-C",
                "/workspace",
                ".",
            ])
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stream = child.stdout.take().ok_or("Checkpoint stream missing")?;
        let mut total = 0u64;
        let mut bytes = [0u8; 65536];
        loop {
            let n = stream.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > bound {
                let _ = child.kill();
                let _ = child.wait();
                drop(file);
                std::fs::remove_file(temporary)?;
                return Err("Checkpoint budget exceeded; previous snapshot preserved".into());
            }
            file.write_all(&bytes[..n])?;
        }
        if !child.wait()?.success() {
            drop(file);
            std::fs::remove_file(temporary)?;
            return Err("Checkpoint copy failed; previous snapshot preserved".into());
        }
        file.sync_all()?;
        if path.exists() {
            std::fs::rename(&path, path.with_extension("previous.tar"))?;
        }
        std::fs::rename(temporary, &path)?;
        std::fs::File::open(root)?.sync_all()?;
    }
    println!(
        "{} checkpoint for {name}",
        if restore { "Restored" } else { "Saved bounded" }
    );
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("init-host") => init_host(args.get(1).map(String::as_str)),
        Some("proxy") => {
            let config: ProxyConfig = serde_json::from_slice(&std::fs::read(
                args.get(1).ok_or("Proxy config required")?,
            )?)?;
            proxy(config).await
        }
        Some("launch") => {
            let config: Launch = serde_json::from_slice(&std::fs::read(
                args.get(1).ok_or("Launch config required")?,
            )?)?;
            launch(config)
        }
        Some("snapshot") | Some("restore") => checkpoint(
            args.get(1).ok_or("Bot name required")?,
            args[0] == "restore",
            args.get(2)
                .ok_or("Checkpoint MiB budget required")?
                .parse()?,
        ),
        _ => Err(
            "Usage: minecraft-sdk-runner init-host [MANAGER_USER] | proxy CONFIG | launch CONFIG"
                .into(),
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deny_local_lan_metadata_and_ipv6_transitions() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.139",
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "2002:7f00:1::",
            "2001::1",
            "2001:db8::1",
        ] {
            assert!(!public(s.parse().unwrap()), "{s}");
        }
        for s in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(public(s.parse().unwrap()), "{s}");
        }
    }
}
