use super::*;
use std::os::unix::fs::PermissionsExt;
fn private(path: &Path, bytes: &[u8]) {
    let mut file = create(path).unwrap();
    file.write_all(bytes).unwrap();
}
fn pin(path: &Path) -> Pinned {
    Pinned {
        path: path.into(),
        sha256: hash_file(path).unwrap(),
    }
}
fn tar_header(path: &str, size: u64, kind: u8) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..path.len()].copy_from_slice(path.as_bytes());
    h[100..108].copy_from_slice(b"0000600\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].fill(b' ');
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}
fn gzip(path: &Path, entries: &[(&str, u8, &[u8])]) {
    let file = create(path).unwrap();
    let mut gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    for (name, kind, data) in entries {
        gzip.write_all(&tar_header(name, data.len() as u64, *kind))
            .unwrap();
        gzip.write_all(data).unwrap();
        gzip.write_all(&vec![0; (512 - data.len() % 512) % 512])
            .unwrap();
    }
    gzip.write_all(&[0; 1024]).unwrap();
    gzip.finish().unwrap().sync_all().unwrap();
}
#[test]
fn normalized_archive_preserves_source_and_extracts_only_declared_private_files() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    mkdir(&out).unwrap();
    let file = root.path().join("world.tar.gz");
    gzip(
        &file,
        &[
            ("world", b'5', b""),
            ("world/region", b'5', b""),
            ("world/region/synthetic.bin", b'0', b"retained fixture"),
        ],
    );
    let before = hash_file(&file).unwrap();
    let bytes = archive::extract_with_space(&file, &out, &["world".into()], || Ok(505_000_000_000))
        .unwrap();
    assert!(bytes < 8192);
    assert_eq!(
        fs::read(out.join("world/region/synthetic.bin")).unwrap(),
        b"retained fixture"
    );
    assert_eq!(
        fs::metadata(out.join("world/region/synthetic.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(hash_file(&file).unwrap(), before);
}
#[test]
fn archive_rejects_traversal_links_extensions_duplicate_roots_and_disk_exhaustion() {
    for (name, kind) in [
        ("../escape", b'0'),
        ("/escape", b'0'),
        ("world/link", b'2'),
        ("world/pax", b'x'),
        ("other", b'0'),
    ] {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("out");
        mkdir(&out).unwrap();
        let file = root.path().join("bad.gz");
        gzip(&file, &[(name, kind, b"x")]);
        assert!(
            archive::extract_with_space(&file, &out, &["world".into()], || Ok(505_000_000_000))
                .is_err(),
            "{name}"
        );
        assert!(!root.path().join("escape").exists());
    }
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("duplicate.gz");
    let out = root.path().join("out");
    mkdir(&out).unwrap();
    gzip(
        &file,
        &[
            ("world/file", b'0', b"first"),
            ("world/file", b'0', b"second"),
        ],
    );
    assert!(
        archive::extract_with_space(&file, &out, &["world".into()], || Ok(505_000_000_000))
            .is_err()
    );
    assert_eq!(fs::read(out.join("world/file")).unwrap(), b"first");
    let file = root.path().join("full.gz");
    gzip(&file, &[("full/file", b'0', b"data")]);
    assert!(
        archive::extract_with_space(&file, &out, &["full".into()], || Ok(500_000_000_000)).is_err()
    );
    assert!(!out.join("full/file").exists());
}
#[test]
fn native_auth_required_is_listener_only_and_request_is_read_only() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let native = Native {
        address: listener.local_addr().unwrap(),
    };
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            socket.read_exact(&mut b).unwrap();
            request.push(b[0]);
            assert!(request.len() < 1024);
        }
        assert!(request.starts_with(b"GET /v1/discover HTTP/1.1\r\n"));
        let body = b"{\"error\":\"unauthorized\"}";
        socket
            .write_all(
                format!(
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        socket.write_all(body).unwrap();
    });
    native_ready(&native).unwrap();
    worker.join().unwrap();
}
#[test]
fn native_connection_refusal_is_not_admission_or_mutation_uncertainty() {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    assert!(native_ready(&Native { address }).is_err());
}
#[test]
fn zero_player_contract_rejects_ambiguity_and_scope_paths_are_closed() {
    assert!(zero_players("There are 0 of a max of 10 players online:"));
    for text in [
        "There are 1 of a max of 10 players online: Player",
        "0",
        "players:0",
        "There are 0 of a max of players online:",
    ] {
        assert!(!zero_players(text));
    }
    for path in ["../world", "/world", "server/../world", ""] {
        assert!(!relative(Path::new(path)));
    }
    assert!(relative(Path::new("server/world")));
}
fn prepared() -> (super::super::tests::Setup, Spec) {
    let mut setup = super::super::tests::setup();
    let root = setup.case.parent().unwrap().to_path_buf();
    for name in ["server", "runtime", "snapshots"] {
        mkdir(&root.join(name)).unwrap();
    }
    let marker = root.join("qa-owner.json");
    private(&marker,br#"{"format":"minecraft-qa-root-v1","owner":"synthetic-qa","isolated_qa":true,"production":false}"#);
    let old = root.join("closed.json");
    let closed = json!({"format":"minecraft-qa-preparation-closure-v1","owner":"synthetic-qa","root":root,"players":0,"sessions":0,"services":[{"role":"java","pid":2147483647u32,"generation":"prior-synthetic-java","executable_sha256":"a".repeat(64)},{"role":"gateway","pid":2147483646u32,"generation":"prior-synthetic-gateway","executable_sha256":"b".repeat(64)}]});
    private(&old, &serde_json::to_vec(&closed).unwrap());
    let fixture = root.join("frozen.json");
    private(&fixture, b"synthetic fixture pin");
    let archive = root.join("snapshots/world.gz");
    gzip(&archive, &[("world/file", b'0', b"world")]);
    let database = root.join("snapshots/gateway.sqlite");
    private(&database, b"");
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(
        "CREATE TABLE metadata(key TEXT,value TEXT); CREATE TABLE sessions(bot TEXT);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO metadata VALUES('instance',?1)",
        [&setup.f.instance_id],
    )
    .unwrap();
    drop(db);
    setup.f.mode = config::Mode::Acquisition;
    setup.f.operator_rcon = Some(config::OperatorRcon {
        address: "127.0.0.1:9".parse().unwrap(),
        password_file: root.join("unused.password"),
    });
    setup.f.operator_gateway_journal = Some(root.join("runtime/gateway.sqlite"));
    let a = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = TcpListener::bind("127.0.0.1:0").unwrap();
    let c = TcpListener::bind("127.0.0.1:0").unwrap();
    let rcon = c.local_addr().unwrap();
    let native = a.local_addr().unwrap();
    let gateway = b.local_addr().unwrap();
    drop((a, b, c));
    setup.f.operator_rcon.as_mut().unwrap().address = rcon;
    let mut profile = json(setup.f.host_profile().unwrap(), 16384).unwrap();
    profile["endpoint"] = json!(format!("http://{gateway}/synthetic/"));
    fs::write(
        setup.f.host_profile().unwrap(),
        serde_json::to_vec(&profile).unwrap(),
    )
    .unwrap();
    let tool = pin(&root.join("tool"));
    let spec = Spec {
        version: 1,
        owner: "synthetic-qa".into(),
        root: root.clone(),
        marker: pin(&marker),
        prior_closure: pin(&old),
        fixture: pin(&fixture),
        bench_sha256: "a".repeat(64),
        archives: vec![Restore {
            file: pin(&archive),
            destination: "server".into(),
            roots: vec!["world".into()],
        }],
        database: Database {
            file: pin(&database),
            destination: "runtime/gateway.sqlite".into(),
        },
        services: vec![
            Service {
                role: Role::Java,
                tool: tool.clone(),
                cwd: root.join("server"),
                args: vec!["synthetic".into()],
                listen: vec![native, rcon],
            },
            Service {
                role: Role::Gateway,
                tool,
                cwd: root.join("server"),
                args: vec!["synthetic".into()],
                listen: vec![gateway],
            },
        ],
        native: Native { address: native },
        ready_timeout_secs: 1,
    };
    (setup, spec)
}
#[test]
fn validation_rejects_foreign_listener_live_generation_hash_and_unsafe_destination_before_restore()
{
    for scenario in 0..7 {
        let (mut setup, mut spec) = prepared();
        let mut listener = None;
        match scenario {
            0 => {
                validate(&spec, &setup.f).unwrap();
                continue;
            }
            1 => {
                listener = Some(TcpListener::bind(spec.native.address).unwrap());
            }
            2 => {
                let mut value = json(&spec.prior_closure.path, 16384).unwrap();
                value["services"][0]["pid"] = json!(std::process::id());
                let path = spec.root.join("live-generation.json");
                private(&path, &serde_json::to_vec(&value).unwrap());
                spec.prior_closure = pin(&path);
            }
            3 => spec.archives[0].file.sha256 = "0".repeat(64),
            4 => spec.archives[0].destination = "../foreign".into(),
            5 => setup.f.operator_rcon.as_mut().unwrap().address = spec.services[1].listen[0],
            _ => {
                let mut profile = json(setup.f.host_profile().unwrap(), 16384).unwrap();
                profile["endpoint"] =
                    json!(format!("http://{}/wrong-server/", spec.native.address));
                fs::write(
                    setup.f.host_profile().unwrap(),
                    serde_json::to_vec(&profile).unwrap(),
                )
                .unwrap();
            }
        };
        assert!(validate(&spec, &setup.f).is_err(), "scenario{scenario}");
        assert!(!spec.root.join("server/world").exists());
        assert!(!spec.root.join("runtime/gateway.sqlite").exists());
        drop(listener);
    }
}
#[test]
fn endpoint_binding_preserves_tls_and_never_guesses_hostnames_or_default_ports() {
    assert_eq!(
        endpoint_address("https://127.0.0.1:19100/scoped/").unwrap(),
        "127.0.0.1:19100".parse::<SocketAddr>().unwrap()
    );
    assert!(endpoint_address("http://[::1]:19100/").is_ok());
    for endpoint in [
        "http://localhost:19100/",
        "http://127.0.0.1/",
        "http://user@127.0.0.1:19100/",
        "ftp://127.0.0.1:19100/",
        "http://192.0.2.1:19100/",
        "http://127.0.0.1:19100/?key=x",
    ] {
        assert!(endpoint_address(endpoint).is_err(), "{endpoint}");
    }
}
#[test]
fn phase_wall_receipts_retain_failure_without_claiming_child_or_game_success() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        phase(root.path(), "failed-readiness", || Err(
            "synthetic refusal".into()
        ))
        .is_err()
    );
    let value = json(&root.path().join("failed-readiness.json"), 16384).unwrap();
    assert_eq!(value["success"], false);
    assert!(value["wall_ms"].is_u64());
    assert!(value["result"].is_null());
    assert!(!root.path().join("bench.json").exists());
}
#[test]
fn install_preserves_original_world_database_and_sidecars_without_rollback_or_delete() {
    let (setup, spec) = prepared();
    let staging = spec.root.join("staging");
    let recovery = spec.root.join("recovery");
    mkdir(&staging).unwrap();
    mkdir(&recovery).unwrap();
    parents(&staging, &staging.join("server/world")).unwrap();
    parents(&staging, &staging.join("runtime")).unwrap();
    parents(&spec.root, &spec.root.join("server/world")).unwrap();
    private(
        &spec.root.join("server/world/old"),
        b"original world history",
    );
    private(&staging.join("server/world/new"), b"new snapshot");
    private(&spec.root.join("runtime/gateway.sqlite"), b"old database");
    private(&spec.root.join("runtime/gateway.sqlite-wal"), b"old WAL");
    private(&staging.join("runtime/gateway.sqlite"), b"new database");
    let paths = vec![
        PathBuf::from("server/world"),
        PathBuf::from("runtime/gateway.sqlite"),
    ];
    install(&spec, &paths, &setup.case, &staging, &recovery).unwrap();
    assert_eq!(
        fs::read(recovery.join("server/world/old")).unwrap(),
        b"original world history"
    );
    assert_eq!(
        fs::read(recovery.join("runtime/gateway.sqlite")).unwrap(),
        b"old database"
    );
    assert_eq!(
        fs::read(recovery.join("runtime/gateway.sqlite-wal")).unwrap(),
        b"old WAL"
    );
    assert_eq!(
        fs::read(spec.root.join("server/world/new")).unwrap(),
        b"new snapshot"
    );
    assert!(!spec.root.join("runtime/gateway.sqlite-wal").exists());
    assert!(setup.case.join("install-1-done.json").exists());
    assert!(
        install(&spec, &paths, &setup.case, &staging, &recovery).is_err(),
        "existing durable intents block replay"
    );
    assert_eq!(
        fs::read(recovery.join("server/world/old")).unwrap(),
        b"original world history"
    );
}
#[test]
fn exclusive_root_lock_never_overwrites_a_prior_preparation() {
    let root = tempfile::tempdir().unwrap();
    let lock = root.path().join(".prepare-arm.lock");
    private(&lock, b"prior owner and recovery receipt");
    assert!(create(&lock).is_err());
    assert_eq!(
        fs::read(&lock).unwrap(),
        b"prior owner and recovery receipt"
    );
}
#[test]
fn remaining_combined_archive_budget_is_enforced_before_file_creation() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    mkdir(&out).unwrap();
    let source = root.path().join("limited.gz");
    gzip(&source, &[("world/file", b'0', b"payload")]);
    assert!(
        archive::extract_bounded(&source, &out, &["world".into()], 1023, || Ok(
            505_000_000_000
        ))
        .is_err()
    );
    assert!(!out.join("world/file").exists());
}
#[test]
fn dangling_restore_root_and_database_sidecar_aliases_fail_before_mutation() {
    for sidecar in [false, true] {
        let (setup, spec) = prepared();
        let alias = if sidecar {
            spec.root.join("runtime/gateway.sqlite-wal")
        } else {
            spec.root.join("server/world")
        };
        std::os::unix::fs::symlink(spec.root.join("missing-recovery-target"), &alias).unwrap();
        assert!(validate(&spec, &setup.f).is_err());
        assert!(
            fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!spec.root.join("runtime/gateway.sqlite").exists());
    }
}
