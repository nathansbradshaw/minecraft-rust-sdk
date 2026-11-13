//! Development runner: offline replay, focused regression, bounded child capture.
use minecraft_sdk_xtask::workflow::{Result, analyze, digest, fixture};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        fs::OpenOptionsExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

fn file_record(file: &Path) -> Result<Value> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut input = File::open(file)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(
        json!({"file":file.file_name().ok_or("Filename required")?.to_string_lossy(),
        "bytes":fs::metadata(file)?.len(),"sha256":format!("{:x}",hash.finalize())}),
    )
}

fn create_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}

fn output_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

fn save(path: &Path, value: &Value) -> Result<()> {
    let mut output = output_file(path)?;
    serde_json::to_writer_pretty(&mut output, value)?;
    writeln!(output)?;
    output.sync_all()?;
    Ok(())
}

struct OwnedChild(Child, bool);
impl OwnedChild {
    fn finish(&mut self) -> Result<std::process::ExitStatus> {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let status = self.0.wait()?;
        self.1 = true;
        Ok(status)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // The child creates its own process group before exec. Only that owned
        // group is signalled, including descendants that outlive their parent.
        if !self.1 {
            let _ = self.finish();
        }
    }
}

fn capture(path: &Path, mut command: Command, timeout: Duration) -> Result<Value> {
    create_directory(path)?;
    let stdout_path = path.join("stdout.log");
    let stderr_path = path.join("stderr.log");
    let stdout = output_file(&stdout_path)?;
    let stderr = output_file(&stderr_path)?;
    let artifacts = path.join("artifacts");
    create_directory(&artifacts)?;
    let started = Instant::now();
    command
        .env("BOT_DEV_CAPTURE_DIR", artifacts.canonicalize()?)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0);
    let spawned = command.spawn();
    let mut report = json!({"format":"bot-dev-capture-v1","timeout_ms":timeout.as_millis(),
        "output_limit_bytes_per_stream":OUTPUT_LIMIT,"timed_out":false,"output_limit":false,
        "exit_code":null,"signal":null,"spawn_error":null,"interrupted":false});
    match spawned {
        Err(error) => report["spawn_error"] = json!(error.to_string()),
        Ok(child) => {
            let mut child = OwnedChild(child, false);
            report["pid"] = json!(child.0.id());
            loop {
                if INTERRUPTED.load(Ordering::Relaxed) {
                    report["interrupted"] = json!(true);
                    break;
                }
                if started.elapsed() >= timeout {
                    report["timed_out"] = json!(true);
                    break;
                }
                if fs::metadata(&stdout_path)?.len() > OUTPUT_LIMIT
                    || fs::metadata(&stderr_path)?.len() > OUTPUT_LIMIT
                {
                    report["output_limit"] = json!(true);
                    break;
                }
                if let Some(status) = child.0.try_wait()? {
                    report["exit_code"] = json!(status.code());
                    report["signal"] = json!(status.signal());
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let status = child.finish()?;
            report["exit_code"] = json!(status.code());
            report["signal"] = json!(status.signal());
            report["owned_group_cleanup"] = json!(
                "SIGKILL then wait; processes that detach into other groups are outside this contract"
            );
        }
    }
    report["elapsed_ms"] = json!(started.elapsed().as_millis());
    for (stream, file) in [("stdout", stdout_path), ("stderr", stderr_path)] {
        report[stream] = file_record(&file)?;
    }
    let mut rows: Vec<_> = fs::read_dir(&artifacts)?.collect::<std::io::Result<_>>()?;
    rows.sort_by_key(|entry| entry.file_name());
    let mut records = Vec::new();
    let mut errors = Vec::new();
    let mut total = 0u64;
    for row in rows {
        let metadata = fs::symlink_metadata(row.path())?;
        total = total.saturating_add(metadata.len());
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || total > 64 * 1024 * 1024
            || records.len() >= 64
        {
            errors.push("Artifact skipped: regular files only,64files/64MiB per run".to_owned());
            continue;
        }
        records.push(file_record(&row.path())?);
    }
    report["artifacts"] = json!(records);
    report["artifact_errors"] = json!(errors);
    save(&path.join("summary.json"), &report)?;
    Ok(report)
}

fn clean_exit(report: &Value, expected: i32) -> bool {
    report["exit_code"] == expected
        && report["timed_out"] == false
        && report["output_limit"] == false
        && report["spawn_error"].is_null()
        && report["interrupted"] == false
        && report["artifact_errors"] == json!([])
}

fn replay(path: &Path) -> Result<Value> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("fixture-worker");
    let process = capture(path, command, Duration::from_secs(10))?;
    let diagnosis = if clean_exit(&process, 2) {
        serde_json::from_slice::<Value>(&fs::read(path.join("stdout.log"))?)?
    } else {
        Value::Null
    };
    let reproduced = clean_exit(&process, 2)
        && diagnosis["model_choices"] == 20
        && diagnosis["summed_model_ms"] == 285186
        && diagnosis["repeated_menu_results"] == 17;
    Ok(json!({"reproduced":reproduced,"process":process,"diagnosis":diagnosis}))
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err("Cannot bind capture to repository source".into());
    }
    Ok(output.stdout)
}

fn check(path: &Path) -> Result<bool> {
    let started = Instant::now();
    // Resolve the caller's checkout, not a compiler-baked temporary worktree.
    // The same built tool remains usable after an obsolete worktree is removed.
    let cwd = std::env::current_dir()?;
    let root = PathBuf::from(
        String::from_utf8(git_bytes(&cwd, &["rev-parse", "--show-toplevel"])?)?.trim(),
    );
    if !root
        .join("crates/brain/src/development_replay.rs")
        .is_file()
    {
        return Err("Run check from an SDK checkout containing development_replay.rs".into());
    }
    create_directory(path)?;
    let revision = String::from_utf8(git_bytes(&root, &["rev-parse", "HEAD"])?)?;
    let status = git_bytes(&root, &["status", "--porcelain"])?;
    let diff = git_bytes(&root, &["diff", "HEAD", "--"])?;
    save(
        &path.join("fixture.json"),
        &serde_json::from_str(fixture::CAPTURE)?,
    )?;
    let mut events = output_file(&path.join("events.jsonl"))?;
    for event in fixture::events()? {
        writeln!(events, "{event}")?;
    }
    events.sync_all()?;
    let baseline = replay(&path.join("replay"))?;
    if baseline["reproduced"] != true {
        save(
            &path.join("summary.json"),
            &json!({"format":"bot-dev-check-v1","success":false,
            "replay":baseline,"regression":"not launched because recorded-failure reproduction failed"}),
        )?;
        return Ok(false);
    }
    let exe = std::env::current_exe()?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let profile = exe.parent()?;
            matches!(profile.file_name()?.to_str()?, "debug" | "release")
                .then(|| profile.parent().map(Path::to_path_buf))
                .flatten()
        })
        .ok_or("Set CARGO_TARGET_DIR to the existing shared cache")?;
    let mut command = Command::new("cargo");
    command
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .args([
            "test",
            "--offline",
            "--locked",
            "-p",
            "minecraft-sdk-brain",
            "--lib",
            "development_replay",
            "-j",
            "2",
            "--",
            "--test-threads=2",
        ]);
    let regression = capture(&path.join("regression"), command, Duration::from_secs(600))?;
    let unchanged = revision.as_bytes() == git_bytes(&root, &["rev-parse", "HEAD"])?
        && status == git_bytes(&root, &["status", "--porcelain"])?
        && diff == git_bytes(&root, &["diff", "HEAD", "--"])?;
    let success = baseline["reproduced"] == true && clean_exit(&regression, 0) && unchanged;
    let report = json!({"format":"bot-dev-check-v1","success":success,
        "source":{"revision":revision.trim(),"tracked_diff_sha256":digest(&diff),
            "status_sha256":digest(&status),"unchanged_during_check":unchanged,
            "binding_limit":"Git status and tracked diff detect ordinary edits; untracked file contents are not attested. Commit before comparative measurements."},
        "runner":file_record(&exe)?,"fixture_sha256":digest(fixture::CAPTURE.as_bytes()),"replay":baseline,
        "regression":regression,"elapsed_ms":started.elapsed().as_millis(),
        "scope":"Offline recorded-failure reproduction and real SDK menu projection checks. No model, gateway, Minecraft server, account or world launched. Does not measure end-to-end model speedup or successful native crafting."});
    save(&path.join("summary.json"), &report)?;
    println!(
        "{}",
        json!({"success":success,"summary":path.join("summary.json"),
        "elapsed_ms":report["elapsed_ms"],"repeated_menu_results":17})
    );
    Ok(success)
}

fn load_events(path: &Path) -> Result<Vec<Value>> {
    if fs::metadata(path)?.len() > 64 * 1024 * 1024 {
        return Err("Journal exceeds64MiB; select a bounded run".into());
    }
    let text = fs::read_to_string(path)?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if line.len() > 1024 * 1024 {
                return Err("Journal event exceeds1MiB".into());
            }
            Ok(serde_json::from_str(line)?)
        })
        .collect()
}

fn argument(args: &[OsString], index: usize) -> Result<&OsStr> {
    args.get(index)
        .map(OsString::as_os_str)
        .ok_or_else(|| "Missing argument".into())
}

fn run() -> Result<bool> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|arg| arg.to_str()) {
        Some("fixture-output") if args.len()==1 && std::env::var_os("BOT_DEV_TEST_FIXTURE").is_some() => {
            std::io::stdout().write_all(&vec![b'x';9*1024*1024])?;
            Ok(true)
        }
        Some("fixture-artifact-link") if args.len()==1 && std::env::var_os("BOT_DEV_TEST_FIXTURE").is_some() => {
            let directory=std::env::var_os("BOT_DEV_CAPTURE_DIR").ok_or("Capture directory required")?;
            std::os::unix::fs::symlink("/no-read-target",Path::new(&directory).join("link"))?;
            Ok(true)
        }
        Some("fixture-process-tree") if args.len()==1 && std::env::var_os("BOT_DEV_TEST_FIXTURE").is_some() => {
            let mut child = Command::new(std::env::current_exe()?).arg("fixture-heartbeat").spawn()?;
            child.wait()?;
            Ok(true)
        }
        Some("fixture-heartbeat") if args.len()==1 && std::env::var_os("BOT_DEV_TEST_FIXTURE").is_some() => {
            let path=std::env::var_os("BOT_DEV_HEARTBEAT").ok_or("Heartbeat path required")?;
            let mut output=OpenOptions::new().append(true).create_new(true).mode(0o600).open(path)?;
            loop { writeln!(output,"alive")?; output.flush()?; std::thread::sleep(Duration::from_millis(10)); }
        }
        Some("fixture-worker") if args.len()==1 => {
            let diagnosis=analyze(&fixture::events()?)?;
            if let Some(directory)=std::env::var_os("BOT_DEV_CAPTURE_DIR") {
                save(&Path::new(&directory).join("diagnosis.json"),&diagnosis)?;
            }
            println!("{diagnosis}");
            std::process::exit(2);
        }
        Some("check") if args.len()==3 && args[1]=="latest-menu" => check(Path::new(&args[2])),
        Some("replay") if args.len()==2 => {
            let report = replay(Path::new(&args[1]))?;
            println!("{report}");
            Ok(report["reproduced"]==true)
        }
        Some("analyze") if args.len()==2 => {
            println!("{}", analyze(&load_events(Path::new(&args[1]))?)?);
            Ok(true)
        }
        Some("capture") if args.len()>=5 && args[3]=="--" => {
            let seconds: u64 = argument(&args,2)?.to_str().ok_or("Numeric timeout required")?.parse()?;
            if !(1..=600).contains(&seconds) { return Err("Timeout must be1..600seconds".into()); }
            let mut command = Command::new(&args[4]);
            command.args(&args[5..]);
            let report = capture(Path::new(&args[1]), command, Duration::from_secs(seconds))?;
            println!("{report}");
            Ok(clean_exit(&report,0))
        }
        _ => Err("bot-dev check latest-menu NEW_DIRECTORY | replay NEW_DIRECTORY | analyze PRIVATE_JOURNAL.jsonl | capture NEW_DIRECTORY TIMEOUT_SECS -- PROGRAM [ARG...]".into()),
    }
}

fn main() {
    unsafe {
        libc::signal(libc::SIGINT, interrupt as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, interrupt as *const () as libc::sighandler_t);
    }
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("bot-dev: {error}");
            std::process::exit(1);
        }
    }
}
