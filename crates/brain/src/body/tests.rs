use super::*;
#[tokio::test]
async fn stalled_native_query_does_not_stop_observation_or_close_threat_defense() {
    // Deliberately motionless transport fixture; this does not certify game
    // movement. Hold the query socket open, then introduce an urgent threat.
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Arc::new(
        Client::with_profile(&Profile {
            endpoint: format!("http://{}/", http.local_addr().unwrap()),
            instance_id: "fixture".into(),
            credential: Credential {
                bot_id: "own".into(),
                player_uuid: "own".into(),
                key: "fixture".into(),
            },
            session: None,
        })
        .unwrap()
        .without_local_arbiter(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let upstream = tokio::spawn(async move {
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for z in -4..=4 {
                for y in -1..=4 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else{"minecraft:air"},
                "collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        let mut tick = 0;
        let mut held_queries = Vec::new();
        let mut observed_after_query = 0;
        let mut moved_after_query = false;
        let mut control = json!({});
        loop {
            let (mut socket, _) = http.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
                assert!(header.len() < 8192);
            }
            let header = String::from_utf8(header).unwrap();
            let first = header.lines().next().unwrap();
            let size = header
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(size < 8192);
            let mut bytes = vec![0; size];
            socket.read_exact(&mut bytes).await.unwrap();
            if first.starts_with("POST /v1/bots/own/observations/query ") {
                held_queries.push(socket);
                continue;
            }
            let mut finished = false;
            let response = if first.starts_with("GET /v1/bots/own/observations ") {
                tick += 5;
                if !held_queries.is_empty() {
                    observed_after_query += 1;
                }
                json!({"revision":tick.to_string(),"player":{"uuid":"own","entity_id":1,
                    "dimension":"minecraft:overworld","position":[0.5,1.,0.5],"health":20.,"food":20,
                    "sprinting":true,"on_ground":true,"attack_strength":1.,"control":control},
                    "world":{"game_time":tick,"sample_clock":{"epoch":"fixture"},"blocks":blocks,
                        "entities":[{"entity_id":9,"visible":true,"hostile":true,
                            "position":[if held_queries.is_empty(){6.5}else{2.5},1.,0.5]}]},"menu":{}})
            } else {
                assert!(first.starts_with("POST /v1/bots/own/actions "), "{first}");
                let request: ActionRequest = serde_json::from_slice(&bytes).unwrap();
                let mut outcome = "observed";
                match request.action {
                    Action::StartMove { target, .. } => {
                        if !held_queries.is_empty() {
                            moved_after_query = true;
                        }
                        control = json!({"action_id":request.action_id,"outcome":"observed","target":[target.x,target.y,target.z]});
                        outcome = "running";
                    }
                    Action::Attack { entity } => {
                        assert_eq!(entity, 9);
                        assert!(!held_queries.is_empty());
                        finished = true;
                    }
                    other => panic!("Unexpected control {other:?}"),
                }
                json!({"action_id":request.action_id,"outcome":outcome,"adapter_version":"fixture","evidence":{},"error":null})
            };
            let body = response.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            if finished {
                stopped.store(true, Ordering::Release);
                break;
            }
        }
        assert_eq!(held_queries.len(), 1);
        assert!(observed_after_query > 0);
        assert!(moved_after_query);
    });
    let shared = Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    });
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("body.sqlite")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        controller(
            client,
            Config {
                enabled: true,
                period_ms: 50,
            },
            shared,
            stop,
            &j,
            3.5,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    upstream.await.unwrap();
}
#[tokio::test]
async fn planning_refresh_waits_for_a_fresh_retry_without_admitting_stale_state() {
    let (_root, mut brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
    let mut observed = brain.observed.clone();
    observed.revision = "old".into();
    let shared = Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(Some((
            Instant::now() - Duration::from_secs(4),
            Arc::new(observed.clone()),
        ))),
        reason: Mutex::new("recovering observation transport".into()),
        failed: tokio::sync::Notify::new(),
    });
    assert!(
        shared.observe().is_err(),
        "Immediate gameplay reads must remain fenced"
    );
    brain.body = Some(Handle {
        shared: shared.clone(),
        socket: brain.cfg.state.join("fixture.sock"),
        stop: Arc::new(AtomicBool::new(false)),
        thread: None,
    });
    let updating = shared.clone();
    let update = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        observed.revision = "fresh-after-retry".into();
        *updating.latest.lock().unwrap() = Some((Instant::now(), Arc::new(observed)));
    });
    brain.refresh().await.unwrap();
    update.await.unwrap();
    assert_eq!(brain.observed.revision, "fresh-after-retry");
    {
        let mut latest = shared.latest.lock().unwrap();
        latest.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(4);
    }
    assert!(
        shared
            .wait_fresh_for(Duration::from_millis(40))
            .await
            .is_err()
    );
    shared.running.store(false, Ordering::Release);
    shared.failed.notify_one();
    assert!(
        shared
            .wait_fresh()
            .await
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shared.failed.notified())
            .await
            .is_ok(),
        "The supervisor must retain its failure notification"
    );
}
#[tokio::test]
async fn controller_defends_without_ending_escape_then_cancels_a_stuck_route() {
    controller_projectile_scenario(false).await;
    controller_projectile_scenario(true).await;
}
async fn controller_projectile_scenario(replace_projectile: bool) {
    // Transport fixture only: the game is deliberately held at one position
    // while its tick counter advances. This is not native gameplay evidence.
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Arc::new(
        Client::with_profile(&Profile {
            endpoint: format!("http://{}/", http.local_addr().unwrap()),
            instance_id: "fixture".into(),
            credential: Credential {
                bot_id: "own".into(),
                player_uuid: "own".into(),
                key: "fixture".into(),
            },
            session: None,
        })
        .unwrap()
        .without_local_arbiter(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let upstream = tokio::spawn(async move {
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for z in -4..=4 {
                for y in -1..=4 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else{"minecraft:air"},"collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        let mut tick = 100;
        let mut control = json!({});
        let mut actions = Vec::new();
        let mut defended_while_moving = false;
        loop {
            let (mut socket, _) = http.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
                assert!(header.len() < 8192);
            }
            let header = String::from_utf8(header).unwrap();
            let first = header.lines().next().unwrap();
            let size = header
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(size < 8192);
            let mut body = vec![0; size];
            socket.read_exact(&mut body).await.unwrap();
            let mut finished = false;
            let response = if first.starts_with("GET /v1/bots/own/observations ") {
                tick += 4;
                serde_json::to_value(Observation {
                    revision:tick.to_string(),
                    player:json!({"uuid":"own","entity_id":1,"dimension":"minecraft:overworld","position":[0.5,1.,0.5],"health":20.,"food":20,"sprinting":true,"attack_strength":1.,"control":control}),
                    world:json!({"game_time":tick,"sample_clock":{"epoch":"fixture"},"blocks":blocks,"entities":[{"entity_id":9,"visible":true,"hostile":true,"position":[2.5,1.,0.5]},{"entity_id":if replace_projectile && tick>=116{11}else{10},"uuid":if replace_projectile && tick>=116{"new-projectile-fixture"}else{"projectile-fixture"},"visible":true,"projectile":true,"position":[2.5,1.9,0.5],"velocity":[-0.25,0.,0.]}]}),
                    menu:json!({}),
                }).unwrap()
            } else {
                assert!(first.starts_with("POST /v1/bots/own/actions "), "{first}");
                let request: ActionRequest = serde_json::from_slice(&body).unwrap();
                let mut outcome = "observed";
                match &request.action {
                    Action::StartMove { target, .. } => {
                        control = json!({"action_id":request.action_id,"outcome":"running","target":[target.x,target.y,target.z]});
                        outcome = "running";
                    }
                    Action::Attack { entity } => {
                        assert_eq!(*entity, 9);
                        defended_while_moving = control["outcome"] == "running";
                    }
                    Action::CancelControl { action_id } => {
                        if replace_projectile {
                            assert!(
                                (116..124).contains(&tick),
                                "A new incoming projectile should replan before the stuck window"
                            );
                        } else {
                            assert!(
                                tick >= 124,
                                "Repeated samples of the same projectile must not cancel the dodge before the native stuck window"
                            );
                        }
                        assert_eq!(control["action_id"], *action_id);
                        assert!(defended_while_moving);
                        finished = true;
                    }
                    other => panic!("unexpected action {other:?}"),
                }
                actions.push(request.action);
                json!({"action_id":request.action_id,"outcome":outcome,"adapter_version":"fixture","evidence":{"native_effect_verified":outcome=="observed"},"error":null})
            };
            let body = response.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            if finished {
                stopped.store(true, Ordering::Release);
                break;
            }
        }
        actions
    });
    let shared = Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    });
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(&dir.path().join("body.sqlite")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        controller(
            client,
            Config {
                enabled: true,
                period_ms: 50,
            },
            shared,
            stop,
            &journal,
            3.5,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let actions = upstream.await.unwrap();
    assert!(matches!(actions.first(), Some(Action::StartMove { .. })));
    assert!(matches!(actions.last(), Some(Action::CancelControl { .. })));
}
#[tokio::test]
async fn delayed_interactions_return_final_feedback_without_leasing_the_survival_gate() {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Arc::new(
        Client::with_profile(&Profile {
            endpoint: format!("http://{}/", http.local_addr().unwrap()),
            instance_id: "fixture".into(),
            credential: Credential {
                bot_id: "own".into(),
                player_uuid: "own".into(),
                key: "fixture-key".into(),
            },
            session: None,
        })
        .unwrap()
        .without_local_arbiter(),
    );
    let shared = Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    });
    let gate = shared.clone();
    let upstream = tokio::spawn(async move {
        let mut methods = Vec::new();
        for phase in 0..2 {
            let (mut socket, _) = http.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
                assert!(header.len() < 8192);
            }
            let header = String::from_utf8(header).unwrap();
            let first = header.lines().next().unwrap().to_owned();
            methods.push(first.clone());
            let size = header
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(size < 8192);
            let mut body = vec![0; size];
            socket.read_exact(&mut body).await.unwrap();
            if phase == 0 {
                assert!(first.starts_with("POST /v1/bots/own/actions "));
                assert_eq!(
                    serde_json::from_slice::<ActionRequest>(&body)
                        .unwrap()
                        .action_id,
                    "delayed-program"
                );
            } else {
                assert!(first.starts_with("GET /v1/bots/own/actions/delayed-program "));
                assert!(
                    gate.mutation.try_lock().is_ok(),
                    "Waiting for a game effect must not block survival preemption"
                );
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            let body=json!({"action_id":"delayed-program","outcome":if phase==0{"running"}else{"observed"},"adapter_version":"fixture","evidence":{"native_effect_verified":phase==1},"error":null}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        methods
    });
    let (mut caller, server) = tokio::net::UnixStream::pair().unwrap();
    let state = shared.clone();
    let task = tokio::spawn(async move { proxy(server, client, state).await });
    let request = LocalActionRequest {
        version: LOCAL_CONTROL_VERSION,
        request: ActionRequest {
            action_id: "delayed-program".into(),
            action: Action::UseItem { hand: Hand::Main },
        },
    };
    let mut bytes = serde_json::to_vec(&request).unwrap();
    bytes.push(b'\n');
    caller.write_all(&bytes).await.unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), caller.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    let response: LocalActionResponse = serde_json::from_slice(&reply).unwrap();
    assert!(
        matches!(response,LocalActionResponse::Receipt(r) if r.outcome==Outcome::Observed && r.evidence["native_effect_verified"]==true),
        "An initial running dispatch is not the result of a completed experiment"
    );
    task.await.unwrap().unwrap();
    let methods = upstream.await.unwrap();
    assert_eq!(methods.len(), 2);
    let trace = shared.feedback.lock().unwrap().since(0);
    assert_eq!(trace["requests"][0]["response"]["outcome"], "observed");
    assert_eq!(
        trace["requests"][0]["response"]["native_effect_verified"],
        true
    );
}
#[tokio::test]
async fn real_local_proxy_returns_the_receipt_and_retains_only_compact_diagnostics() {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", http.local_addr().unwrap());
    let reply = json!({"action_id":"program-probe","outcome":"observed","adapter_version":"fixture","evidence":{"native_effect_verified":true,"after":{"world":"x".repeat(10000)}},"error":null});
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = http.accept().await.unwrap();
        let mut buffer = [0u8; 4096];
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(String::from_utf8_lossy(&buffer[..n]).contains("/v1/bots/own/actions"));
        let body = reply.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let client = Arc::new(
        Client::with_profile(&Profile {
            endpoint,
            instance_id: "fixture".into(),
            credential: Credential {
                bot_id: "own".into(),
                player_uuid: "own".into(),
                key: "fixture-key".into(),
            },
            session: Some(Session {
                session_id: "fixture-session".into(),
                adapter_version: "fixture".into(),
            }),
        })
        .unwrap()
        .without_local_arbiter(),
    );
    let shared = Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    });
    let (mut caller, server) = tokio::net::UnixStream::pair().unwrap();
    let state = shared.clone();
    let task = tokio::spawn(async move { proxy(server, client, state).await });
    let request = LocalActionRequest {
        version: LOCAL_CONTROL_VERSION,
        request: ActionRequest {
            action_id: "program-probe".into(),
            action: Action::Look { yaw: 1., pitch: 2. },
        },
    };
    let mut bytes = serde_json::to_vec(&request).unwrap();
    bytes.push(b'\n');
    caller.write_all(&bytes).await.unwrap();
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), caller.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    let response: LocalActionResponse = serde_json::from_slice(&received).unwrap();
    assert!(
        matches!(response,LocalActionResponse::Receipt(r) if r.outcome==Outcome::Observed && r.evidence["after"]["world"].as_str().unwrap().len()==10000)
    );
    task.await.unwrap().unwrap();
    upstream.await.unwrap();
    let trace = shared.feedback.lock().unwrap().since(0);
    assert_eq!(
        trace["requests"][0]["request"]["action_id"],
        "program-probe"
    );
    assert_eq!(trace["requests"][0]["response"]["outcome"], "observed");
    assert!(trace.to_string().len() < 1500);
}
#[tokio::test]
async fn stalled_callers_cannot_lease_the_body_and_abandoned_work_never_dispatches() {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Arc::new(
        Client::with_profile(&Profile {
            endpoint: format!("http://{}/", http.local_addr().unwrap()),
            instance_id: "fixture".into(),
            credential: Credential {
                bot_id: "own".into(),
                player_uuid: "own".into(),
                key: "fixture-key".into(),
            },
            session: Some(Session {
                session_id: "fixture-session".into(),
                adapter_version: "fixture".into(),
            }),
        })
        .unwrap()
        .without_local_arbiter(),
    );
    let shared = Arc::new(Shared {
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        emergency: AtomicBool::new(true),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    });
    let (mut caller, server) = tokio::net::UnixStream::pair().unwrap();
    let c = client.clone();
    let s = shared.clone();
    let partial = tokio::spawn(async move { proxy(server, c, s).await });
    caller.write_all(b"{").await.unwrap();
    let guard = tokio::time::timeout(Duration::from_millis(100), shared.mutation.lock())
        .await
        .expect("partial caller must not acquire gameplay authority");
    drop(guard);
    drop(caller);
    assert!(partial.await.unwrap().is_err());

    let request = LocalActionRequest {
        version: LOCAL_CONTROL_VERSION,
        request: ActionRequest {
            action_id: "abandoned".into(),
            action: Action::StartMove {
                target: Position {
                    x: 1.,
                    y: 0.,
                    z: 0.,
                },
                jump: false,
                max_ticks: 20,
            },
        },
    };
    let (mut caller, server) = tokio::net::UnixStream::pair().unwrap();
    let c = client.clone();
    let s = shared.clone();
    let mut queued = tokio::spawn(async move { proxy(server, c, s).await });
    caller
        .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert!(
        !queued.is_finished(),
        "ordinary work must defer during danger"
    );
    drop(caller);
    shared.emergency.store(false, Ordering::Release);
    let ended = tokio::time::timeout(Duration::from_secs(1), &mut queued).await;
    if ended.is_err() {
        let forwarded = tokio::time::timeout(Duration::from_millis(10), http.accept())
            .await
            .is_ok();
        panic!(
            "abandoned caller stuck: forwarded={forwarded}, emergency={}, lock_available={}",
            shared.urgent(),
            shared.mutation.try_lock().is_ok()
        );
    }
    ended.unwrap().unwrap().unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), http.accept())
            .await
            .is_err(),
        "abandoned intention must never reach the gateway"
    );
    assert!(shared.mutation.try_lock().is_ok());

    shared.emergency.store(true, Ordering::Release);
    let (mut caller, server) = tokio::net::UnixStream::pair().unwrap();
    let c = client.clone();
    let state = shared.clone();
    let waiting = tokio::spawn(async move { proxy(server, c, state).await });
    caller
        .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(4), caller.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let response: LocalActionResponse = serde_json::from_slice(&bytes).unwrap();
    assert!(
        matches!(response,LocalActionResponse::Error(e) if e.code==ErrorCode::LocalPreempted && e.message.contains("no mutation was submitted"))
    );
    waiting.await.unwrap().unwrap();
    shared.emergency.store(false, Ordering::Release);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), http.accept())
            .await
            .is_err(),
        "Expired admission must not dispatch later when danger clears"
    );
    let trace = shared.feedback.lock().unwrap().since(0);
    assert_eq!(
        trace["requests"][0]["response"]["error"]["code"],
        "local_preempted"
    );
}
