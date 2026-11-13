use super::*;
use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;

struct Fake {
    replies: VecDeque<process::Output>,
    calls: Vec<Vec<String>>,
}
impl Runner for Fake {
    fn run(
        &mut self,
        _: &Path,
        args: &[String],
        _: u64,
        secrets: &Secrets,
        on_line: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<process::Output> {
        self.calls.push(args.to_vec());
        let mut out = self
            .replies
            .pop_front()
            .ok_or("unexpected fake tool call")?;
        if args.first().is_some_and(|s| s == "connect") {
            let path = Path::new(&args[1]);
            let mut profile = json(path, 16384)?;
            profile["session"] =
                json!({"session_id":"freshly-created-session-do-not-log","adapter_version":"test"});
            fs::write(path, serde_json::to_vec(&profile)?)?;
            out.stdout = serde_json::to_vec(
                &json!({"connected":true,"session_id":"freshly-created-session-do-not-log"}),
            )?;
            out.stderr = b"created freshly-created-session-do-not-log".to_vec();
        }
        out.stdout = secrets.scrub(&out.stdout);
        out.stderr = secrets.scrub(&out.stderr);
        for line in out.stdout.split(|b| *b == b'\n') {
            if let Ok(v) = serde_json::from_slice(line) {
                if on_line(&v).is_err() {
                    out.success = false;
                    break;
                }
            }
        }
        Ok(out)
    }
}
fn output(text: String, success: bool) -> process::Output {
    process::Output {
        exit_code: Some(if success { 0 } else { 1 }),
        signal: None,
        interrupted: false,
        stdout: text.into_bytes(),
        stderr: Vec::new(),
        success,
        timed_out: false,
    }
}
pub(super) struct Setup {
    _temp: tempfile::TempDir,
    pub(super) f: Fixture,
    pub(super) case: PathBuf,
}
fn write(path: &Path, bytes: &[u8]) {
    let mut f = create(path).unwrap();
    f.write_all(bytes).unwrap();
}
pub(super) fn setup() -> Setup {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    for dir in ["workspace", "secrets", "case"] {
        fs::create_dir(base.join(dir)).unwrap();
        fs::set_permissions(base.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = json!({"endpoint":"https://gateway.invalid","instance_id":"a".repeat(64),"credential":{"bot_id":"owned","player_uuid":"941dfe08-5bbd-4715-8640-898ba50f443d","key":"private-sdk-key-not-for-output"},"session":null});
    write(
        &base.join("secrets/profile.json"),
        &serde_json::to_vec(&profile).unwrap(),
    );
    write(
        &base.join("secrets/model.key"),
        b"allocated-proxy-key-not-for-output",
    );
    let config = json!({"profile":"/run/bench-secrets/profile.json","model_key":"/run/bench-secrets/model.key","workspace":"/workspace/program","state":"/tmp/brain-state","body":{"enabled":false,"period_ms":150,"decision_samples":128}});
    let spec = json!({"entity_uuid":"a3af7dfd-4558-4ef8-bccb-4c5379960383","entity_type":"minecraft:skeleton","entity_health":20,"main_hand":"minecraft:bow","position_tolerance":0.5,"player_health":20,"player_food":20,"player_position":[262.5,63.0,-720.5],"entity_position":[262.5,64.0,-709.5],"timeout_secs":120});
    write(&base.join("handler"), b"reviewed handler");
    write(
        &base.join("config.json"),
        &serde_json::to_vec(&config).unwrap(),
    );
    write(&base.join("spec.json"), &serde_json::to_vec(&spec).unwrap());
    write(&base.join("tool"), b"fake tool");
    let pinned =
        |name: &str| json!({"path":base.join(name),"sha256":hash_file(&base.join(name)).unwrap()});
    let mut f:Fixture=serde_json::from_value(json!({"version":1,"case_id":"test-case","instance_id":"a".repeat(64),"player_uuid":"941dfe08-5bbd-4715-8640-898ba50f443d","npc_uuid":"a3af7dfd-4558-4ef8-bccb-4c5379960383","connection_request_id":"test-connect-once","duration_secs":120,"source_revision":"b".repeat(40),"container":"existing-test-container","workspace":base.join("workspace"),"container_workspace":"/workspace","secret_dir":base.join("secrets"),"container_secret_dir":"/run/bench-secrets","profile_name":"profile.json","model_key_name":"model.key","allocated_model_actor_uuid":"941dfe08-5bbd-4715-8640-898ba50f443d","allocated_model_kind":"operator-allocated-proxy","sdk":pinned("tool"),"docker":pinned("tool"),"evidence":pinned("tool"),"artifacts":[{"file":pinned("handler"),"container_path":"/workspace/handler"},{"file":pinned("config.json"),"container_path":"/workspace/config.json"},{"file":pinned("spec.json"),"container_path":"/workspace/spec.json"}],"handler":"/workspace/handler","handler_args":["/workspace/config.json","blocked","120","/workspace/spec.json"],"expected_player_position":[262.5,63.0,-720.5],"expected_npc_position":[262.5,64.0,-709.5],"position_tolerance":0.5,"expected_health":20.0,"expected_food":20,"trace_action_prefix":"body-owned-","operator_rcon":null})).unwrap();
    f.container_id = Some("d".repeat(64));
    f.container_started_at = Some("2026-11-13T00:00:00Z".into());
    f.owned_container = true;
    f.bot_dev = Some(f.sdk.clone());
    f.runtime_check = Some("/workspace/mc-sdk-runtime-check".into());
    f.artifacts.push(config::Artifact {
        file: f.sdk.clone(),
        container_path: f.runtime_check.clone().unwrap(),
    });
    f.operator_profile = Some(base.join("operator-profile.json"));
    let mut host = json(&f.profile(), 16384).unwrap();
    host["endpoint"] = json!("http://127.0.0.1:19146/mac-eval/");
    write(
        f.host_profile().unwrap(),
        &serde_json::to_vec(&host).unwrap(),
    );
    Setup {
        _temp: temp,
        f,
        case: base.join("case"),
    }
}
fn observation(f: &Fixture) -> Value {
    json!({"revision":"test","player":{"uuid":f.player_uuid,"life_id":"eac7697a-2c77-4ae2-a7f9-a9ed7aac21b5","dimension":"minecraft:overworld","health":20.0,"food":20,"position":f.expected_player_position,"inventory":[]},"world":{},"menu":{"menu_id":0,"slots":[],"carried":{"item":"minecraft:air","count":0}}})
}
fn mounts(f: &Fixture) -> Value {
    json!({"id":f.container_id,"started_at":f.container_started_at,"running":true,"readonly_rootfs":true,"privileged":false,"user":"1000:1000","memory":536870912,"nano_cpus":1000000000u64,"pids":256,"cap_drop":["ALL"],"cap_add":[],"security":["no-new-privileges:true"],"network":"existing-private-network","pid_mode":"","ipc_mode":"private","devices":[],"env_names":["PATH","HOME"],"mounts":[{"Type":"bind","Source":f.secret_dir,"Destination":f.container_secret_dir,"RW":false},{"Type":"tmpfs","Source":"","Destination":"/workspace","RW":true}]})
}
fn preflight_replies(f: &Fixture) -> VecDeque<process::Output> {
    let mut q = VecDeque::from([output(mounts(f).to_string(), true)]);
    for a in &f.artifacts {
        q.push_back(output(
            format!("{}  {}\n", a.file.sha256, a.container_path),
            true,
        ));
    }
    q.push_back(output(String::new(), true));
    if f.model_key_name.is_some() {
        q.push_back(output(String::new(), true));
    }
    q.push_back(output("1000".into(), true));
    q.push_back(output("8180 600 1000\n8180 600 1000\n".into(), true));
    q.push_back(output(json!({"format":"minecraft-runtime-filesystem-check-v1","unix_socket_bind":true,"socket_mode_0600":true,"probe_removed":true,"sdk_or_gameplay_calls":false}).to_string(), true));
    q.push_back(output(json!({"ready":true,"instance_id":f.instance_id,"capabilities":["minecraft:fresh_observation"]}).to_string(),true));
    q
}
#[test]
fn fixture_rejects_credential_copy_extra_mount_files_and_drift() {
    let s = setup();
    s.f.validate().unwrap();
    write(
        &s.f.secret_dir.join("server-native.token"),
        b"must never be mounted",
    );
    assert!(s.f.validate().is_err());
    fs::remove_file(s.f.secret_dir.join("server-native.token")).unwrap();
    fs::write(&s.f.artifacts[0].file.path, b"changed handler").unwrap();
    assert!(s.f.validate().is_err());
}
#[test]
fn list_fastpath_refuses_nonloopback_before_opening_any_fixture_or_secret() {
    assert!(
        run(vec![
            "rcon-list".into(),
            "192.0.2.1:25575".into(),
            "/nonexistent/password".into()
        ])
        .is_err()
    );
}
#[test]
fn absent_fresh_capability_stops_preflight_before_connect() {
    let s = setup();
    let mut replies = preflight_replies(&s.f);
    *replies.back_mut().unwrap() = output(
        json!({"ready":true,"instance_id":s.f.instance_id,"capabilities":[]}).to_string(),
        true,
    );
    let mut fake = Fake {
        replies,
        calls: vec![],
    };
    assert!(preflight(&mut fake, &s.f, &s.case, "no-cap").is_err());
    assert!(
        !fake
            .calls
            .iter()
            .any(|a| a.first().is_some_and(|v| v == "connect"))
    );
}
#[test]
fn unsupported_runtime_socket_stops_before_actor_connection() {
    let s = setup();
    let mut replies = preflight_replies(&s.f);
    let socket_index = replies.len() - 2;
    replies[socket_index] = output("chmod: Invalid argument".into(), false);
    let mut fake = Fake {
        replies,
        calls: vec![],
    };
    assert!(stage(&mut fake, &s.f, &s.case, &"c".repeat(64)).is_err());
    assert!(
        !fake
            .calls
            .iter()
            .any(|args| args.first().is_some_and(|a| a == "connect"))
    );
    assert!(!s.case.join("stage-intent.json").exists());
}
#[test]
fn mounts_reject_rw_secret_alias_and_unreviewed_credentials() {
    let s = setup();
    let mut m = mounts(&s.f);
    config::validate_mounts(&s.f, &m).unwrap();
    m["mounts"][0]["RW"] = json!(true);
    assert!(config::validate_mounts(&s.f, &m).is_err());
    let mut m = mounts(&s.f);
    m["mounts"].as_array_mut().unwrap().push(
        json!({"Type":"volume","Source":"/unreviewed/account","Destination":"/account","RW":false}),
    );
    assert!(config::validate_mounts(&s.f, &m).is_err());
    for (field, bad) in [
        ("user", json!("0")),
        ("network", json!("host")),
        ("memory", json!(0)),
        ("cap_add", json!(["SYS_ADMIN"])),
    ] {
        let mut m = mounts(&s.f);
        m[field] = bad;
        assert!(config::validate_mounts(&s.f, &m).is_err());
    }
    let mut m = mounts(&s.f);
    m["mounts"].as_array_mut().unwrap().push(json!({"Type":"bind","Source":s.f.secret_dir.parent().unwrap(),"Destination":"/host","RW":false}));
    assert!(config::validate_mounts(&s.f, &m).is_err());
}
#[test]
fn blocked_requires_no_model_key_but_task_does() {
    let mut s = setup();
    fs::remove_file(s.f.model_key().unwrap()).unwrap();
    s.f.model_key_name = None;
    s.f.allocated_model_actor_uuid = None;
    s.f.allocated_model_kind = None;
    let path = &s.f.artifacts[1].file.path;
    let mut cfg = json(path, 65536).unwrap();
    cfg["model_key"] = json!("");
    fs::write(path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    s.f.artifacts[1].file.sha256 = hash_file(path).unwrap();
    s.f.validate().unwrap();
    s.f.handler_args[1] = "task".into();
    assert!(s.f.validate().is_err());
}
#[test]
fn session_sync_changes_only_session_and_keeps_endpoints_separate() {
    let s = setup();
    let original = json(&s.f.profile(), 16384).unwrap();
    let mut host = json(s.f.host_profile().unwrap(), 16384).unwrap();
    host["session"] = json!({"session_id":"new-shared-session-identity","adapter_version":"test"});
    fs::write(
        s.f.host_profile().unwrap(),
        serde_json::to_vec(&host).unwrap(),
    )
    .unwrap();
    sync_session(&s.f, false).unwrap();
    let internal = json(&s.f.profile(), 16384).unwrap();
    assert_eq!(internal["endpoint"], original["endpoint"]);
    assert_eq!(internal["credential"], original["credential"]);
    assert_eq!(internal["session"], host["session"]);
    assert_ne!(internal["endpoint"], host["endpoint"]);
    sync_session(&s.f, true).unwrap();
    assert!(json(&s.f.profile(), 16384).unwrap()["session"].is_null());
    assert!(json(s.f.host_profile().unwrap(), 16384).unwrap()["session"].is_null());
}
#[test]
fn container_private_inputs_require_mode_owner_and_nonsymlink() {
    validate_private_metadata("8180 600 1000", 1000).unwrap();
    for bad in ["81a4 644 1000", "a180 600 1000", "8180 600 0"] {
        assert!(validate_private_metadata(bad, 1000).is_err());
    }
}
#[test]
fn invalid_profile_after_child_withholds_raw_output() {
    struct Corrupt;
    impl Runner for Corrupt {
        fn run(
            &mut self,
            _: &Path,
            args: &[String],
            _: u64,
            _: &Secrets,
            _: &mut dyn FnMut(&Value) -> Result<()>,
        ) -> Result<process::Output> {
            fs::write(&args[0], b"{}")?;
            Ok(output("untracked-new-session-token".into(), false))
        }
    }
    let s = setup();
    let result = process::execute(
        &mut Corrupt,
        &s.f,
        &s.f.sdk,
        vec![s.f.profile().display().to_string()],
        1,
        &s.case,
        "invalid-profile",
        &mut |_| Ok(()),
    );
    assert!(result.is_err());
    assert!(!s.case.join("invalid-profile.stdout").exists());
    assert!(
        s.case
            .join("invalid-profile.secret-refresh-failed.json")
            .exists()
    );
}
#[test]
fn ready_refuses_old_life_wrong_actor_injured_cursor_and_pose() {
    let s = setup();
    let good = observation(&s.f);
    validate_player(&s.f, &good, true).unwrap();
    for (path, bad) in [
        ("uuid", json!(s.f.npc_uuid)),
        ("life_id", Value::Null),
        ("health", json!(19)),
        ("food", json!(19)),
        ("position", json!([0, 0, 0])),
    ] {
        let mut v = good.clone();
        v["player"][path] = bad;
        assert!(validate_player(&s.f, &v, true).is_err());
    }
    let mut v = good;
    v["menu"]["carried"] = json!({"item":"minecraft:oak_log","count":1});
    assert!(validate_player(&s.f, &v, true).is_err());
}
#[test]
fn fake_stage_connects_exact_id_and_leaves_console_handoff_pending() {
    let s = setup();
    let mut replies = preflight_replies(&s.f);
    replies.push_back(output(json!({"connected":true}).to_string(), true));
    replies.push_back(output(
        json!({"format":"minecraft-fresh-observation-v1","observation":observation(&s.f)})
            .to_string(),
        true,
    ));
    let mut fake = Fake {
        replies,
        calls: Vec::new(),
    };
    stage(&mut fake, &s.f, &s.case, &"c".repeat(64)).unwrap();
    assert_eq!(
        fake.calls[9],
        vec![
            "connect",
            s.f.host_profile().unwrap().to_str().unwrap(),
            "test-connect-once"
        ]
    );
    assert_eq!(
        json(&s.case.join("stage.json"), 16384).unwrap()["operator_handoff_pending"],
        true
    );
    assert!(!s.case.join("summon-intent.json").exists());
    for entry in fs::read_dir(&s.case).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(path).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("freshly-created-session-do-not-log")
            );
        }
    }
    assert!(stage(&mut fake, &s.f, &s.case, &"c".repeat(64)).is_err());
}
#[test]
fn fake_launch_retains_fatal_tail_and_disconnects_without_pass() {
    let s = setup();
    let hash = "c".repeat(64);
    let obs = observation(&s.f);
    let mut profile = json(&s.f.profile(), 16384).unwrap();
    profile["session"] = json!({"session_id":"private-session-never-log","adapter_version":"test"});
    fs::write(s.f.profile(), serde_json::to_vec(&profile).unwrap()).unwrap();
    let mut host = json(s.f.host_profile().unwrap(), 16384).unwrap();
    host["session"] = profile["session"].clone();
    fs::write(
        s.f.host_profile().unwrap(),
        serde_json::to_vec(&host).unwrap(),
    )
    .unwrap();
    save(&s.case.join("stage.json"),&json!({"fixture_sha256":hash,"player_uuid":s.f.player_uuid,"life_id":obs["player"]["life_id"],"connected":true})).unwrap();
    let ready =
        json!({"event":"body_probe_ready","data":{"entity_uuid":s.f.npc_uuid,"observation":obs}});
    let admitted = json!({"event":"body_probe_stimulus_admitted","data":ready["data"]});
    let mut replies = preflight_replies(&s.f);
    let mut handler = output(
        format!("{ready}\n{admitted}\nFATAL private-sdk-key-not-for-output\n"),
        false,
    );
    handler.stderr = b"fatal stderr preserved allocated-proxy-key-not-for-output".to_vec();
    replies.push_back(handler);
    replies.push_back(output(json!({"disconnected":true}).to_string(), true));
    for a in &s.f.artifacts {
        replies.push_back(output(format!("{}  x", a.file.sha256), true));
    }
    replies.push_back(output(String::new(), true));
    replies.push_back(output(String::new(), true));
    let mut fake = Fake {
        replies,
        calls: Vec::new(),
    };
    struct Unavailable;
    impl Runner for Unavailable {
        fn run(
            &mut self,
            _: &Path,
            _: &[String],
            _: u64,
            _: &Secrets,
            _: &mut dyn FnMut(&Value) -> Result<()>,
        ) -> Result<process::Output> {
            Err("fake remote control unavailable".into())
        }
    }
    assert!(launch(&mut fake, &mut Unavailable, &s.f, &s.case, &hash).is_err());
    let raw =
        String::from_utf8(read(&s.case.join("handler.stdout"), 1024 * 1024).unwrap()).unwrap();
    assert!(raw.contains("FATAL [REDACTED]"));
    assert!(!raw.contains("private-sdk-key"));
    assert!(
        String::from_utf8(read(&s.case.join("handler.stderr"), 1024).unwrap())
            .unwrap()
            .contains("fatal stderr preserved [REDACTED]")
    );
    let result = json(&s.case.join("launch-result.json"), 16384).unwrap();
    assert_eq!(result["sdk_disconnected"], true);
    assert_eq!(result["handler_exit_success"], false);
    assert_eq!(result["certifies_goal_completion"], false);
}
#[test]
fn native_capture_keeps_every_failed_running_uncertain_attempt_and_fatal_line() {
    use minecraft_sdk_core::{Action, ActionRequest, Outcome, Receipt};
    use minecraft_sdk_evidence::{ExpectedAction, ResourceUsage, Spec};
    let s = setup();
    let dbpath = s.case.join("gateway.sqlite");
    write(&dbpath, b"");
    let db = rusqlite::Connection::open(&dbpath).unwrap();
    db.execute_batch("CREATE TABLE metadata(key TEXT,value TEXT);CREATE TABLE bots(id TEXT,uuid TEXT);CREATE TABLE actions(bot TEXT,id TEXT,intent TEXT,receipt TEXT);").unwrap();
    db.execute(
        "INSERT INTO metadata VALUES('instance',?1)",
        [&s.f.instance_id],
    )
    .unwrap();
    db.execute("INSERT INTO bots VALUES('owned',?1)", [&s.f.player_uuid])
        .unwrap();
    let action = Action::SelectSlot { slot: 0 };
    let mut trace = Vec::new();
    for seq in 1..=131 {
        let outcome = match seq % 3 {
            1 => Outcome::Rejected,
            2 => Outcome::Uncertain,
            _ => Outcome::Running,
        };
        let id = format!("body-owned-{seq}");
        let receipt = Receipt {
            action_id: id.clone(),
            outcome,
            adapter_version: "test".into(),
            evidence: json!({}),
            error: None,
        };
        db.execute(
            "INSERT INTO actions VALUES('owned',?1,?2,?3)",
            rusqlite::params![
                id,
                serde_json::to_string(&action).unwrap(),
                serde_json::to_string(&receipt).unwrap()
            ],
        )
        .unwrap();
        trace.extend(
            serde_json::to_vec(
                &json!({"event":"body_action_intent","sequence":seq,"data":{"action":action}}),
            )
            .unwrap(),
        );
        trace.push(b'\n');
    }
    trace.extend(b"Error: real fatal tail\n");
    let tracepath = s.case.join("input.log");
    write(&tracepath, &trace);
    write(&s.case.join("input.stderr"), b"fatal stderr preserved");
    write(&s.case.join("input.status.json"), b"{\"exit_code\":1}");
    let spec = Spec {
        run_id: "test-run".into(),
        goal: "retain failures".into(),
        instance_id: s.f.instance_id.clone(),
        player_uuid: s.f.player_uuid.clone(),
        source_revision: s.f.source_revision.clone(),
        controller_sha256: s.f.artifacts[0].file.sha256.clone(),
        program_source_sha256: sha256(b""),
        program_binary_sha256: sha256(b""),
        trace_sha256: sha256(&trace),
        model_requests: Default::default(),
        resources: ResourceUsage {
            measurement: "unmeasured".into(),
            container_memory_bytes: None,
            cpu_percent: None,
            pids: None,
            notes: "no inference or programme execution claim".into(),
        },
        assists: vec![],
        interventions: vec![],
        failures: vec![],
        actions: vec![ExpectedAction {
            request: ActionRequest {
                action_id: "placeholder".into(),
                action,
            },
            outcome: Outcome::Running,
        }],
    };
    let specpath = s.case.join("template.json");
    write(&specpath, &serde_json::to_vec(&spec).unwrap());
    capture::native(&s.f, &s.case, &dbpath, &tracepath, &specpath, &[]).unwrap();
    let manifest = json(&s.case.join("capture-manifest.json"), 65536).unwrap();
    assert_eq!(manifest["all_original_actions"], 131);
    assert_eq!(
        manifest["outcomes"],
        json!({"rejected":44,"uncertain":44,"running":43})
    );
    assert_eq!(manifest["non_json_lines_retained"], 1);
    assert_eq!(
        read(&s.case.join("source-stderr.log"), 1024).unwrap(),
        b"fatal stderr preserved"
    );
    assert_eq!(
        json(&s.case.join("source-status.json"), 1024).unwrap()["exit_code"],
        1
    );
    assert_eq!(manifest["native_reports"].as_array().unwrap().len(), 2);
    assert_eq!(
        read(&s.case.join("native-original.log"), 65536).unwrap(),
        trace
    );
}
#[test]
#[ignore = "child fixture invoked only by the bounded process test"]
fn closed_pipe_child_fixture() {
    unsafe {
        libc::close(1);
        libc::close(2);
    }
    std::thread::sleep(Duration::from_secs(60));
}
#[test]
fn closed_child_pipes_do_not_escape_process_deadline() {
    let started = std::time::Instant::now();
    let args = [
        "--exact",
        "bench::tests::closed_pipe_child_fixture",
        "--ignored",
        "--nocapture",
    ]
    .map(str::to_owned);
    let out = SystemRunner::new()
        .run(
            &std::env::current_exe().unwrap(),
            &args,
            1,
            &Secrets(vec![]),
            &mut |_| Ok(()),
        )
        .unwrap();
    assert!(out.timed_out && !out.success);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn prelaunch_rcon_init_failure_attempts_one_host_disconnect() {
    let mut s = setup();
    let hash = "c".repeat(64);
    let observed = observation(&s.f);
    let password =
        s.f.workspace
            .parent()
            .unwrap()
            .join("operator-rcon.password");
    write(&password, b"short"); // Fails locally before any socket connection.
    s.f.operator_rcon = Some(config::OperatorRcon {
        address: "127.0.0.1:0".parse().unwrap(),
        password_file: password,
    });
    let mut host = json(s.f.host_profile().unwrap(), 16384).unwrap();
    host["session"] =
        json!({"session_id":"staged-existing-session-token","adapter_version":"test"});
    fs::write(
        s.f.host_profile().unwrap(),
        serde_json::to_vec(&host).unwrap(),
    )
    .unwrap();
    sync_session(&s.f, false).unwrap();
    save(&s.case.join("stage.json"),&json!({"fixture_sha256":hash,"player_uuid":s.f.player_uuid,"life_id":observed["player"]["life_id"],"connected":true})).unwrap();
    let mut replies = preflight_replies(&s.f);
    replies.push_back(output(json!({"disconnected":true}).to_string(), true));
    let mut fake = Fake {
        replies,
        calls: vec![],
    };
    let mut control = Fake {
        replies: VecDeque::new(),
        calls: vec![],
    };
    assert!(launch(&mut fake, &mut control, &s.f, &s.case, &hash).is_err());
    assert_eq!(
        fake.calls
            .iter()
            .filter(|a| a.first().is_some_and(|v| v == "disconnect"))
            .count(),
        1
    );
    assert!(!s.case.join("handler.stdout").exists());
    assert_eq!(
        json(&s.case.join("prelaunch-failure.json"), 16384).unwrap()["native_absence_verified"],
        false
    );
}
#[test]
fn prepared_generation_change_disconnects_without_repeating_preflight() {
    let s = setup();
    let mut host = json(s.f.host_profile().unwrap(), 16384).unwrap();
    host["session"] = json!({"session_id":"prepared-session-token","adapter_version":"test"});
    fs::write(
        s.f.host_profile().unwrap(),
        serde_json::to_vec(&host).unwrap(),
    )
    .unwrap();
    sync_session(&s.f, false).unwrap();
    let mut runner = Fake {
        replies: VecDeque::from([output(json!({"disconnected":true}).to_string(), true)]),
        calls: vec![],
    };
    let mut control = Fake {
        replies: VecDeque::from([output(
            json!({"id":s.f.container_id,"started_at":"different-generation","running":true})
                .to_string(),
            true,
        )]),
        calls: vec![],
    };
    assert!(
        launch_prepared(
            &mut runner,
            &mut control,
            &s.f,
            &s.case,
            &"c".repeat(64),
            true
        )
        .is_err()
    );
    assert_eq!(
        runner.calls,
        vec![vec![
            "disconnect".to_owned(),
            s.f.host_profile().unwrap().display().to_string()
        ]]
    );
    assert_eq!(control.calls.len(), 1);
    assert_eq!(control.calls[0][0], "inspect");
    assert!(!s.case.join("handler.stdout").exists());
}
#[test]
fn preexisting_or_unwritable_intent_cannot_block_disconnect() {
    for unwritable in [false, true] {
        let s = setup();
        let blocked = s.case.join("unwritable-ledger");
        let case = if unwritable {
            write(&blocked, b"owned regular file, not a directory");
            blocked.as_path()
        } else {
            write(
                &s.case.join("launch-disconnect-intent.json"),
                b"original retained intent",
            );
            s.case.as_path()
        };
        let mut fake = Fake {
            replies: VecDeque::from([output(json!({"disconnected":true}).to_string(), true)]),
            calls: vec![],
        };
        let mut attempted = false;
        let result = disconnect_launch(&mut fake, &s.f, case, &mut attempted);
        assert!(attempted);
        assert_eq!(fake.calls.len(), 1);
        assert_eq!(fake.calls[0][0], "disconnect");
        if unwritable {
            assert!(result.is_err());
        } else {
            assert!(result.is_ok());
            assert_eq!(
                read(&s.case.join("launch-disconnect-intent.json"), 1024).unwrap(),
                b"original retained intent"
            );
        }
        assert!(disconnect_launch(&mut fake, &s.f, case, &mut attempted).is_err());
        assert_eq!(fake.calls.len(), 1);
    }
}
#[test]
fn damaged_internal_profile_or_missing_model_key_cannot_block_host_cleanup() {
    let s = setup();
    fs::write(s.f.profile(), b"{}").unwrap();
    fs::remove_file(s.f.model_key().unwrap()).unwrap();
    let mut fake = Fake {
        replies: VecDeque::from([output(json!({"disconnected":true}).to_string(), true)]),
        calls: vec![],
    };
    let mut attempted = false;
    // The request succeeds; internal session synchronization remains explicitly incomplete.
    assert!(disconnect_launch(&mut fake, &s.f, &s.case, &mut attempted).is_err());
    assert_eq!(fake.calls.len(), 1);
    assert_eq!(
        fake.calls[0],
        vec!["disconnect", s.f.host_profile().unwrap().to_str().unwrap()]
    );
    let status = json(&s.case.join("launch-disconnect.status.json"), 16384).unwrap();
    assert_eq!(status["request_attempted"], true);
    assert_eq!(status["api_acknowledged"], true);
    assert_eq!(status["session_sync_cleared"], false);
    assert_eq!(status["native_absence_verified"], false);
}
