//! Entirely simulated SDK/container/Brain evidence. The bounded loopback server
//! speaks only the test RCON protocol; no real actor, model, compiler or kit score.
use super::*;
use std::{
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

const BINARY_ARCHIVE: &[u8] = &[0x1f, 0x8b, 0x08, 0x00, 0xff, 0x00, 0x80, 0x00, 0x00];
const FAKE_PASSWORD: &str = "synthetic-rcon-password-only";
#[derive(Clone, Copy)]
enum Ending {
    Clean,
    Fatal,
    Timeout,
}
struct Host {
    handler: String,
    stdout: String,
    ending: Ending,
    artifact_hashes: std::collections::BTreeMap<String, String>,
    finished: Arc<AtomicBool>,
    calls: Vec<(Vec<String>, u64)>,
}
impl Runner for Host {
    fn run(
        &mut self,
        _: &Path,
        args: &[String],
        seconds: u64,
        secrets: &Secrets,
        on_line: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<process::Output> {
        self.calls.push((args.to_vec(), seconds));
        if args.iter().any(|a| a == "timeout") {
            assert!(args.iter().any(|a| a == &self.handler));
            let mut out = tests::output(self.stdout.clone(), matches!(self.ending, Ending::Clean));
            out.stdout = secrets.scrub(&out.stdout);
            for line in out.stdout.split(|b| *b == b'\n') {
                if let Ok(value) = serde_json::from_slice(line)
                    && let Err(error) = on_line(&value)
                {
                    out.callback_error = Some(error.to_string().chars().take(1024).collect());
                    out.success = false;
                    break;
                }
            }
            if matches!(self.ending, Ending::Timeout) {
                out.timed_out = true;
                out.exit_code = None;
                out.signal = Some(libc::SIGKILL);
            }
            if matches!(self.ending, Ending::Fatal) {
                out.stderr = b"synthetic fatal Brain stderr".to_vec();
            }
            self.finished.store(true, Ordering::SeqCst);
            return Ok(out);
        }
        if args.first().is_some_and(|a| a == "disconnect") {
            let path = Path::new(&args[1]);
            let mut profile = json(path, 16384)?;
            profile["session"] = Value::Null;
            fs::write(path, serde_json::to_vec(&profile)?)?;
            return Ok(tests::output(
                json!({"disconnected":true}).to_string(),
                true,
            ));
        }
        if args.get(2).is_some_and(|a| a == "sha256sum") {
            return Ok(tests::output(
                format!(
                    "{}  {}\n",
                    self.artifact_hashes
                        .get(&args[3])
                        .ok_or("unmocked artifact")?,
                    args[3]
                ),
                true,
            ));
        }
        if args.get(2).is_some_and(|a| a == "test") {
            return Ok(tests::output(String::new(), true));
        }
        Err("unmocked host operation".into())
    }
}
struct Control {
    id: String,
    started: String,
    handler: String,
    observed: Value,
    artifact_hashes: std::collections::BTreeMap<String, String>,
    finished: Arc<AtomicBool>,
    remote_survives: bool,
    interrupted: bool,
    running: bool,
    changed_generation: bool,
    fresh_queries: usize,
    commands: Vec<Vec<String>>,
    archive_bytes: Vec<u8>,
}
impl Runner for Control {
    fn run(
        &mut self,
        _: &Path,
        args: &[String],
        _: u64,
        _: &Secrets,
        _: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<process::Output> {
        use std::os::unix::fs::DirBuilderExt;
        self.commands.push(args.to_vec());
        let text=match args.first().map(String::as_str) {
            Some("inspect")=>json!({"id":self.id,"started_at":if self.changed_generation{"2026-11-14T00:00:00Z"}else{self.started.as_str()},"running":self.running}).to_string(),
            Some("query")=>{self.fresh_queries+=1;json!({"format":"minecraft-fresh-observation-v1","observation":self.observed}).to_string()},
            Some("stop")=>{assert_eq!(args.last(),Some(&self.id));self.running=false;self.id.clone()},
            Some("capture")=>{
                let dir=PathBuf::from(&args[1]);fs::DirBuilder::new().mode(0o700).create(&dir)?;
                let mut bytes=create(&dir.join("stdout.log"))?;bytes.write_all(&self.archive_bytes)?;bytes.sync_all()?;
                let summary=json!({"format":"bot-dev-capture-v1","exit_code":0,"signal":null,"timed_out":false,"output_limit":false,"interrupted":false,"spawn_error":null});
                save(&dir.join("summary.json"),&summary)?;
                summary.to_string()
            },
            Some("exec")=>match args.get(2).map(String::as_str) {
                Some("/workspace/mc-sdk-runtime-check")=>{
                    assert_eq!(&args[3..], &["process-witness", self.handler.as_str()]);
                    json!({"format":"minecraft-runtime-process-witness-v1","complete":true,"uid":501,
                        "executable":{"path":self.handler,"sha256":self.artifact_hashes.get(&self.handler).ok_or("unmocked handler hash")?,"device":1,"inode":2},
                        "matches":[{"pid":10,"start_jiffies":42,"exe_path":self.handler,"uid":501}],"processes":[{"pid":10,"status":"matched"}]}).to_string()
                },
                Some("ps")=>if !self.finished.load(Ordering::SeqCst) || (self.remote_survives && !self.interrupted) {"1\n10\n11\n"}else{"1\n11\n"}.into(),
                Some("readlink")=>if args[3]=="/proc/10/exe" {self.handler.clone()}else{"/usr/bin/mock-helper".into()},
                Some("cat")=>{let mut fields=vec!["S".to_owned();20];fields[19]="42".into();format!("10 (mock-brain) {}",fields.join(" "))},
                Some("kill")=>{assert_eq!(args.last().map(String::as_str),Some("10"));self.interrupted=true;String::new()},
                Some("sha256sum")=>format!("{}  {}\n",self.artifact_hashes.get(&args[3]).ok_or("unmocked artifact")?,args[3]),
                Some("test")=>String::new(),
                _=>return Err("unmocked container operation".into())
            },
            _=>return Err("unmocked control operation".into())
        };
        Ok(tests::output(text, true))
    }
}
fn prepared() -> tests::Setup {
    let setup = acquisition_config_tests::acquisition();
    let mut profile = json(setup.f.host_profile().unwrap(), 16384).unwrap();
    profile["session"] =
        json!({"session_id":"synthetic-staged-session","adapter_version":"fixture"});
    fs::write(
        setup.f.host_profile().unwrap(),
        serde_json::to_vec(&profile).unwrap(),
    )
    .unwrap();
    sync_session(&setup.f, false).unwrap();
    let observation = tests::observation(&setup.f);
    save(&setup.case.join("stage.json"),&json!({"fixture_sha256":"c".repeat(64),"player_uuid":setup.f.player_uuid,"life_id":observation["player"]["life_id"],"connected":true})).unwrap();
    setup
}
fn harness(f: &Fixture, ending: Ending) -> (Host, Control) {
    let started =
        json!({"event":"brain_started","data":{"uuid":f.player_uuid,"instance":f.instance_id}});
    let another = json!({"event":"synthetic_fixture_event","data":{"models_called":0}});
    let exit = json!({"event":"controller_exit","data":{"disconnected":true}});
    let stdout = if matches!(ending, Ending::Clean) {
        format!("{started}\n{another}\n{exit}\n")
    } else {
        format!("{started}\n{another}\nFATAL synthetic failure\n")
    };
    let finished = Arc::new(AtomicBool::new(false));
    let hashes: std::collections::BTreeMap<_, _> = f
        .artifacts
        .iter()
        .chain(f.ca_file.iter())
        .map(|a| (a.container_path.clone(), a.file.sha256.clone()))
        .collect();
    (
        Host {
            handler: f.handler.clone(),
            stdout,
            ending,
            artifact_hashes: hashes.clone(),
            finished: finished.clone(),
            calls: vec![],
        },
        Control {
            id: f.target().unwrap().into(),
            started: f.container_started_at.clone().unwrap(),
            handler: f.handler.clone(),
            observed: tests::observation(f),
            artifact_hashes: hashes,
            finished,
            remote_survives: matches!(ending, Ending::Timeout),
            interrupted: false,
            running: true,
            changed_generation: false,
            fresh_queries: 0,
            commands: vec![],
            archive_bytes: BINARY_ARCHIVE.to_vec(),
        },
    )
}
fn packet(stream: &mut TcpStream) -> (i32, i32, Vec<u8>) {
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let length = i32::from_le_bytes(header);
    assert!((10..=2048).contains(&length));
    let mut body = vec![0; length as usize];
    stream.read_exact(&mut body).unwrap();
    assert_eq!(&body[body.len() - 2..], &[0, 0]);
    (
        i32::from_le_bytes(body[..4].try_into().unwrap()),
        i32::from_le_bytes(body[4..8].try_into().unwrap()),
        body[8..body.len() - 2].to_vec(),
    )
}
fn reply(stream: &mut TcpStream, id: i32, kind: i32, text: &str) {
    let mut bytes = ((text.len() + 10) as i32).to_le_bytes().to_vec();
    bytes.extend(id.to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend(text.as_bytes());
    bytes.extend([0, 0]);
    stream.write_all(&bytes).unwrap();
}
fn final_absence_server(f: &mut Fixture) -> thread::JoinHandle<Vec<String>> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let path = f
        .workspace
        .parent()
        .unwrap()
        .join("synthetic-rcon.password");
    let mut password = create(&path).unwrap();
    password.write_all(FAKE_PASSWORD.as_bytes()).unwrap();
    password.sync_all().unwrap();
    f.operator_rcon = Some(config::OperatorRcon {
        address,
        password_file: path,
    });
    thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "no final absence connection"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("fake RCON accept failed: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (id, kind, auth) = packet(&mut stream);
        assert_eq!(kind, 3);
        assert!(auth == FAKE_PASSWORD.as_bytes());
        reply(&mut stream, id, 2, "");
        let (id, kind, command) = packet(&mut stream);
        assert_eq!(kind, 2);
        let command = String::from_utf8(command).unwrap();
        assert!(command.starts_with("data get entity ") && command.ends_with(" Health"));
        reply(&mut stream, id, 0, "No entity was found");
        let (id, kind, boundary) = packet(&mut stream);
        assert_eq!(kind, 2);
        assert_eq!(boundary, b"list");
        reply(
            &mut stream,
            id,
            0,
            "There are 0 of a max of 10 players online:",
        );
        vec![command, "list".into()]
    })
}
fn assert_no_npc_operations(case: &Path, commands: &[Vec<String>], rcon: &[String]) {
    assert!(
        commands
            .iter()
            .flatten()
            .all(|s| !s.contains("summon") && !s.contains("forceload"))
    );
    assert_eq!(rcon.len(), 2);
    assert_eq!(rcon[1], "list");
    for name in [
        "summon-intent.json",
        "summon-operator.json",
        "admitted-native.json",
        "last-known-npc.json",
        "cleanup-result.json",
    ] {
        assert!(
            !case.join(name).exists(),
            "unexpected NPC fixture artifact: {name}"
        );
    }
}
#[test]
fn acquisition_brain_first_json_binds_fresh_go_and_closes_without_npc_or_kit_credit() {
    let mut setup = prepared();
    let server = final_absence_server(&mut setup.f);
    let (mut host, mut control) = harness(&setup.f, Ending::Clean);
    launch_prepared(
        &mut host,
        &mut control,
        &setup.f,
        &setup.case,
        &"c".repeat(64),
        true,
    )
    .unwrap();
    let rcon = server.join().unwrap();
    let (args, seconds) = host
        .calls
        .iter()
        .find(|(a, _)| a.iter().any(|v| v == "timeout"))
        .unwrap();
    let index = args.iter().position(|a| a == &setup.f.handler).unwrap();
    assert_eq!(&args[index + 1..], &setup.f.handler_args);
    assert_eq!(
        &setup.f.handler_args[..],
        &["run".to_owned(), setup.f.config_path().unwrap().to_owned()]
    );
    assert!(args.contains(&"1200s".into()));
    assert_eq!(*seconds, 1210);
    assert_eq!(control.fresh_queries, 1);
    let query = control
        .commands
        .iter()
        .position(|a| a.first().is_some_and(|v| v == "query"))
        .unwrap();
    assert!(
        control.commands[..query]
            .iter()
            .any(|a| a.iter().any(|v| v == "process-witness"))
    );
    let go = json(&setup.case.join("acquisition-go.json"), 1024 * 1024).unwrap();
    assert_eq!(go["first_handler_event"], "brain_started");
    assert_eq!(go["npc_operations"], false);
    assert_eq!(go["body_enabled_in_pinned_config"], true);
    let result = json(&setup.case.join("launch-result.json"), 65536).unwrap();
    assert_eq!(result["admitted_native_verified"], false);
    assert_eq!(result["stimulus_admission_required"], false);
    assert_eq!(result["certifies_goal_completion"], false);
    assert!(
        result["kit_score"]
            .as_str()
            .unwrap()
            .contains("no score awarded")
    );
    assert_eq!(result["owned_container_stopped"], true);
    assert_eq!(
        read(&setup.case.join("workspace-archive/stdout.log"), 1024).unwrap(),
        BINARY_ARCHIVE
    );
    assert_eq!(
        json(&setup.case.join("workspace-archive.json"), 65536).unwrap()["sha256"],
        sha256(BINARY_ARCHIVE)
    );
    assert!(
        host.calls
            .iter()
            .any(|(a, _)| a.first().is_some_and(|s| s == "disconnect"))
    );
    assert_no_npc_operations(&setup.case, &control.commands, &rcon);
}
#[test]
fn acquisition_fatal_and_timeout_retain_failure_and_still_finalize_owned_resources() {
    for ending in [Ending::Fatal, Ending::Timeout] {
        let mut setup = prepared();
        let server = final_absence_server(&mut setup.f);
        let (mut host, mut control) = harness(&setup.f, ending);
        assert!(
            launch_prepared(
                &mut host,
                &mut control,
                &setup.f,
                &setup.case,
                &"c".repeat(64),
                true
            )
            .is_err()
        );
        let rcon = server.join().unwrap();
        let result = json(&setup.case.join("launch-result.json"), 65536).unwrap();
        for field in [
            "sdk_disconnect_acknowledged",
            "native_actor_absent",
            "archive_complete",
            "owned_container_stopped",
        ] {
            assert_eq!(result[field], true, "missing finalization: {field}");
        }
        assert_eq!(result["handler_exit_success"], false);
        assert_eq!(result["certifies_goal_completion"], false);
        let status = json(&setup.case.join("handler.status.json"), 65536).unwrap();
        assert_eq!(status["timed_out"], matches!(ending, Ending::Timeout));
        if matches!(ending, Ending::Fatal) {
            assert!(
                String::from_utf8(read(&setup.case.join("handler.stdout"), 1024 * 1024).unwrap())
                    .unwrap()
                    .contains("FATAL synthetic failure")
            );
            assert_eq!(
                read(&setup.case.join("handler.stderr"), 1024).unwrap(),
                b"synthetic fatal Brain stderr"
            );
        } else {
            assert!(control.interrupted);
        }
        assert_eq!(
            read(&setup.case.join("workspace-archive/stdout.log"), 1024).unwrap(),
            BINARY_ARCHIVE
        );
        assert_no_npc_operations(&setup.case, &control.commands, &rcon);
    }
}
#[test]
fn acquisition_changed_container_generation_disconnects_before_brain_launch() {
    let setup = prepared();
    let (mut host, mut control) = harness(&setup.f, Ending::Clean);
    control.changed_generation = true;
    assert!(
        launch_prepared(
            &mut host,
            &mut control,
            &setup.f,
            &setup.case,
            &"c".repeat(64),
            true
        )
        .is_err()
    );
    assert_eq!(host.calls.len(), 1);
    assert_eq!(host.calls[0].0[0], "disconnect");
    assert!(
        !control
            .commands
            .iter()
            .any(|a| a.first().is_some_and(|s| s == "stop"))
    );
    assert!(!setup.case.join("handler.stdout").exists());
    assert_eq!(
        json(&setup.case.join("prelaunch-failure.json"), 65536).unwrap()["native_absence_verified"],
        false
    );
}
