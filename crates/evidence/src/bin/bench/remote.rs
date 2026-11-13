//! Container/PID ownership evidence, never host-process-name signalling.
use super::*;
use std::time::Instant;
#[derive(Clone, Copy)]
pub struct Identity {
    pub pid: u32,
    pub start: u64,
}
#[derive(PartialEq, Eq)]
pub enum State {
    Gone,
    Alive,
    Unknown,
}
fn tool(runner: &mut dyn Runner, f: &Fixture, args: Vec<String>) -> Result<process::Output> {
    tool_with_timeout(runner, f, args, 2)
}
fn tool_with_timeout(
    runner: &mut dyn Runner,
    f: &Fixture,
    args: Vec<String>,
    seconds: u64,
) -> Result<process::Output> {
    f.docker.verify()?;
    // These fixed metadata/control commands neither read nor persist credentials.
    // A damaged bot profile must not prevent closure of its frozen container.
    runner.run(
        &f.docker.path,
        &args,
        seconds,
        &Secrets(vec![]),
        &mut |_| Ok(()),
    )
}
pub fn snapshot(runner: &mut dyn Runner, f: &Fixture, case: &Path, label: &str) -> Result<bool> {
    let out=tool(runner,f,vec!["inspect".into(),"--format".into(),"{\"id\":{{json .Id}},\"started_at\":{{json .State.StartedAt}},\"running\":{{json .State.Running}}}".into(),f.target()?.into()])?;
    ensure(out.success, "cannot verify frozen container ownership")?;
    let value: Value = serde_json::from_slice(&out.stdout)?;
    ensure(
        value["id"] == f.target()?
            && value["started_at"].as_str() == f.container_started_at.as_deref()
            && value["running"].is_boolean(),
        "container ID/start ownership changed",
    )?;
    // Preserve the ownership fence even when evidence storage has failed. The
    // surrounding launch still fails if its required final record cannot save.
    let _ = save(&case.join(format!("{label}.json")), &value);
    Ok(value["running"] == true)
}
fn pids(runner: &mut dyn Runner, f: &Fixture) -> Result<Vec<u32>> {
    let out = tool(
        runner,
        f,
        vec![
            "exec".into(),
            f.target()?.into(),
            "ps".into(),
            "-eo".into(),
            "pid=".into(),
        ],
    )?;
    ensure(
        out.success && out.stdout.len() <= 65536,
        "scoped PID inventory unavailable",
    )?;
    let pids: Vec<u32> = std::str::from_utf8(&out.stdout)?
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    ensure(pids.len() <= 64, "scoped PID inventory exceeds bound")?;
    Ok(pids)
}
fn start(runner: &mut dyn Runner, f: &Fixture, pid: u32) -> Result<u64> {
    let out = tool(
        runner,
        f,
        vec![
            "exec".into(),
            f.target()?.into(),
            "cat".into(),
            format!("/proc/{pid}/stat"),
        ],
    )?;
    ensure(
        out.success && out.stdout.len() <= 16384,
        "PID identity unavailable",
    )?;
    let text = std::str::from_utf8(&out.stdout)?;
    ensure(
        text.split_once('(')
            .and_then(|(p, _)| p.trim().parse::<u32>().ok())
            == Some(pid),
        "PID identity differs",
    )?;
    Ok(text
        .rsplit_once(") ")
        .ok_or("stat shape differs")?
        .1
        .split_whitespace()
        .nth(19)
        .ok_or("start time absent")?
        .parse()?)
}
pub fn bind(runner: &mut dyn Runner, f: &Fixture, case: &Path) -> Result<Identity> {
    ensure(
        snapshot(runner, f, case, "ready-container-identity")?,
        "container stopped before READY",
    )?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut found = None;
    for pid in pids(runner, f)? {
        ensure(Instant::now() < deadline, "PID binding deadline exceeded")?;
        let out = tool(
            runner,
            f,
            vec![
                "exec".into(),
                f.target()?.into(),
                "readlink".into(),
                format!("/proc/{pid}/exe"),
            ],
        )?;
        if out.success && std::str::from_utf8(&out.stdout)?.trim() == f.handler {
            let identity = Identity {
                pid,
                start: start(runner, f, pid)?,
            };
            ensure(
                found.is_none(),
                "multiple matching handlers; ownership ambiguous",
            )?;
            found = Some(identity);
        }
    }
    let identity = found.ok_or("exact handler PID not found")?;
    save(
        &case.join("owned-remote-handler.json"),
        &json!({"container_id":f.target()?,"container_started_at":f.container_started_at,"pid":identity.pid,"start_jiffies":identity.start,"reviewed_executable_match":true,"scope":"Container-scoped /proc identity; no argv/environment retained"}),
    )?;
    Ok(identity)
}
fn state(runner: &mut dyn Runner, f: &Fixture, id: Identity) -> State {
    match pids(runner, f) {
        Ok(pids) if !pids.contains(&id.pid) => State::Gone,
        Ok(_) => match start(runner, f, id.pid) {
            Ok(now) if now == id.start => State::Alive,
            Ok(_) => State::Gone,
            Err(_) => State::Unknown,
        },
        Err(_) => State::Unknown,
    }
}
pub fn close(
    runner: &mut dyn Runner,
    f: &Fixture,
    case: &Path,
    id: Option<Identity>,
) -> Result<bool> {
    if !snapshot(runner, f, case, "close-container-before")? {
        return Ok(true);
    }
    let Some(id) = id else { return Ok(false) };
    let mut current = state(runner, f, id);
    if current == State::Alive {
        // Fence both container generation and PID start immediately before signalling.
        ensure(
            snapshot(runner, f, case, "interrupt-container-identity")?,
            "container stopped before scoped signal",
        )?;
        if state(runner, f, id) == State::Alive {
            let out = tool(
                runner,
                f,
                vec![
                    "exec".into(),
                    f.target()?.into(),
                    "kill".into(),
                    "-s".into(),
                    "INT".into(),
                    id.pid.to_string(),
                ],
            )?;
            save(
                &case.join("remote-interrupt.json"),
                &json!({"pid":id.pid,"start_jiffies":id.start,"signal":"SIGINT","command_acknowledged":out.success,"scope":"Checked PID/start before signal; no process-name or host PID signalling"}),
            )?;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            current = state(runner, f, id);
            if current != State::Alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    save(
        &case.join("remote-handler-closure.json"),
        &json!({"pid":id.pid,"start_jiffies":id.start,"old_handler_gone":current==State::Gone,"scope":"Remote process evidence, not inference from host docker-exec exit; descendants require container finalization"}),
    )?;
    Ok(current == State::Gone)
}
pub fn stop(runner: &mut dyn Runner, f: &Fixture, case: &Path) -> Result<bool> {
    if !snapshot(runner, f, case, "stop-container-before")? {
        return Ok(true);
    }
    let stopped = tool_with_timeout(
        runner,
        f,
        vec![
            "stop".into(),
            "--time".into(),
            "2".into(),
            f.target()?.into(),
        ],
        10,
    )?;
    let _ = save(
        &case.join("stop-owned-container.json"),
        &json!({"container_id":f.target()?,"command_acknowledged":stopped.success,"timed_out":stopped.timed_out,"native_state_verified":false}),
    );
    Ok(!snapshot(runner, f, case, "stop-container-after")?)
}
pub fn archive(
    runner: &mut dyn Runner,
    f: &Fixture,
    case: &Path,
    seconds: u64,
    handler_and_actor_closed: bool,
) -> Result<bool> {
    ensure(
        snapshot(runner, f, case, "archive-container-before")?,
        "archive requires matching running owned container",
    )?;
    let bot = f.bot_dev.as_ref().ok_or("pinned bot-dev unavailable")?;
    let archive = case.join("workspace-archive");
    let mut args = vec![
        "capture".into(),
        archive.display().to_string(),
        seconds.to_string(),
        "--".into(),
        f.docker.path.display().to_string(),
        "exec".into(),
        f.target()?.into(),
        "tar".into(),
    ];
    for pattern in [
        "target",
        "cache",
        ".cache",
        ".cargo",
        "*.key",
        "*.token",
        "*.password",
        "*.secret",
        ".env",
        ".env.*",
        "auth.json",
        "credentials.json",
        f.profile_name.as_str(),
    ] {
        args.push(format!("--exclude={pattern}"));
    }
    if let Some(name) = &f.model_key_name {
        args.push(format!("--exclude={name}"));
    }
    args.extend([
        "-czf".into(),
        "-".into(),
        "-C".into(),
        "/".into(),
        f.container_workspace.trim_start_matches('/').into(),
    ]);
    let state = f.state_path()?;
    if !Path::new(&state).starts_with(&f.container_workspace) {
        args.push(state.trim_start_matches('/').into());
    }
    let out = process::execute(
        runner,
        f,
        bot,
        args,
        seconds + 10,
        case,
        "archive-bot-dev",
        &mut |_| Ok(()),
    )?;
    let summary = json(&archive.join("summary.json"), 65536)?;
    let complete = out.success
        && summary["format"] == "bot-dev-capture-v1"
        && summary["exit_code"] == 0
        && summary["timed_out"] == false
        && summary["output_limit"] == false
        && summary["interrupted"] == false
        && summary["spawn_error"].is_null()
        && summary["signal"].is_null();
    let path = archive.join("stdout.log");
    let bytes = read(&path, 16 * 1024 * 1024)?;
    let complete = complete && bytes.starts_with(&[0x1f, 0x8b]) && handler_and_actor_closed;
    save(
        &case.join("workspace-archive.json"),
        &json!({"complete":complete,"compressed_tar_file":"workspace-archive/stdout.log","sha256":sha256(&bytes),"bytes":bytes.len(),"handler_and_actor_closure_verified":handler_and_actor_closed,"descendant_quiescence_verified":false,"secret_mount_outside_workspace":true,"credential_file_patterns_excluded":true,"arbitrary_embedded_content_certified_keyfree":false,"scope":"Existing bot-dev preserves a bounded selected-file snapshot before final container stop, not final writes of remaining descendants. Operator must verify content before publication; compressed bytes are not text-scrubbed."}),
    )?;
    Ok(complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        container: String,
        started: String,
        handler: String,
        running: bool,
        pid_present: bool,
        start: u64,
        kills: Vec<String>,
        stops: usize,
    }
    impl Runner for Fake {
        fn run(
            &mut self,
            _: &Path,
            args: &[String],
            _: u64,
            _: &Secrets,
            _: &mut dyn FnMut(&Value) -> Result<()>,
        ) -> Result<process::Output> {
            let text = match args.first().map(String::as_str) {
                Some("inspect") => {
                    json!({"id":self.container,"started_at":self.started,"running":self.running})
                        .to_string()
                }
                Some("stop") => {
                    self.stops += 1;
                    self.running = false;
                    self.container.clone()
                }
                Some("exec") => match args.get(2).map(String::as_str) {
                    Some("ps") => if self.pid_present {
                        "1\n10\n11\n"
                    } else {
                        "1\n11\n"
                    }
                    .into(),
                    Some("readlink") => {
                        if args[3] == "/proc/10/exe" {
                            self.handler.clone()
                        } else {
                            "/usr/bin/ps".into()
                        }
                    }
                    Some("cat") => {
                        let mut fields = vec!["S".into(); 20];
                        fields[19] = self.start.to_string();
                        format!("10 (reviewed-handler) {}", fields.join(" "))
                    }
                    Some("kill") => {
                        self.kills.push(args.last().unwrap().clone());
                        self.pid_present = false;
                        String::new()
                    }
                    _ => return Err("unexpected metadata call".into()),
                },
                _ => return Err("unexpected control call".into()),
            };
            Ok(process::Output {
                stdout: text.into_bytes(),
                stderr: vec![],
                success: true,
                timed_out: false,
                exit_code: Some(0),
                signal: None,
                interrupted: false,
            })
        }
    }
    fn fake(f: &Fixture) -> Fake {
        Fake {
            container: f.target().unwrap().into(),
            started: f.container_started_at.clone().unwrap(),
            handler: f.handler.clone(),
            running: true,
            pid_present: true,
            start: 42,
            kills: vec![],
            stops: 0,
        }
    }
    #[test]
    fn binds_remote_identity_and_interrupts_only_same_start() {
        let s = super::super::tests::setup();
        let mut c = fake(&s.f);
        let id = bind(&mut c, &s.f, &s.case).unwrap();
        assert_eq!((id.pid, id.start), (10, 42));
        assert!(close(&mut c, &s.f, &s.case, Some(id)).unwrap());
        assert_eq!(c.kills, vec!["10"]);
    }
    #[test]
    fn reused_pid_is_never_signalled_and_old_handler_is_gone() {
        let s = super::super::tests::setup();
        let mut c = fake(&s.f);
        c.start = 43;
        assert!(close(&mut c, &s.f, &s.case, Some(Identity { pid: 10, start: 42 })).unwrap());
        assert!(c.kills.is_empty());
    }
    #[test]
    fn generation_change_refuses_container_stop() {
        let s = super::super::tests::setup();
        let mut c = fake(&s.f);
        c.started = "2026-11-14T00:00:00Z".into();
        assert!(stop(&mut c, &s.f, &s.case).is_err());
        assert_eq!(c.stops, 0);
    }
    #[test]
    fn owned_container_stop_requires_remote_stopped_snapshot() {
        let s = super::super::tests::setup();
        let mut c = fake(&s.f);
        assert!(stop(&mut c, &s.f, &s.case).unwrap());
        assert_eq!(c.stops, 1);
        assert!(!c.running);
    }
    #[test]
    fn damaged_profile_and_existing_evidence_do_not_block_owned_container_stop() {
        let s = super::super::tests::setup();
        let mut c = fake(&s.f);
        fs::write(s.f.profile(), b"invalid profile").unwrap();
        save(
            &s.case.join("stop-container-before.json"),
            &json!({"old":true}),
        )
        .unwrap();
        assert!(stop(&mut c, &s.f, &s.case).unwrap());
        assert_eq!(c.stops, 1);
        assert!(!c.running);
        assert_eq!(
            json(&s.case.join("stop-container-before.json"), 1024).unwrap()["old"],
            true
        );
    }
}
