use super::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "minecraft-craft-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("one.json")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn observation(step: u8) -> Observation {
    let (input, output, grid, cursor, stored) = match step {
        0 => (8, 0, 0, 0, 0),
        1 => (7, 4, 1, 0, 0),
        2 => (7, 0, 0, 4, 0),
        _ => (7, 0, 0, 0, 4),
    };
    Observation {
        revision: step.to_string(),
        player: json!({"uuid":"own","life_id":"fixture-life","inventory":[{"slot":9,"item":"fixture:input","count":input},{"slot":10,"item":if stored==0 {"minecraft:air"}else{"fixture:result"},"count":stored}]}),
        world: json!({}),
        menu: json!({"menu_id":17,"state_id":step,"type":"fixture:table",
        "carried":{"item":if cursor==0 {"minecraft:air"}else{"fixture:result"},"count":cursor},"slots":[
        {"slot":0,"player_inventory":false,"item":if output==0 {"minecraft:air"}else{"fixture:result"},"count":output,"may_pickup":true,"may_place_carried":false},
        {"slot":1,"player_inventory":false,"item":if grid==0 {"minecraft:air"}else{"fixture:input"},"count":grid,"may_pickup":true,"may_place_carried":true},
        {"slot":2,"player_inventory":true,"item":"fixture:input","count":input,"may_pickup":true,"may_place_carried":true},
        {"slot":3,"player_inventory":true,"item":if stored==0 {"minecraft:air"}else{"fixture:result"},"count":stored,"may_pickup":true,"may_place_carried":true}]}),
    }
}
fn receipt(id: &str, prepare: bool, outcome: Outcome) -> Receipt {
    Receipt {
        action_id: id.into(),
        outcome: outcome.clone(),
        adapter_version: "fixture".into(),
        error: None,
        evidence: json!({"native_effect_verified":outcome==Outcome::Observed,
        "effect":if prepare {json!({"kind":"recipe_preparation","recipe":"fixture:recipe","menu_id":17,"menu_type":"fixture:table","output_slot":0,"result":{"item":"fixture:result","count":4},"verified_fields":["item","count"],"components_verified":false})}else{Value::Null}}),
    }
}
#[derive(Default)]
struct Fixture {
    step: u8,
    uncertain: bool,
    running: bool,
    reused: bool,
    respawn_after_prepare: bool,
    missing_life: bool,
    posts: Vec<ActionRequest>,
    methods: Vec<String>,
    receipts: BTreeMap<String, Receipt>,
}
struct Peer {
    client: Client,
    state: Arc<Mutex<Fixture>>,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn stop(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap();
    }
}
async fn peer(fixture: Fixture) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = Client::new(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    client.expected_instance = Some("instance".into());
    client.credential = Some(Credential {
        bot_id: "own-bot".into(),
        player_uuid: "own".into(),
        key: "fixture-only-key".into(),
    });
    let state = Arc::new(Mutex::new(fixture));
    let server = state.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = tokio::select! {_=&mut stopped=>break,connection=listener.accept()=>connection.unwrap()};
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 16384);
                header.push(stream.read_u8().await.unwrap());
            }
            let header = String::from_utf8(header).unwrap();
            let first = header.lines().next().unwrap();
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap();
            let path = parts.next().unwrap();
            let size = header
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(size < 16384);
            let mut bytes = vec![0; size];
            stream.read_exact(&mut bytes).await.unwrap();
            let response = {
                let mut state = server.lock().unwrap();
                state.methods.push(first.into());
                if path == "/v1/server" {
                    json!({"api_version":API_VERSION,"instance_id":"instance","minecraft_version":"fixture","platform":"fixture","adapter_version":"fixture","ready":true,"capabilities":[],"limits":{}})
                } else if path.ends_with("/observations") {
                    let mut o = observation(state.step);
                    if state.respawn_after_prepare && state.step > 0 {
                        o.player["life_id"] = json!("new-life");
                    }
                    if state.missing_life {
                        o.player.as_object_mut().unwrap().remove("life_id");
                    }
                    serde_json::to_value(o).unwrap()
                } else if method == "POST" && path.ends_with("/actions") {
                    let request: ActionRequest = serde_json::from_slice(&bytes).unwrap();
                    let prepare = matches!(&request.action, Action::Craft { .. });
                    match &request.action {
                        Action::Craft {
                            make_all, recipe, ..
                        } => {
                            assert!(!make_all);
                            assert_eq!(recipe, "fixture:recipe");
                            if !state.uncertain && !state.reused {
                                state.step = 1;
                            }
                        }
                        Action::ClickSlot {
                            slot, mode, button, ..
                        } => {
                            assert_eq!(*mode, click_mode::PICKUP);
                            assert_eq!(*button, 0);
                            match (*slot, state.step) {
                                (0, 1) => state.step = 2,
                                (3, 2) => state.step = 3,
                                other => panic!("unexpected click{other:?}"),
                            }
                        }
                        _ => panic!("unexpected action"),
                    }
                    let mut receipt = receipt(
                        &request.action_id,
                        prepare,
                        if state.uncertain {
                            Outcome::Uncertain
                        } else if state.running {
                            Outcome::Running
                        } else {
                            Outcome::Observed
                        },
                    );
                    if state.reused {
                        receipt.evidence["gateway_delivery"] = json!({"action_id_already_recorded":true,"new_intention_dispatched":false});
                    }
                    state.posts.push(request);
                    state
                        .receipts
                        .insert(receipt.action_id.clone(), receipt.clone());
                    serde_json::to_value(receipt).unwrap()
                } else if method == "POST" && path.ends_with("/reconcile") {
                    let id = path
                        .trim_end_matches("/reconcile")
                        .rsplit('/')
                        .next()
                        .unwrap();
                    let r = state.receipts.get_mut(id).unwrap();
                    r.outcome = Outcome::Reconciled;
                    serde_json::to_value(r).unwrap()
                } else {
                    let id = path.rsplit('/').next().unwrap();
                    serde_json::to_value(state.receipts.get(id).unwrap()).unwrap()
                }
            };
            let body = serde_json::to_vec(&response).unwrap();
            if stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.is_ok() {
                let _=stream.write_all(&body).await;
            }
        }
    });
    Peer {
        client,
        state,
        stop,
        task,
    }
}

#[tokio::test]
async fn exactly_one_batch_stores_output_preserves_unused_input_and_never_repeats_completed_work() {
    let peer = peer(Fixture::default()).await;
    let dir = Temp::new();
    let result = craft_once(
        &peer.client,
        "fixture:recipe",
        "one",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(result.status, CraftStatus::Completed);
    assert_eq!(result.current_inventory_count, Some(4));
    assert_eq!(result.action_id.as_deref(), Some("one-store"));
    let again = craft_once(
        &peer.client,
        "fixture:recipe",
        "one",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(again.status, CraftStatus::AlreadyCompleted);
    {
        let fixture = peer.state.lock().unwrap();
        assert_eq!(fixture.posts.len(), 3);
        assert_eq!(
            item_count(&observation(fixture.step), "fixture:input").unwrap(),
            7
        );
    }
    peer.stop().await;
}
#[tokio::test]
async fn resumed_take_is_status_only_then_stores_once_without_repeating_prepare_or_take() {
    let dir = Temp::new();
    {
        let mut journal = Journal::open(
            &dir.path(),
            "instance",
            "own",
            "fixture-life",
            "fixture:recipe",
            "resume",
        )
        .unwrap();
        journal.state.output = Some(
            Output::bind(
                "fixture:recipe",
                &receipt("resume-prepare", true, Outcome::Observed),
            )
            .unwrap(),
        );
        journal.state.phase = CraftPhase::Take;
        journal.state.pending = Some(build(&mut journal.state, &observation(1)).unwrap());
        journal.save().unwrap();
    }
    let mut fixture = Fixture {
        step: 2,
        ..Fixture::default()
    };
    fixture.receipts.insert(
        "resume-take".into(),
        receipt("resume-take", false, Outcome::Observed),
    );
    let peer = peer(fixture).await;
    let result = craft_once(
        &peer.client,
        "fixture:recipe",
        "resume",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(result.status, CraftStatus::Completed);
    {
        let fixture = peer.state.lock().unwrap();
        assert_eq!(fixture.posts.len(), 1);
        assert_eq!(fixture.posts[0].action_id, "resume-store");
        assert!(
            fixture
                .methods
                .iter()
                .any(|m| m == "GET /v1/bots/own-bot/actions/resume-take HTTP/1.1")
        );
    }
    peer.stop().await;
}
#[tokio::test]
async fn uncertain_and_reconciled_steps_stop_without_replay_or_advancing_to_output() {
    let peer = peer(Fixture {
        uncertain: true,
        ..Fixture::default()
    })
    .await;
    let dir = Temp::new();
    for _ in 0..2 {
        let r = craft_once(
            &peer.client,
            "fixture:recipe",
            "uncertain",
            &dir.path(),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert_eq!(r.status, CraftStatus::NeedsInspection);
        assert_eq!(r.action_id.as_deref(), Some("uncertain-prepare"));
    }
    peer.client.reconcile("uncertain-prepare").await.unwrap();
    let r = craft_once(
        &peer.client,
        "fixture:recipe",
        "uncertain",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(r.receipt_outcome, Some(Outcome::Reconciled));
    assert_eq!(r.status, CraftStatus::NeedsInspection);
    assert_eq!(peer.state.lock().unwrap().posts.len(), 1);
    peer.stop().await;
}
#[tokio::test]
async fn reused_id_delivery_cannot_be_mistaken_for_a_new_preparation() {
    let peer = peer(Fixture {
        reused: true,
        ..Fixture::default()
    })
    .await;
    let dir = Temp::new();
    let r = craft_once(
        &peer.client,
        "fixture:recipe",
        "reused",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(r.status, CraftStatus::NeedsInspection);
    assert_eq!(peer.state.lock().unwrap().posts.len(), 1);
    peer.stop().await;
}
#[test]
fn stale_output_cursor_and_full_inventory_are_explicit_stops() {
    let output =
        Output::bind("fixture:recipe", &receipt("proof", true, Outcome::Observed)).unwrap();
    let mut o = observation(1);
    assert!(output.ready(&o).is_ok());
    o.menu["state_id"] = json!(99);
    assert!(output.ready(&o).is_ok());
    o.menu["menu_id"] = json!(18);
    assert!(output.ready(&o).is_err());
    o = observation(1);
    o.menu["slots"][0]["may_pickup"] = json!(false);
    assert!(output.ready(&o).is_err());
    assert!(empty_grid(&observation(1)).is_err());
    assert!(empty_grid(&observation(2)).is_err());
    let dir = Temp::new();
    let mut journal = Journal::open(
        &dir.path(),
        "instance",
        "own",
        "fixture-life",
        "fixture:recipe",
        "full",
    )
    .unwrap();
    journal.state.phase = CraftPhase::Store;
    journal.state.output = Some(output);
    o = observation(2);
    o.menu["slots"][3]["may_place_carried"] = json!(false);
    assert!(build(&mut journal.state, &o).is_err());
    assert!(journal.state.pending.is_none());
}
#[test]
fn journal_is_private_exclusive_scoped_and_preserves_pending_identity() {
    let dir = Temp::new();
    let mut journal = Journal::open(
        &dir.path(),
        "instance",
        "own",
        "fixture-life",
        "fixture:recipe",
        "scope",
    )
    .unwrap();
    assert!(
        Journal::open(
            &dir.path(),
            "instance",
            "own",
            "fixture-life",
            "fixture:recipe",
            "scope"
        )
        .is_err()
    );
    let request = build(&mut journal.state, &observation(0)).unwrap();
    journal.state.pending = Some(request);
    journal.save().unwrap();
    drop(journal);
    assert!(
        Journal::open(
            &dir.path(),
            "other",
            "own",
            "fixture-life",
            "fixture:recipe",
            "scope"
        )
        .is_err()
    );
    assert!(
        Journal::open(
            &dir.path(),
            "instance",
            "other",
            "fixture-life",
            "fixture:recipe",
            "scope"
        )
        .is_err()
    );
    assert!(
        Journal::open(
            &dir.path(),
            "instance",
            "own",
            "new-life",
            "fixture:recipe",
            "scope"
        )
        .is_err()
    );
    let restored = Journal::open(
        &dir.path(),
        "instance",
        "own",
        "fixture-life",
        "fixture:recipe",
        "scope",
    )
    .unwrap();
    assert_eq!(restored.state.pending.unwrap().action_id, "scope-prepare");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}

#[tokio::test]
async fn legacy_provider_is_rejected_without_creating_journal_or_mutating() {
    let peer = peer(Fixture {
        missing_life: true,
        ..Fixture::default()
    })
    .await;
    let dir = Temp::new();
    let error = craft_once(
        &peer.client,
        "fixture:recipe",
        "legacy",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedCapability);
    assert!(!dir.path().exists());
    assert!(peer.state.lock().unwrap().posts.is_empty());
    peer.stop().await;
}

#[tokio::test]
async fn same_menu_and_matching_output_in_another_life_never_advances_or_resumes() {
    let peer = peer(Fixture {
        respawn_after_prepare: true,
        ..Fixture::default()
    })
    .await;
    let dir = Temp::new();
    for _ in 0..2 {
        assert!(
            craft_once(
                &peer.client,
                "fixture:recipe",
                "death",
                &dir.path(),
                Duration::from_secs(3)
            )
            .await
            .is_err()
        );
        assert_eq!(peer.state.lock().unwrap().posts.len(), 1);
    }
    let state: State = serde_json::from_slice(&fs::read(dir.path()).unwrap()).unwrap();
    assert_eq!(state.phase, CraftPhase::Prepare);
    assert_eq!(state.life_id, "fixture-life");
    assert_eq!(state.pending.unwrap().action_id, "death-prepare");
    peer.stop().await;
}

#[tokio::test]
async fn interrupted_running_future_preserves_original_request_for_status_only_resume() {
    let peer = peer(Fixture {
        running: true,
        ..Fixture::default()
    })
    .await;
    let dir = Temp::new();
    let path = dir.path();
    let mut running = Box::pin(craft_once(
        &peer.client,
        "fixture:recipe",
        "running",
        &path,
        Duration::from_secs(3),
    ));
    loop {
        tokio::select! {
            result=&mut running=>panic!("Expected a pending native action, got{result:?}"),
            _=tokio::time::sleep(Duration::from_millis(1))=>if peer.state.lock().unwrap().posts.len()==1 {break;}
        }
    }
    drop(running);
    {
        let mut fixture = peer.state.lock().unwrap();
        assert_eq!(fixture.posts.len(), 1);
        fixture.running = false;
        let receipt = fixture.receipts.get_mut("running-prepare").unwrap();
        receipt.outcome = Outcome::Observed;
        receipt.evidence["native_effect_verified"] = json!(true);
    }
    let result = craft_once(
        &peer.client,
        "fixture:recipe",
        "running",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(result.status, CraftStatus::Completed);
    assert_eq!(peer.state.lock().unwrap().posts.len(), 3);
    peer.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn local_preemption_forwards_nothing_and_can_retry_same_intention_later() {
    use tokio::io::AsyncBufReadExt;
    struct SocketFile(PathBuf);
    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let dir = Temp::new();
    let mut peer = peer(Fixture::default()).await;
    let path = std::env::temp_dir().join(format!(
        "mcc{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let socket_file = SocketFile(path.clone());
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    peer.client.local_arbiter = Some(path);
    let arbiter = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio::io::BufReader::new(socket);
        let mut line = String::new();
        socket.read_line(&mut line).await.unwrap();
        let request: LocalActionRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(request.request.action_id, "preempt-prepare");
        let mut response = serde_json::to_vec(&LocalActionResponse::Error(ApiError::new(
            ErrorCode::LocalPreempted,
            "Body owns urgent control; no mutation submitted",
        )))
        .unwrap();
        response.push(b'\n');
        socket.get_mut().write_all(&response).await.unwrap();
    });
    let result = craft_once(
        &peer.client,
        "fixture:recipe",
        "preempt",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(result.status, CraftStatus::LocalPreempted);
    assert_eq!(peer.state.lock().unwrap().posts.len(), 0);
    arbiter.await.unwrap();
    peer.client.local_arbiter = None;
    let result = craft_once(
        &peer.client,
        "fixture:recipe",
        "preempt",
        &dir.path(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(result.status, CraftStatus::Completed);
    assert_eq!(peer.state.lock().unwrap().posts.len(), 3);
    peer.stop().await;
    drop(socket_file);
}
