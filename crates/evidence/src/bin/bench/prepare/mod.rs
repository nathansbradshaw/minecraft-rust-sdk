//! Closed operator-only QA preparation. No API/agent entry point or shell interpreter.
use super::*;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::{fs::DirBuilderExt, process::CommandExt},
    process::{Child, Command, Stdio},
    time::Instant,
};
mod archive;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Role {
    Java,
    Gateway,
    Relay,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Java => "java",
            Self::Gateway => "gateway",
            Self::Relay => "relay",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    role: Role,
    tool: Pinned,
    cwd: PathBuf,
    args: Vec<String>,
    listen: Vec<SocketAddr>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Restore {
    file: Pinned,
    destination: PathBuf,
    roots: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Database {
    file: Pinned,
    destination: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Native {
    address: SocketAddr,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    version: u32,
    owner: String,
    root: PathBuf,
    marker: Pinned,
    prior_closure: Pinned,
    fixture: Pinned,
    bench_sha256: String,
    archives: Vec<Restore>,
    database: Database,
    services: Vec<Service>,
    native: Native,
    ready_timeout_secs: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PriorService {
    role: Role,
    pid: u32,
    generation: String,
    executable_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Closure {
    format: String,
    owner: String,
    root: PathBuf,
    players: u64,
    sessions: u64,
    services: Vec<PriorService>,
}

fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
fn present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure(
                !meta.file_type().is_symlink(),
                "restore aliases, including dangling symlinks, are forbidden",
            )?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn beneath(root: &Path, path: &Path) -> Result<()> {
    ensure(
        canonical(path)?.starts_with(root) && path != root,
        "path outside isolated QA root",
    )
}
fn endpoint_address(endpoint: &str) -> Result<SocketAddr> {
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or("explicit HTTP(S) operator endpoint required")?;
    ensure(
        matches!(scheme, "http" | "https") && !rest.contains(['@', '?', '#']),
        "canonical HTTP(S) endpoint without userinfo/query required",
    )?;
    let authority = rest
        .split('/')
        .next()
        .ok_or("operator endpoint authority absent")?;
    let address: SocketAddr = authority.parse()?;
    ensure(
        address.ip().is_loopback() && address.port() != 0 && authority == address.to_string(),
        "operator endpoint must use canonical loopback IP and explicit port",
    )?;
    Ok(address)
}
fn mkdir(path: &Path) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}
fn parents(root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(root)?;
    let mut p = root.to_path_buf();
    for c in relative.components() {
        p.push(c);
        if !p.exists() {
            mkdir(&p)?;
        }
        directory(&p)?;
        canonical(&p)?;
    }
    Ok(())
}
fn database(path: &Path, instance: &str) -> Result<()> {
    database_open(path, instance, false)
}
fn cold_database(path: &Path, instance: &str) -> Result<()> {
    cold_sidecars(path)?;
    database_open(path, instance, true)
}
fn cold_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        ensure(
            !present(&PathBuf::from(format!("{}{}", path.display(), suffix)))?,
            "stopped database must be checkpointed and standalone before inspection",
        )?;
    }
    Ok(())
}
fn database_open(path: &Path, instance: &str, immutable: bool) -> Result<()> {
    read(path, 64 * 1024 * 1024)?;
    let mut flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let location = if immutable {
        use std::os::unix::ffi::OsStrExt;
        let mut uri = String::from("file:");
        for byte in path.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || b"/._-".contains(byte) {
                uri.push(char::from(*byte));
            } else {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
        uri.push_str("?immutable=1");
        flags |= rusqlite::OpenFlags::SQLITE_OPEN_URI;
        PathBuf::from(uri)
    } else {
        path.to_path_buf()
    };
    let db = rusqlite::Connection::open_with_flags(location, flags)?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF")?;
    ensure(
        db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))? == "ok",
        "QA database integrity failed",
    )?;
    ensure(
        db.query_row("SELECT value FROM metadata WHERE key='instance'", [], |r| {
            r.get::<_, String>(0)
        })? == instance,
        "QA database instance differs",
    )?;
    ensure(
        db.query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))? == 0,
        "QA database has live or pending sessions",
    )
}
fn validate(s: &Spec, f: &Fixture) -> Result<Vec<PathBuf>> {
    ensure(
        s.version == 1 && id(&s.owner) && (1..=120).contains(&s.ready_timeout_secs),
        "invalid preparation version/owner/budget",
    )?;
    canonical(&s.root)?;
    directory(&s.root)?;
    ensure(
        s.root.components().count() >= 3
            && s.root
                != std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default(),
        "isolated private QA root required",
    )?;
    ensure(
        s.marker.path.parent() == Some(s.root.as_path()),
        "ownership marker must be at QA root",
    )?;
    s.marker.verify()?;
    let marker = json(&s.marker.path, 16384)?;
    ensure(
        marker["format"] == "minecraft-qa-root-v1"
            && marker["owner"] == s.owner
            && marker["isolated_qa"] == true
            && marker["production"] == false,
        "explicit isolated QA ownership marker required",
    )?;
    ensure(
        f.immediate_launch() && f.operator_rcon.is_some(),
        "prepare delegates only acquire/combine with typed RCON",
    )?;
    beneath(&s.root, &f.workspace)?;
    beneath(&s.root, &f.secret_dir)?;
    s.fixture.verify()?;
    s.prior_closure.verify()?;
    let prior: Closure = serde_json::from_slice(&read(&s.prior_closure.path, 16384)?)?;
    ensure(
        prior.format == "minecraft-qa-preparation-closure-v1"
            && prior.owner == s.owner
            && prior.root == s.root
            && prior.players == 0
            && prior.sessions == 0,
        "prior zero-player/session closure witness differs",
    )?;
    let mut roles = BTreeSet::new();
    let mut addresses = BTreeSet::new();
    for service in &s.services {
        ensure(
            roles.insert(service.role)
                && service.args.len() <= 32
                && service
                    .args
                    .iter()
                    .all(|a| a.len() <= 4096 && !a.contains(['\0', '\n', '\r']))
                && (1..=4).contains(&service.listen.len()),
            "invalid closed service declaration",
        )?;
        service.tool.verify()?;
        beneath(&s.root, &service.cwd)?;
        ensure(
            !service.cwd.starts_with(&f.workspace)
                && !service.cwd.starts_with(&f.secret_dir)
                && !service.tool.path.starts_with(&f.workspace)
                && !service.tool.path.starts_with(&f.secret_dir),
            "trusted services cannot execute from bot mounts",
        )?;
        let filename = service
            .tool
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        ensure(
            !matches!(
                filename,
                "sh" | "bash" | "zsh" | "env" | "python" | "python3" | "osascript"
            ),
            "service must be direct pinned executable, never an interpreter",
        )?;
        for address in &service.listen {
            ensure(
                address.ip().is_loopback() && address.port() != 0 && addresses.insert(*address),
                "unique loopback service listeners required",
            )?;
        }
    }
    ensure(
        roles.contains(&Role::Java)
            && roles.contains(&Role::Gateway)
            && roles.len() == s.services.len()
            && s.services.len() <= 3,
        "exact Java/gateway/optional relay roles required",
    )?;
    ensure(
        prior.services.len() == roles.len(),
        "all prior service generations must be declared closed",
    )?;
    let mut prior_roles = BTreeSet::new();
    for old in &prior.services {
        ensure(
            roles.contains(&old.role)
                && prior_roles.insert(old.role)
                && old.pid > 1
                && !old.generation.is_empty()
                && old.generation.len() <= 128
                && valid_hash(&old.executable_sha256, 64),
            "invalid prior service generation witness",
        )?;
        ensure(old.pid <= i32::MAX as u32, "invalid prior PID")?;
        let rc = unsafe { libc::kill(old.pid as i32, 0) };
        ensure(
            rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "prior service PID still exists or its absence is unknown; never signal it",
        )?;
    }
    ensure(
        s.native.address.ip().is_loopback()
            && s.services
                .iter()
                .any(|x| x.role == Role::Java && x.listen.contains(&s.native.address)),
        "native-hook listener readiness must bind Java listener",
    )?;
    let rcon = f.operator_rcon.as_ref().ok_or("operator RCON absent")?;
    ensure(
        s.services
            .iter()
            .any(|service| service.role == Role::Java && service.listen.contains(&rcon.address)),
        "RCON must bind the declared Java listener",
    )?;
    let profile = json(f.host_profile()?, 16384)?;
    let endpoint = endpoint_address(
        profile["endpoint"]
            .as_str()
            .ok_or("operator endpoint absent")?,
    )?;
    ensure(
        s.services.iter().any(|service| {
            matches!(service.role, Role::Gateway | Role::Relay)
                && service.listen.contains(&endpoint)
        }),
        "SDK discovery must bind the declared gateway or relay listener",
    )?;
    // Bind every assigned port at once before any restore/spawn. A foreign owner blocks.
    let _ports = addresses
        .iter()
        .map(TcpListener::bind)
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure(
        (1..=3).contains(&s.archives.len()),
        "bounded world/registry/aux archives required",
    )?;
    let mut paths = Vec::new();
    for restore in &s.archives {
        restore.file.verify()?;
        ensure(
            fs::metadata(&restore.file.path)?.len() <= 4 * 1024 * 1024 * 1024,
            "compressed archive exceeds bound",
        )?;
        ensure(
            relative(&restore.destination) && (1..=32).contains(&restore.roots.len()),
            "invalid archive restore destination",
        )?;
        for name in &restore.roots {
            ensure(
                relative(Path::new(name)) && Path::new(name).components().count() == 1,
                "archive roots must be single safe components",
            )?;
            paths.push(restore.destination.join(name));
        }
    }
    s.database.file.verify()?;
    ensure(
        relative(&s.database.destination),
        "invalid database destination",
    )?;
    paths.push(s.database.destination.clone());
    let mut set = BTreeSet::new();
    for p in &paths {
        ensure(set.insert(p.clone()), "duplicate restore path")?;
        for q in &paths {
            ensure(p == q || !p.starts_with(q), "overlapping restore roots")?;
        }
        let destination = s.root.join(p);
        ensure(
            !destination.starts_with(&f.workspace)
                && !destination.starts_with(&f.secret_dir)
                && !f.workspace.starts_with(&destination)
                && !f.secret_dir.starts_with(&destination),
            "restore paths cannot overlap bot mounts",
        )?;
        if present(&destination)? {
            canonical(&destination)?;
        }
        let parent = destination.parent().ok_or("restore parent absent")?;
        beneath(&s.root, parent)?;
    }
    let db = s.root.join(&s.database.destination);
    cold_sidecars(&db)?;
    if present(&db)? {
        cold_database(&db, &f.instance_id)?;
    }
    cold_database(&s.database.file.path, &f.instance_id)?;
    ensure(
        f.operator_gateway_journal.as_ref() == Some(&db),
        "fixture must bind restored operator gateway database",
    )?;
    ensure(
        s.archives.iter().all(|a| {
            !paths
                .iter()
                .any(|p| a.file.path.starts_with(s.root.join(p)))
        }) && !paths
            .iter()
            .any(|p| s.database.file.path.starts_with(s.root.join(p))),
        "snapshot cannot be inside a replaced tree",
    )?;
    Ok(paths)
}

struct Owned {
    role: Role,
    child: Child,
    handed_off: bool,
}
impl Owned {
    fn alive(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            return Err(format!("owned {} service exited: {status}", self.role.name()).into());
        }
        Ok(())
    }
    fn stop(&mut self) {
        if !self.handed_off && matches!(self.child.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                match self.child.try_wait() {
                    Ok(None) => {}
                    _ => return,
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.stop();
    }
}
fn native_ready(native: &Native) -> Result<()> {
    let mut stream = TcpStream::connect_timeout(&native.address, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(
        format!(
            "GET /v1/discover HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            native.address
        )
        .as_bytes(),
    )?;
    let mut bytes = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure(
            !remaining.is_zero(),
            "native listener read deadline expired",
        )?;
        stream.set_read_timeout(Some(remaining.min(Duration::from_secs(1))))?;
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        ensure(
            bytes.len() <= 65536,
            "native discover exceeds readiness bound",
        )?;
    }
    ensure(
        bytes.len() <= 65536,
        "native discover exceeds readiness bound",
    )?;
    let split = bytes
        .windows(4)
        .position(|x| x == b"\r\n\r\n")
        .ok_or("native discover header absent")?;
    ensure(
        [
            b"HTTP/1.1 200 ".as_slice(),
            b"HTTP/1.0 200 ",
            b"HTTP/1.1 401 ",
            b"HTTP/1.0 401 ",
        ]
        .iter()
        .any(|status| bytes.starts_with(status)),
        "native integration hook listener not ready",
    )?;
    let _: Value = serde_json::from_slice(&bytes[split + 4..])?;
    //401 only proves the authenticated hook listener is present. Functional
    //compatibility must still pass actual gateway/SDK discovery and staging.
    Ok(())
}
fn zero_players(text: &str) -> bool {
    text.trim()
        .strip_prefix("There are 0 of a max of ")
        .and_then(|s| s.strip_suffix(" players online:"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
fn phase(case: &Path, name: &str, work: impl FnOnce() -> Result<Value>) -> Result<Value> {
    let utc = utc_ms()?;
    let start = Instant::now();
    let result = work();
    save(
        &case.join(format!("{name}.json")),
        &json!({"phase":name,"started_utc_ms":utc,"finished_utc_ms":utc_ms()?,"wall_ms":start.elapsed().as_millis(),"success":result.is_ok(),"result":result.as_ref().ok(),"error":result.as_ref().err().map(|e|e.to_string().chars().take(512).collect::<String>())}),
    )?;
    result
}
fn restore(
    s: &Spec,
    paths: &[PathBuf],
    case: &Path,
    staging: &Path,
    recovery: &Path,
) -> Result<()> {
    let mut bytes = 0u64;
    for (i, archive) in s.archives.iter().enumerate() {
        archive.file.verify()?;
        let destination = staging.join(&archive.destination);
        parents(staging, &destination)?;
        bytes = bytes.saturating_add(archive::extract(
            &archive.file.path,
            &destination,
            &archive.roots,
            (4 * 1024 * 1024 * 1024u64).saturating_sub(bytes),
        )?);
        ensure(
            bytes <= 4 * 1024 * 1024 * 1024,
            "all archives exceed4GB restore bound",
        )?;
        save(
            &case.join(format!("archive-{i}.json")),
            &json!({"sha256":archive.file.sha256,"safe_regular_and_directory_entries_only":true}),
        )?;
    }
    let staged = staging.join(&s.database.destination);
    s.database.file.verify()?;
    parents(staging, staged.parent().unwrap())?;
    let mut out = create(&staged)?;
    let mut source = fs::File::open(&s.database.file.path)?;
    std::io::copy(&mut source, &mut out)?;
    out.sync_all()?;
    install(s, paths, case, staging, recovery)
}
fn install(
    s: &Spec,
    paths: &[PathBuf],
    case: &Path,
    staging: &Path,
    recovery: &Path,
) -> Result<()> {
    for (i, path) in paths.iter().enumerate() {
        ensure(
            !take_interrupt(),
            "preparation interrupted before install mutation",
        )?;
        let target = s.root.join(path);
        let saved = recovery.join(path);
        parents(recovery, saved.parent().unwrap())?;
        save(
            &case.join(format!("install-{i}-intent.json")),
            &json!({"relative":path,"recovery":saved,"old_exists":target.exists(),"replay_permitted":false}),
        )?;
        fs::File::open(case)?.sync_all()?;
        if present(&target)? {
            durable_rename(&target, &saved)?;
        }
        if path == &s.database.destination {
            for suffix in ["-wal", "-shm"] {
                let side = PathBuf::from(format!("{}{}", target.display(), suffix));
                if present(&side)? {
                    canonical(&side)?;
                    durable_rename(
                        &side,
                        &PathBuf::from(format!("{}{}", saved.display(), suffix)),
                    )?;
                }
            }
        }
        durable_rename(&staging.join(path), &target)?;
        save(
            &case.join(format!("install-{i}-done.json")),
            &json!({"installed":true,"relative":path,"prior_preserved":saved.exists()}),
        )?;
    }
    Ok(())
}
fn durable_rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    fs::File::open(from.parent().ok_or("rename source parent absent")?)?.sync_all()?;
    fs::File::open(to.parent().ok_or("rename destination parent absent")?)?.sync_all()?;
    Ok(())
}
fn spawn(service: &Service, case: &Path) -> Result<Owned> {
    service.tool.verify()?;
    let child = Command::new(&service.tool.path)
        .args(&service.args)
        .current_dir(&service.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let owned = Owned {
        role: service.role,
        child,
        handed_off: false,
    };
    save(
        &case.join(format!("service-{}-generation.json", service.role.name())),
        &json!({"role":service.role.name(),"pid":owned.child.id(),"process_group":owned.child.id(),"spawned_utc_ms":utc_ms()?,"executable_sha256":service.tool.sha256,"direct_owned_child":true,"stdout_stderr":"not captured; bounded readiness and phase errors are retained","no_adoption_or_port_signalling":true}),
    )?;
    Ok(owned)
}
fn wait(
    case: &Path,
    label: &str,
    seconds: u64,
    owned: &mut [Owned],
    mut check: impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    for attempt in 0..=120 {
        ensure(!take_interrupt(), "preparation readiness interrupted")?;
        for service in &mut *owned {
            service.alive()?;
        }
        let start = Instant::now();
        let before = utc_ms()?;
        let result = check(attempt);
        save(
            &case.join(format!("{label}-{attempt:03}.json")),
            &json!({"success":result.is_ok(),"started_utc_ms":before,"finished_utc_ms":utc_ms()?,"wall_ms":start.elapsed().as_millis(),"error":result.as_ref().err().map(|e|e.to_string().chars().take(512).collect::<String>()),"read_only":true,"mutation_retry":false}),
        )?;
        if result.is_ok() {
            return Ok(());
        }
        ensure(
            Instant::now() < deadline,
            "readiness deadline expired before actor connection",
        )?;
        std::thread::sleep(
            Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    Err("readiness attempt bound exhausted".into())
}

pub(super) fn run(args: &[OsString]) -> Result<()> {
    let total = Instant::now();
    ensure(args.len() == 4, "prepare requires PREP.json SHA NEW_CASE")?;
    let bytes = read(Path::new(&args[1]), 65536)?;
    let hash = args[2].to_str().ok_or("preparation hash absent")?;
    ensure(
        valid_hash(hash, 64) && sha256(&bytes) == hash,
        "preparation hash differs",
    )?;
    let s: Spec = serde_json::from_slice(&bytes)?;
    let f = Fixture::load(&s.fixture.path, &s.fixture.sha256)?;
    ensure(
        hash_file(&std::env::current_exe()?)? == s.bench_sha256,
        "running bench build differs",
    )?;
    let case = Path::new(&args[3]);
    ensure(!case.exists(), "new preparation receipt directory required")?;
    canonical(case.parent().ok_or("case parent absent")?)?;
    mkdir(case)?;
    directory(case)?;
    ensure(
        !case.starts_with(&f.workspace) && !case.starts_with(&f.secret_dir),
        "operator evidence must remain outside bot mounts",
    )?;
    let input_verification_ms = total.elapsed().as_millis();
    save(
        &case.join("preparation.json"),
        &json!({"sha256":hash,"fixture_sha256":s.fixture.sha256,"bench_sha256":s.bench_sha256,"owner":s.owner,"root":s.root,"certifies_goal_completion":false}),
    )?;
    let mut owned = Vec::new();
    let mut delegated = false;
    let result = (|| -> Result<()> {
        let paths = phase(case, "verify", || {
            let paths = validate(&s, &f)?;
            ensure(
                free_bytes(&s.root)? >= 505_000_000_000,
                "500GB reserve plus5GB restore headroom required",
            )?;
            Ok(serde_json::to_value(paths)?)
        })?;
        let paths: Vec<PathBuf> = serde_json::from_value(paths)?;
        ensure(
            !paths.iter().any(|p| case.starts_with(s.root.join(p))),
            "preparation evidence cannot be inside a replaced tree",
        )?;
        let mut lock = create(&s.root.join(".prepare-arm.lock"))?;
        lock.write_all(&serde_json::to_vec(&json!({"owner":s.owner,"case":case,"pid":std::process::id(),"manual_release_only_after_owned_service_and_actor_closure":true}))?)?;
        lock.sync_all()?;
        fs::File::open(&s.root)?.sync_all()?;
        let working = s
            .root
            .join(format!(".prepare-{}-{}", s.owner, std::process::id()));
        mkdir(&working)?;
        let staging = working.join("staging");
        let recovery = working.join("recovery");
        mkdir(&staging)?;
        mkdir(&recovery)?;
        save(
            &case.join("recovery.json"),
            &json!({"staging":staging,"prior_state":recovery,"automatic_rollback":false,"instructions":"Never rerun this preparation. Preserve working/recovery trees and original snapshots; inspect install intents/completions and exact service generations. Never replay uncertain gameplay mutations."}),
        )?;
        phase(case, "restore", || {
            restore(&s, &paths, case, &staging, &recovery)?;
            Ok(json!({"installed_roots":paths.len(),"prior_state_preserved":true}))
        })?;
        for role in [Role::Java, Role::Gateway, Role::Relay] {
            if let Some(service) = s.services.iter().find(|x| x.role == role) {
                phase(case, &format!("start-{}", role.name()), || {
                    owned.push(spawn(service, case)?);
                    Ok(json!({"role":role.name(),"direct_child_spawned":true}))
                })?;
            }
            if role == Role::Java {
                phase(case, "native-hook-ready", || {
                    wait(
                        case,
                        "native-read",
                        s.ready_timeout_secs,
                        &mut owned,
                        |_| native_ready(&s.native),
                    )?;
                    Ok(
                        json!({"gateway_not_started_until_native_hook_ready":true,"listener_only":true,"unauthenticated_401_is_not_functional_admission":true}),
                    )
                })?;
            }
        }
        let mut runner = SystemRunner::new();
        phase(case, "sdk-ready", || {
            wait(
                case,
                "sdk-read",
                s.ready_timeout_secs,
                &mut owned,
                |attempt| {
                    let endpoint = json(f.host_profile()?, 16384)?["endpoint"]
                        .as_str()
                        .ok_or("operator endpoint absent")?
                        .to_owned();
                    let out = process::execute(
                        &mut runner,
                        &f,
                        &f.sdk,
                        vec!["discover".into(), endpoint],
                        2,
                        case,
                        &format!("sdk-discover-{attempt:03}"),
                        &mut |_| Ok(()),
                    )?;
                    ensure(out.success, "SDK discovery not ready")?;
                    let value: Value = serde_json::from_slice(&out.stdout)?;
                    ensure(
                        value["ready"] == true
                            && value["instance_id"] == f.instance_id
                            && value["capabilities"].as_array().is_some_and(|a| {
                                a.iter().any(|v| v == "minecraft:fresh_observation")
                            }),
                        "frozen ready SDK instance/capability absent",
                    )
                },
            )?;
            Ok(json!({"actor_connections":0,"model_calls":0}))
        })?;
        phase(case, "zero-player-session", || {
            let mut rcon = rcon_client(&f)?.ok_or("typed loopback RCON required")?;
            let reply = rcon.execute(&rcon::Command::ListPlayers)?;
            ensure(
                zero_players(&reply.text),
                "owned server must be empty before bench",
            )?;
            database(&s.root.join(&s.database.destination), &f.instance_id)?;
            Ok(json!({"players":0,"sessions":0,"actor_connections":0}))
        })?;
        // From here, never auto-stop Java beneath a possibly unresolved actor.
        // Persistent service generations are an explicit operator recovery handoff.
        for child in &mut owned {
            child.handed_off = true;
        }
        delegated = true;
        phase(case, "bench", || {
            super::run(vec![
                if f.is_combined() {
                    "combine"
                } else {
                    "acquire"
                }
                .into(),
                s.fixture.path.as_os_str().into(),
                s.fixture.sha256.clone().into(),
                case.join("arm").into_os_string(),
            ])?;
            Ok(
                json!({"existing_bench_completed":true,"native_goal_verdict":"external evidence required"}),
            )
        })?;
        Ok(())
    })();
    if !delegated {
        for child in &mut owned {
            child.stop();
        }
    }
    save(
        &case.join("prepare-summary.json"),
        &json!({"format":"minecraft-qa-prepare-arm-v1","success":result.is_ok(),"wall_ms":total.elapsed().as_millis(),"input_verification_ms":input_verification_ms,"fixture_sha256":s.fixture.sha256,"preparation_sha256":hash,"bench_sha256":s.bench_sha256,"bench_delegated":delegated,"owned_services":owned.iter().map(|c|json!({"role":c.role.name(),"pid":c.child.id(),"operator_handoff":c.handed_off})).collect::<Vec<_>>(),"services_preserved_for_operator_closure":delegated,"readiness_failure_cleanup":"Only newly spawned direct owned process groups; previous state/snapshots remain. No automatic rollback.","certifies_goal_completion":false,"error":result.as_ref().err().map(|_|"preparation or delegated bench failed; private phase evidence and recovery state retained")}),
    )?;
    result
}
