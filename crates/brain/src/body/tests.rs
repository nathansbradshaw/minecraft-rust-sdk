use super::*;

fn admission_shared() -> Arc<Shared> {
    Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    })
}

#[test]
fn observation_admission_requires_read_started_after_contention_and_unchanged_generation() {
    let shared = admission_shared();
    let now = Instant::now();
    let guard = shared.mutation.try_lock().unwrap();
    let read_began_while_busy = shared.observation_admission();
    drop(guard);
    assert!(
        shared
            .try_observed_mutation(read_began_while_busy, now)
            .is_none()
    );
    let fresh = shared.observation_admission();
    assert!(shared.try_observed_mutation(fresh, now).is_some());
    shared.program_dispatch("original-in-flight").unwrap();
    assert!(shared.try_observed_mutation(fresh, now).is_none());
    let unresolved_but_fresh = shared.observation_admission();
    assert!(
        shared
            .try_observed_mutation(unresolved_but_fresh, now)
            .is_some(),
        "fresh independent defense may coexist with retained uncertainty"
    );
    assert!(
        shared
            .try_observed_mutation(unresolved_but_fresh, now - Duration::from_millis(251))
            .is_none()
    );
    assert_eq!(
        shared.pending_program().unwrap().as_deref(),
        Some("original-in-flight")
    );
    assert!(
        !shared
            .program_generation
            .load(Ordering::Acquire)
            .is_multiple_of(2)
    );
}

#[test]
fn native_admission_rejects_cached_regression_wrong_actor_and_unknown_scope() {
    let make = |life: &str, nanos: &str| Observation {
        revision: nanos.into(),
        menu: Value::Null,
        player: json!({"uuid":"own","entity_id":1,"life_id":life,"dimension":"minecraft:overworld","position":[0.,1.,0.]}),
        world: json!({"sample_clock":{"epoch":life,"nanos":nanos}}),
    };
    let mut state = NativeAdmission {
        generation: 0,
        required: false,
        floor: None,
        actor: Some("own".into()),
        attempted: None,
    };
    let admission = ObservationAdmission {
        generation: 2,
        mutation_free: true,
    };
    state.begin(admission);
    assert!(state.required);
    assert!(state.accept_fresh(&make("life", "200"), admission));
    assert!(!state.accepts_cached(&make("life", "199")));
    assert!(!state.accepts_cached(&make("new-life", "201")));
    assert!(state.accepts_cached(&make("life", "200")));
    assert!(state.accepts_cached(&make("life", "201")));
    let mut wrong = make("life", "201");
    wrong.player["uuid"] = json!("other");
    assert!(!state.accept_fresh(&wrong, admission));
    assert!(!state.accept_fresh(&make("life", ""), admission));
    assert!(!state.accept_fresh(&make("life", "199"), admission));
    assert!(
        state.accept_fresh(&make("new-life", "1"), admission),
        "native authority can establish respawn"
    );
    assert!(!state.accepts_cached(&make("life", "999")));
}

#[test]
fn default_body_requires_native_refresh_capability_before_startup() {
    let root = tempfile::tempdir().unwrap();
    let profile = Profile {
        endpoint: "https://example.invalid/".into(),
        instance_id: "fixture".into(),
        session: None,
        credential: Credential {
            bot_id: "own".into(),
            player_uuid: "own".into(),
            key: "fixture".into(),
        },
    };
    let server = ServerInfo {
        api_version: 1,
        instance_id: "fixture".into(),
        minecraft_version: "1.21.1".into(),
        platform: "neoforge".into(),
        adapter_version: "fixture".into(),
        ready: true,
        limits: json!({}),
        capabilities: [
            "minecraft:finite_locomotion",
            "minecraft:survival_facts",
            "minecraft:ranged_observation",
            "minecraft:pending_actions",
            "minecraft:session_material_preservation",
            auditory::CAPABILITY,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    };
    let initial = Observation {
        revision: "fixture".into(),
        player: Value::Null,
        world: Value::Null,
        menu: Value::Null,
    };
    let error = Handle::start_for_server(profile, Config::default(), root.path(), initial, &server)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains(menu::FRESH_OBSERVATION_CAPABILITY)
    );
    assert!(!root.path().join("actions.sock").exists());
}

#[tokio::test]
async fn slow_terminal_status_keeps_reads_live_and_never_overwrites_unflushed_interaction() {
    // A delayed RESPONSE on an independent connection, not serialized adapter
    // execution: prove the local controller does not await it before movement
    // decisions, nor replace the interaction before its receipt arrives.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Arc::new(
        Client::with_profile(&Profile {
            endpoint: format!("http://{}/", listener.local_addr().unwrap()),
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
    let server_stop = stop.clone();
    let delivered = Arc::new(AtomicBool::new(false));
    let upstream = tokio::spawn(async move {
        let mut reads = 0;
        let mut status = None;
        loop {
            let (mut socket, line, input) = admission_incoming(&listener).await;
            if line.starts_with("GET /v1/bots/own/observations ") {
                reads += 1;
                admission_reply(&mut socket,"200 OK",serde_json::to_value(Observation {
                    revision:reads.to_string(),menu:Value::Null,
                    player:json!({"uuid":"own","entity_id":1,"dimension":"minecraft:overworld","health":20.,"food":20,"position":[0.5,1.,0.5],
                        "attack_strength":1.,"interaction":{"action_id":"old-interaction","outcome":"observed","input_released":true}}),
                    world:json!({"game_time":100+reads,"blocks":[],"entities":[{"entity_id":9,"visible":true,"hostile":true,"type":"minecraft:zombie","position":[2.,1.,0.5]}]}),
                }).unwrap()).await;
            } else if line.starts_with("GET /v1/bots/own/actions/old-interaction ") {
                assert!(status.is_none(), "only one exact status read in flight");
                let delivered = delivered.clone();
                status = Some(tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(320)).await;
                    admission_reply(&mut socket,"200 OK",json!({"action_id":"old-interaction","outcome":"observed","adapter_version":"fixture","evidence":{},"error":null})).await;
                    delivered.store(true, Ordering::Release);
                }));
            } else {
                assert!(line.starts_with("POST /v1/bots/own/actions "), "{line}");
                let request: ActionRequest = serde_json::from_value(input).unwrap();
                assert!(matches!(request.action, Action::Attack { entity: 9 }));
                assert!(
                    delivered.load(Ordering::Acquire),
                    "never overwrite terminal interaction before flush"
                );
                assert!(
                    reads >= 5,
                    "observations progress throughout slow status read"
                );
                admission_reply(&mut socket,"200 OK",json!({"action_id":request.action_id,"outcome":"running","adapter_version":"fixture","evidence":{},"error":null})).await;
                server_stop.store(true, Ordering::Release);
                break;
            }
        }
        status.unwrap().await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(&dir.path().join("body.sqlite")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        controller(
            client,
            Config {
                period_ms: 50,
                ..Config::default()
            },
            admission_shared(),
            stop,
            &journal,
            3.5,
            false,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    upstream.await.unwrap();
}

#[tokio::test]
async fn withheld_post_response_keeps_observing_then_uses_only_the_fresh_control() {
    for replaced in [false, true] {
        withheld_post_scenario(replaced, false, false).await;
    }
}

#[tokio::test]
async fn lost_post_response_and_not_found_retain_original_pending_without_replay() {
    withheld_post_scenario(true, true, false).await;
}

#[tokio::test]
async fn unavailable_native_refresh_never_authorizes_cached_control_or_replays_foreground() {
    withheld_post_scenario(false, false, true).await;
}

async fn admission_incoming(
    listener: &tokio::net::TcpListener,
) -> (tokio::net::TcpStream, String, Value) {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(socket.read_u8().await.unwrap());
        assert!(headers.len() < 8192);
    }
    let headers = std::str::from_utf8(&headers).unwrap();
    let first = headers.lines().next().unwrap().to_owned();
    let size = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(size < 16384);
    let mut body = vec![0; size];
    socket.read_exact(&mut body).await.unwrap();
    (
        socket,
        first,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
    )
}
async fn admission_reply(socket: &mut tokio::net::TcpStream, status: &str, value: Value) {
    let body = value.to_string();
    socket
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}
async fn withheld_post_scenario(replaced: bool, lost_reply: bool, fail_refresh: bool) {
    // Transport fixture, not Survival: hold an actual initial SDK HTTP POST
    // while accepting independent observation connections. This deliberately
    // models a delayed RESPONSE, not a gateway/adapter execution stall.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Arc::new(
        Client::with_profile(&Profile {
            endpoint: format!("http://{}/", listener.local_addr().unwrap()),
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
    let shared = admission_shared();
    let stop = Arc::new(AtomicBool::new(false));
    let (mut caller, server) = tokio::net::UnixStream::pair().unwrap();
    let request = LocalActionRequest {
        version: LOCAL_CONTROL_VERSION,
        request: ActionRequest {
            action_id: "original-post".into(),
            action: Action::UseItem { hand: Hand::Main },
        },
    };
    let done = Arc::new(tokio::sync::Notify::new());
    let (c, s, d) = (client.clone(), shared.clone(), done.clone());
    let proxy_task = tokio::spawn(async move {
        let result = proxy(server, c, s).await;
        d.notify_one();
        result
    });
    caller
        .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
        .await
        .unwrap();
    let (foreground, line, input) = admission_incoming(&listener).await;
    assert!(line.starts_with("POST /v1/bots/own/actions "));
    let submitted: ActionRequest = serde_json::from_value(input).unwrap();
    assert_eq!(submitted.action_id, "original-post");
    assert!(shared.mutation.try_lock().is_err());
    assert_eq!(
        shared.pending_program().unwrap().as_deref(),
        Some("original-post")
    );
    let (upstream_shared, upstream_stop) = (shared.clone(), stop.clone());
    let upstream = tokio::spawn(async move {
        let mut foreground = Some(foreground);
        let mut reads = 0;
        let mut cancels = 0;
        let mut released = false;
        let mut native_attempt = None;
        let mut native_ready = false;
        loop {
            let (mut socket, line, input) = admission_incoming(&listener).await;
            let native_query = line.starts_with("POST /v1/bots/own/observations/query ");
            if line.starts_with("GET /v1/bots/own/observations ") || native_query {
                if !native_query {
                    reads += 1;
                }
                if native_query {
                    assert_eq!(input["capability"], menu::FRESH_OBSERVATION_CAPABILITY);
                    assert!(reads >= 5);
                    if let Some(at) = native_attempt {
                        assert!(Instant::now().duration_since(at) >= Duration::from_millis(990));
                    } else if fail_refresh {
                        native_attempt = Some(Instant::now());
                        admission_reply(
                            &mut socket,
                            "503 Service Unavailable",
                            serde_json::to_value(ApiError::new(
                                ErrorCode::NotReady,
                                "Fresh native read unavailable",
                            ))
                            .unwrap(),
                        )
                        .await;
                        continue;
                    }
                    native_ready = true;
                }
                if reads == 4 && !native_query {
                    assert!(
                        upstream_shared.mutation.try_lock().is_err(),
                        "POST is still genuinely in flight while fresh reads progress"
                    );
                    let mut held = foreground.take().unwrap();
                    if !lost_reply {
                        admission_reply(&mut held,"200 OK",json!({"action_id":"original-post",
                            "outcome":"observed","adapter_version":"fixture","evidence":{},"error":null})).await;
                    }
                    drop(held);
                    done.notified().await;
                    assert!(upstream_shared.mutation.try_lock().is_ok());
                }
                // The fourth read began under the old lock and deliberately
                // returns the OLD scene after the POST has finished. Only the
                // Even a new cached GET can be stale after lock release. Only
                // the typed native query supplies the current life/control.
                let fresh = native_query || released;
                let changed = fresh && replaced;
                let control = if changed {
                    json!({"action_id":"new-life-control","outcome":"observed","input_released":true})
                } else {
                    json!({"action_id":"current-move","outcome":if released{"cancelled"}else{"running"},
                        "input_released":released,"target":[1.5,1.,0.5],"cancel_requested":released})
                };
                let snapshot = serde_json::to_value(Observation {
                    revision:reads.to_string(),menu:Value::Null,
                    player:json!({"uuid":"own","life_id":if changed{"new-life"}else{"old-life"},"entity_id":1,
                        "dimension":"minecraft:overworld","health":20.,"food":20,"position":if changed{[30.,1.,30.]}else{[0.5,1.,0.5]},"control":control}),
                    world:json!({"game_time":100+reads,"sample_clock":{"epoch":if changed{"new-life"}else{"old-life"},"nanos":(100+reads).to_string()},
                        "blocks":[],"entities":if changed || released{json!([])}else{json!([{"entity_id":9,"visible":true,"hostile":true,"position":[2.5,1.,0.5]}])}}),
                }).unwrap();
                admission_reply(
                    &mut socket,
                    "200 OK",
                    if native_query {
                        json!({"format":"minecraft-fresh-observation-v1","observation":snapshot})
                    } else {
                        snapshot
                    },
                )
                .await;
                if (changed || released) && fresh {
                    upstream_stop.store(true, Ordering::Release);
                    if lost_reply {
                        let (mut status, line, _) = admission_incoming(&listener).await;
                        assert!(line.starts_with("GET /v1/bots/own/actions/original-post "));
                        admission_reply(
                            &mut status,
                            "404 Not Found",
                            serde_json::to_value(ApiError::new(
                                ErrorCode::NotFound,
                                "Unknown action ID",
                            ))
                            .unwrap(),
                        )
                        .await;
                    }
                    break;
                }
            } else {
                assert!(line.starts_with("POST /v1/bots/own/actions "), "{line}");
                let action: ActionRequest = serde_json::from_value(input).unwrap();
                assert!(
                    reads >= 5,
                    "must fetch a fresh scene after foreground admission becomes available"
                );
                assert!(
                    native_ready,
                    "cached frames never replace a failed native query"
                );
                assert!(
                    !replaced,
                    "must not cancel an old life's control from the stale fourth response"
                );
                assert!(
                    matches!(action.action,Action::CancelControl{ref action_id} if action_id=="current-move")
                );
                cancels += 1;
                assert_eq!(cancels, 1);
                assert_ne!(action.action_id, "original-post");
                released = true;
                admission_reply(
                    &mut socket,
                    "200 OK",
                    json!({"action_id":action.action_id,"outcome":"running",
                    "adapter_version":"fixture","evidence":{},"error":null}),
                )
                .await;
            }
        }
        assert!(reads >= 5);
        assert_eq!(cancels, usize::from(!replaced));
    });
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(&dir.path().join("body.sqlite")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        controller(
            client.clone(),
            Config {
                enabled: true,
                period_ms: 50,
                decision_samples: 0,
                experimental_ranged_boundary_turns: false,
            },
            shared.clone(),
            stop,
            &journal,
            3.5,
            false,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    proxy_task.await.unwrap().unwrap();
    let mut bytes = Vec::new();
    caller.read_to_end(&mut bytes).await.unwrap();
    let response: LocalActionResponse = serde_json::from_slice(&bytes).unwrap();
    if lost_reply {
        assert!(matches!(response, LocalActionResponse::Error(_)));
        assert_eq!(
            client
                .action_status("original-post")
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            shared.pending_program().unwrap().as_deref(),
            Some("original-post")
        );
        assert!(
            !shared
                .program_generation
                .load(Ordering::Acquire)
                .is_multiple_of(2)
        );
    } else {
        assert!(matches!(response,LocalActionResponse::Receipt(r) if r.outcome==Outcome::Observed));
        assert!(shared.pending_program().unwrap().is_none());
    }
    upstream.await.unwrap();
}

#[test]
fn visible_blast_invalidates_only_approaching_exposed_targets_without_audio() {
    let mut observation = Observation {
        revision: "new-visible-fuse".into(),
        player: json!({"position":[0.,1.,0.],"control":{"target":[3.,1.,0.]}}),
        world: json!({"entities":[{"visible":true,"position":[8.,1.,0.],
            "creeper":{"ignited":true,"powered":true}}]}),
        menu: Value::Null,
    };
    assert!(toward_visible_blast(&observation));
    assert!(hearing::blasts(&observation).is_empty());
    for target in [[-3., 1., 0.], [0., 1., 3.], [0., 1., 0.]] {
        observation.player["control"]["target"] = json!(target);
        assert!(
            !toward_visible_blast(&observation),
            "outward/tangent/idle {target:?}"
        );
    }
    observation.player["control"]["target"] = json!([3., 1., 0.]);
    for (field, value) in [
        ("visible", json!(false)),
        ("position", Value::Null),
        ("creeper", json!({"powered":true})),
        ("creeper", json!({"powered":true,"swell_direction":-1})),
    ] {
        let mut hidden_or_calm = observation.clone();
        hidden_or_calm.world["entities"][0][field] = value;
        assert!(!toward_visible_blast(&hidden_or_calm), "{field}");
    }
    observation.world["entities"][0]["creeper"] = json!({"powered":false,"swell_direction":1});
    assert!(
        toward_visible_blast(&observation),
        "normal fuse uses its six-block radius"
    );
    observation.world["entities"][0]["position"] = json!([10., 1., 0.]);
    assert!(
        !toward_visible_blast(&observation),
        "approaching outside blast clearance is not exposed yet"
    );
    observation.player["control"]["target"] = Value::Null;
    assert!(!toward_visible_blast(&observation));
}

#[tokio::test]
async fn newly_visible_blast_cancels_owned_approach_once_before_a_fresh_escape() {
    controller_visible_blast_scenario(true).await;
}

#[tokio::test]
async fn newly_visible_blast_preserves_owned_outward_escape() {
    controller_visible_blast_scenario(false).await;
}

async fn controller_visible_blast_scenario(approaching: bool) {
    // Typed HTTP/controller fixture only, not a game or a survival claim.
    // First establish genuine controller ownership, then show a fuse without
    // delivering a sound. Keep the old native input held until release is read.
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
    let shared = Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let upstream = tokio::spawn(async move {
        let mut blocks = Vec::new();
        for x in -5..=5 {
            for z in -5..=5 {
                for y in 0..=4 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else{"minecraft:air"},
                "collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        let start = [0.5, 1., 0.5];
        let mut control = json!({});
        let mut tick = 100;
        let mut moves = Vec::new();
        let mut cancels = Vec::new();
        let mut terminal_release_seen = false;
        let mut held_samples = 0;
        let mut after_start_samples = 0;
        loop {
            let (mut socket, _) = http.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
                assert!(headers.len() < 8192);
            }
            let headers = std::str::from_utf8(&headers).unwrap();
            let first = headers.lines().next().unwrap();
            let size = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(size < 8192);
            let mut body = vec![0; size];
            socket.read_exact(&mut body).await.unwrap();
            let mut finished = false;
            let response = if first.starts_with("GET /v1/bots/own/observations ") {
                tick += 1;
                let entities = if moves.is_empty() {
                    json!([{"entity_id":9,"visible":true,"hostile":true,"ranged_attack":true,"position":[10.5,1.,0.5]}])
                } else {
                    after_start_samples += 1;
                    let target = geometry::point(&control["target"]).unwrap();
                    let length = (0..3)
                        .map(|i| (target[i] - start[i]).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    assert!(length > 0.2);
                    let sign = if approaching { 1. } else { -1. };
                    let origin = std::array::from_fn::<_, 3, _>(|i| {
                        start[i] + sign * 8. * (target[i] - start[i]) / length
                    });
                    if !cancels.is_empty() {
                        held_samples += 1;
                        if held_samples >= 3 {
                            control["outcome"] = json!("cancelled");
                            control["input_released"] = json!(true);
                            terminal_release_seen = true;
                        }
                    }
                    if !approaching && after_start_samples >= 5 {
                        finished = true;
                    }
                    json!([{"entity_id":10,"visible":true,"hostile":true,"position":origin,
                        "creeper":{"ignited":true,"powered":true}}])
                };
                serde_json::to_value(Observation {
                    revision:tick.to_string(), menu:Value::Null,
                    player:json!({"uuid":"own","entity_id":1,"dimension":"minecraft:overworld","health":20.,
                        "position":start,"food":20,"sprinting":true,"control":control}),
                    world:json!({"game_time":tick,"sample_clock":{"epoch":"same-life"},"entities":entities,"blocks":blocks}),
                }).unwrap()
            } else {
                assert!(first.starts_with("POST /v1/bots/own/actions "), "{first}");
                let request: ActionRequest = serde_json::from_slice(&body).unwrap();
                let outcome = match request.action {
                    Action::StartMove {
                        target, max_ticks, ..
                    } => {
                        assert!(max_ticks <= 60);
                        if !moves.is_empty() {
                            assert!(
                                approaching && terminal_release_seen,
                                "no replacement before observing terminal release"
                            );
                            assert_ne!(
                                moves[0], request.action_id,
                                "never replay the old intention"
                            );
                            finished = true;
                        }
                        moves.push(request.action_id.clone());
                        control = json!({"action_id":request.action_id,"outcome":"running","input_released":false,
                            "cancel_requested":false,"target":[target.x,target.y,target.z]});
                        "running"
                    }
                    Action::CancelControl { action_id } => {
                        assert!(approaching);
                        assert_eq!(action_id, moves[0]);
                        assert!(
                            cancels.is_empty(),
                            "only one cancellation while native release is pending"
                        );
                        assert!(tick < 114, "new fuse preempts before the old stuck timer");
                        cancels.push(request.action_id.clone());
                        control["cancel_requested"] = json!(true);
                        "running"
                    }
                    other => panic!("unexpected action {other:?}"),
                };
                json!({"action_id":request.action_id,"outcome":outcome,"adapter_version":"fixture","evidence":{},"error":null})
            };
            let body = response.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            if finished {
                stopped.store(true, Ordering::Release);
                break;
            }
        }
        assert_eq!(cancels.len(), usize::from(approaching));
        assert_eq!(moves.len(), if approaching { 2 } else { 1 });
        assert!(!approaching || held_samples >= 3);
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
                decision_samples: 0,
                experimental_ranged_boundary_turns: false,
            },
            shared,
            stop,
            &journal,
            3.5,
            false,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    upstream.await.unwrap();
}

#[test]
fn reconciled_owned_shore_hands_off_without_certifying_its_historical_effect() {
    let observation = Observation {
        revision: "settled-after-interruption".into(),
        menu: Value::Null,
        world: json!({"game_time":10}),
        player: json!({"eyes_in_water":false,"air":300,"max_air":300,
            "control":{"action_id":"owned-shore","outcome":"idle","input_released":true,
                "historical_outcome":"uncertain","reconciliation":{"settled":true,
                    "historical_effect_verified":false,"replayed":false}}}),
    };
    let before = observation.clone();
    assert!(may_resume_after_danger(
        &observation,
        Duration::from_millis(50),
        "owned-shore",
        true,
        true
    ));
    assert!(may_resume_after_danger(
        &observation,
        Duration::from_secs(30),
        "owned-shore",
        true,
        true
    ));
    assert_eq!(
        serde_json::to_value(&observation).unwrap(),
        serde_json::to_value(before).unwrap(),
        "readiness does not rewrite native observations or historical uncertainty"
    );
    assert!(
        serde_json::from_value::<Outcome>(observation.player["control"]["outcome"].clone())
            .is_err(),
        "idle remains scheduling metadata, not a successful action outcome"
    );
    for (pointer, value) in [
        ("/outcome", json!("uncertain")),
        ("/input_released", json!(false)),
        ("/input_released", Value::Null),
        ("/historical_outcome", json!("observed")),
        ("/historical_outcome", Value::Null),
        ("/reconciliation", Value::Null),
        ("/reconciliation/settled", json!(false)),
        ("/reconciliation/settled", Value::Null),
        ("/reconciliation/historical_effect_verified", json!(true)),
        ("/reconciliation/historical_effect_verified", Value::Null),
        ("/reconciliation/replayed", json!(true)),
        ("/reconciliation/replayed", Value::Null),
    ] {
        let mut malformed = observation.clone();
        *malformed.player["control"].pointer_mut(pointer).unwrap() = value;
        assert!(
            !may_resume_after_danger(
                &malformed,
                Duration::from_secs(30),
                "owned-shore",
                true,
                true
            ),
            "unconfirmed idle field {pointer} must not release owned shore urgency"
        );
    }
    let mut replacement = observation.clone();
    replacement.player["control"]["action_id"] = json!("another-control");
    assert!(
        !may_resume_after_danger(
            &replacement,
            Duration::from_millis(50),
            "owned-shore",
            true,
            true
        ),
        "another control's reconciliation cannot claim this immediate handoff"
    );
}

#[test]
fn shore_handoff_requires_terminal_native_release_despite_elapsed_wall_time() {
    let mut observed = Observation {
        revision: "same-native-sample".into(),
        menu: Value::Null,
        world: json!({"game_time":10}),
        player: json!({"eyes_in_water":false,"air":300,"max_air":300,
            "control":{"action_id":"owned-shore","outcome":"running","input_released":false}}),
    };
    for outcome in ["prepared", "dispatched", "running", "uncertain", "unknown"] {
        for release in [Value::Null, json!(false), json!(true)] {
            observed.player["control"]["outcome"] = json!(outcome);
            observed.player["control"]["input_released"] = release;
            assert!(
                !may_resume_after_danger(
                    &observed,
                    Duration::from_secs(30),
                    "owned-shore",
                    true,
                    true,
                ),
                "A wall timer or release flag alone cannot retire {outcome} native input"
            );
        }
    }
    for outcome in ["observed", "cancelled", "expired", "failed", "reconciled"] {
        observed.player["control"]["outcome"] = json!(outcome);
        for release in [Value::Null, json!(false)] {
            observed.player["control"]["input_released"] = release;
            assert!(!may_resume_after_danger(
                &observed,
                Duration::from_secs(30),
                "owned-shore",
                true,
                true,
            ));
        }
        observed.player["control"]["input_released"] = json!(true);
        assert!(may_resume_after_danger(
            &observed,
            Duration::from_millis(50),
            "owned-shore",
            true,
            true,
        ));
        assert!(
            may_resume_after_danger(
                &observed,
                Duration::from_millis(50),
                "owned-shore",
                true,
                false,
            ),
            "Proven breathing-only release still hands off immediately"
        );
    }
    observed.player["control"] = json!({"action_id":"replacement","outcome":"running"});
    assert!(
        may_resume_after_danger(
            &observed,
            Duration::from_secs(30),
            "owned-shore",
            true,
            true,
        ),
        "Replacement controls do not inherit old shore ownership"
    );
    assert!(
        may_resume_after_danger(
            &observed,
            Duration::from_secs(30),
            "replacement",
            false,
            false,
        ),
        "Ordinary non-water timer behavior is unchanged"
    );
    assert!(!may_resume_after_danger(
        &observed,
        Duration::from_secs(3),
        "replacement",
        false,
        false,
    ));
}

#[tokio::test]
async fn recovered_breathing_releases_owned_input_once_and_yields_after_native_release() {
    // Transport/lifecycle fixture, not evidence of real swimming. Keep the
    // native control running after breath recovery, then acknowledge release.
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
    let shared = Arc::new(Shared {
        mutation: Arc::new(tokio::sync::Mutex::new(())),
        model_lanes: crate::model::Lanes::default(),
        feedback: Mutex::new(crate::feedback::Trace::default()),
        program_generation: AtomicU64::new(0),
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
        emergency: AtomicBool::new(false),
        running: AtomicBool::new(true),
        latest: Mutex::new(None),
        reason: Mutex::new(String::new()),
        failed: tokio::sync::Notify::new(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let cancellations = Arc::new(AtomicU64::new(0));
    let yielded = Arc::new(AtomicBool::new(false));
    let held_until_release = Arc::new(AtomicBool::new(false));
    let (server_shared, server_stop, server_cancel, server_yield, server_held) = (
        shared.clone(),
        stop.clone(),
        cancellations.clone(),
        yielded.clone(),
        held_until_release.clone(),
    );
    let upstream = tokio::spawn(async move {
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for z in -4..=4 {
                for y in 0..=5 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else if y<=2{"minecraft:water"}else{"minecraft:air"},
                "collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        let mut control = json!({});
        let mut tick = 0;
        let mut release_polls = 0;
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
            let response = if first.starts_with("GET /v1/bots/own/observations ") {
                tick += 5;
                let recovering = control["action_id"].is_string();
                if server_cancel.load(Ordering::Acquire) > 0 {
                    release_polls += 1;
                    if release_polls == 1 {
                        server_held.store(server_shared.urgent(), Ordering::Release);
                    } else {
                        control["outcome"] = json!("cancelled");
                        control["input_released"] = json!(true);
                    }
                    if release_polls > 2 && !server_shared.urgent() {
                        server_yield.store(true, Ordering::Release);
                        server_stop.store(true, Ordering::Release);
                    }
                    // A short observation budget distinguishes immediate
                    // confirmed-release handoff from the old3second hold.
                    if release_polls >= 8 {
                        server_stop.store(true, Ordering::Release);
                    }
                }
                json!({"revision":tick.to_string(),"player":{"uuid":"own","entity_id":1,
                    "dimension":"minecraft:overworld","position":[0.5,if recovering{1.5}else{1.},0.5],
                    "health":20.,"food":20,"sprinting":true,"on_ground":false,"in_water":true,
                    "eyes_in_water":!recovering,"air":if recovering{300}else{200},"max_air":300,"control":control},
                    "world":{"game_time":tick,"sample_clock":{"epoch":"fixture"},"blocks":blocks,"entities":[]},"menu":{}})
            } else if first.starts_with("GET /v1/bots/own/actions/pending ") {
                json!([])
            } else {
                assert!(first.starts_with("POST /v1/bots/own/actions "), "{first}");
                let request: ActionRequest = serde_json::from_slice(&bytes).unwrap();
                match request.action {
                    Action::StartMove { target, .. } => {
                        assert!(
                            control["action_id"].is_null(),
                            "only one breathing intention"
                        );
                        control = json!({"action_id":request.action_id,"outcome":"running",
                            "target":[target.x,target.y,target.z],"progress":{"elapsed_ms":100}});
                    }
                    Action::CancelControl { action_id } => {
                        assert_eq!(
                            control["action_id"], action_id,
                            "only the owned input may be cancelled"
                        );
                        server_cancel.fetch_add(1, Ordering::AcqRel);
                        control["cancel_requested"] = json!(true);
                    }
                    other => panic!("Unexpected control {other:?}"),
                }
                json!({"action_id":request.action_id,"outcome":"running","adapter_version":"fixture","evidence":{},"error":null})
            };
            let body = response.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("body.sqlite")).unwrap();
    let handoff_state = shared.clone();
    tokio::time::timeout(
        Duration::from_secs(5),
        controller(
            client,
            Config {
                enabled: true,
                period_ms: 50,
                decision_samples: 0,
                experimental_ranged_boundary_turns: false,
            },
            shared,
            stop,
            &j,
            3.5,
            false,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    upstream.abort();
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    assert!(held_until_release.load(Ordering::Acquire));
    assert!(
        yielded.load(Ordering::Acquire),
        "yield only after fresh native release, without3second relatch"
    );
    assert_eq!(
        handoff_state.view()["scheduling_revision"],
        2,
        "Only emergency entry and confirmed release advance scheduling; observations and final stop do not"
    );
}

#[test]
fn unproved_boundary_turns_require_explicit_deployment_opt_in() {
    assert!(!Config::default().experimental_ranged_boundary_turns);
    let previous: Config = serde_json::from_value(json!({"enabled":true,"period_ms":150})).unwrap();
    assert!(!previous.experimental_ranged_boundary_turns);
    let enabled: Config =
        serde_json::from_value(json!({"experimental_ranged_boundary_turns":true})).unwrap();
    assert!(enabled.experimental_ranged_boundary_turns);
}
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
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
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
                decision_samples: 0,
                experimental_ranged_boundary_turns: false,
            },
            shared,
            stop,
            &j,
            3.5,
            false,
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
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
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
    controller_projectile_scenario(false, false).await;
    controller_projectile_scenario(true, false).await;
}
#[tokio::test]
async fn native_rejection_under_urgent_fire_uses_one_prefix_without_repeating_geometric_dispatch() {
    controller_projectile_scenario(false, true).await;
}
async fn controller_projectile_scenario(replace_projectile: bool, require_native: bool) {
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
        let mut native_queried = false;
        let mut move_attempts = 0;
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
                    player:json!({"uuid":"own","entity_id":1,"dimension":"minecraft:overworld","position":[0.5,1.,0.5],"health":20.,"food":20,"sprinting":true,"on_ground":true,"attack_strength":1.,"control":control}),
                    world:json!({"game_time":tick,"sample_clock":{"epoch":"fixture"},"blocks":blocks,"entities":[{"entity_id":9,"visible":true,"hostile":true,"position":[2.5,1.,0.5]},{"entity_id":if replace_projectile && tick>=116{11}else{10},"uuid":if replace_projectile && tick>=116{"new-projectile-fixture"}else{"projectile-fixture"},"visible":true,"projectile":true,"position":[2.5,1.9,0.5],"velocity":[-0.25,0.,0.]}]}),
                    menu:json!({}),
                }).unwrap()
            } else if first.starts_with("POST /v1/bots/own/observations/query ") {
                assert!(require_native && !native_queried);
                let query: ObservationQuery = serde_json::from_slice(&body).unwrap();
                assert_eq!(query.capability, "minecraft:movement_options");
                assert_eq!(
                    query.input,
                    json!({"offset":0,"limit":8,"target":[-0.5,1.,-2.5]})
                );
                native_queried = true;
                json!({"format":"minecraft-movement-options-v1","scene":"urgent-native-prefix",
                    "origin":{"position":[0.5,1.,0.5],"dimension":"minecraft:overworld"},
                    "options":[{"target":[0.5,1.,2.5],"jump":false,"duration_ticks":8,
                        "max_ticks":60,"touched_blocks":["minecraft:stone","minecraft:air"]}],
                    "complete":false,"next_offset":8})
            } else {
                assert!(first.starts_with("POST /v1/bots/own/actions "), "{first}");
                let request: ActionRequest = serde_json::from_slice(&body).unwrap();
                let mut outcome = "observed";
                match &request.action {
                    Action::StartMove { target, .. } => {
                        move_attempts += 1;
                        if require_native && move_attempts == 1 {
                            outcome = "rejected";
                        } else {
                            if require_native {
                                assert!(
                                    native_queried,
                                    "do not dispatch another guess after native refusal"
                                );
                                assert_eq!([target.x, target.y, target.z], [0.5, 1., 2.5]);
                                finished = true;
                            }
                            control = json!({"action_id":request.action_id,"outcome":"running","target":[target.x,target.y,target.z]});
                            outcome = "running";
                        }
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
                json!({"action_id":request.action_id,"outcome":outcome,"adapter_version":"fixture","evidence":{"native_effect_verified":outcome=="observed"},"error":if outcome=="rejected"{json!({"code":"unsupported_capability","message":"No bounded held-input trajectory reaches waypoint"})}else{Value::Null}})
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
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
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
                decision_samples: 0,
                experimental_ranged_boundary_turns: false,
            },
            shared,
            stop,
            &journal,
            3.5,
            false,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let actions = upstream.await.unwrap();
    assert!(matches!(actions.first(), Some(Action::StartMove { .. })));
    if require_native {
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::StartMove { .. }))
                .count(),
            2
        );
    } else {
        assert!(matches!(actions.last(), Some(Action::CancelControl { .. })));
    }
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
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
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
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
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
        program_pending: Mutex::new(Default::default()),
        scheduling_revision: AtomicU64::new(0),
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

#[tokio::test]
async fn controller_cover_occlusion_lease_starts_after_original_input_is_released() {
    // Real controller/HTTP sequence, synthetic geometry: no native survival claim.
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
    let mut blocks = Vec::new();
    for x in -4..=6 {
        for z in -4..=4 {
            for y in 0..=4 {
                let solid = y == 0 || (x == 3 && z == 1 && y <= 3);
                blocks.push(json!({"position":[x,y,z],"block":if solid{"minecraft:stone"}else{"minecraft:air"},
            "collision":if solid{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
            }
        }
    }
    let initial = Observation {
        revision: "100".into(),
        menu: json!({}),
        player: json!({"uuid":"own","life_id":"life","entity_id":1,"dimension":"fixture:world","position":[0.5,1.,0.5],"health":20.,"food":20,"sprinting":true,"on_ground":true,"in_water":false,"in_lava":false}),
        world: json!({"game_time":100,"sample_clock":{"epoch":"epoch","nanos":"100"},"blocks":blocks,
            "entities":[{"uuid":"shooter","entity_id":9,"type":"minecraft:skeleton","visible":true,"hostile":true,"ranged_attack":true,"health":20.,"position":[6.5,1.,0.5]}]}),
    };
    let selected = geometry::evade_policy(&initial, &[], &[], &[], 1.1, &[], false)
        .expect("synthetic visible cover route");
    assert!(
        selected.cover_path.len() > 1,
        "fixture must select a multi-leg cover path: {selected:?}"
    );
    let endpoint = [selected.target.x, selected.target.y, selected.target.z];
    let final_point = selected.cover_path.last().unwrap();
    let final_point = [final_point.x, final_point.y, final_point.z];
    assert_ne!(endpoint, final_point, "fixture needs remaining work");
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let upstream = tokio::spawn(async move {
        let mut phase = 0usize;
        let mut original = None::<String>;
        let mut reads = 0;
        loop {
            let (mut socket, _) = http.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
                assert!(header.len() < 8192);
            }
            let header = String::from_utf8(header).unwrap();
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
            let mut finished = false;
            let response = if header.starts_with("GET /v1/bots/own/observations ") {
                reads += 1;
                assert!(reads < 30, "controller did not finish bounded sequence");
                let mut o = initial.clone();
                o.revision = (100 + reads).to_string();
                o.world["game_time"] = json!(100 + reads);
                o.world["sample_clock"]["nanos"] = json!((100 + reads).to_string());
                if let Some(id) = &original {
                    phase += 1;
                    let fraction = match phase {
                        1 => 0.25,
                        2 => 0.5,
                        3 => 0.75,
                        _ => 1.,
                    };
                    o.player["position"] =
                        json!(std::array::from_fn::<_, 3, _>(
                            |i| [0.5, 1., 0.5][i] + fraction * (endpoint[i] - [0.5, 1., 0.5][i])
                        ));
                    o.player["control"] = json!({"action_id":id,"target":endpoint,"outcome":if phase<4{"running"}else{"observed"},"input_released":if phase<4{Value::Null}else{json!(phase>=5)},"cancel_requested":false});
                    if phase == 2 || phase >= 4 {
                        o.world["entities"] = json!([]);
                    }
                }
                serde_json::to_value(o).unwrap()
            } else {
                assert!(header.starts_with("POST /v1/bots/own/actions "), "{header}");
                let request: ActionRequest = serde_json::from_slice(&bytes).unwrap();
                let target = match &request.action {
                    Action::StartMove { target, .. } | Action::StartMoveThrough { target, .. } => {
                        [target.x, target.y, target.z]
                    }
                    other => panic!("unexpected {other:?} phase{phase}"),
                };
                if let Some(id) = &original {
                    assert!(phase >= 5, "successor before Observed+released");
                    assert_ne!(&request.action_id, id);
                    assert!(
                        matches!(request.action, Action::StartMove { max_ticks: 60, .. }),
                        "continuation must stop at its finite endpoint"
                    );
                    assert_eq!(
                        target, final_point,
                        "Running visibility changes must not poison the terminal occlusion lease"
                    );
                    finished = true;
                } else {
                    assert_eq!(target, endpoint);
                    original = Some(request.action_id.clone());
                }
                json!({"action_id":request.action_id,"outcome":"running","adapter_version":"fixture","evidence":{}})
            };
            let body = response.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            if finished {
                stopped.store(true, Ordering::Release);
                break;
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(&dir.path().join("body.sqlite")).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        controller(
            client,
            Config {
                enabled: true,
                period_ms: 50,
                decision_samples: 8,
                experimental_ranged_boundary_turns: false,
            },
            admission_shared(),
            stop.clone(),
            &journal,
            1.1,
            false,
        ),
    )
    .await;
    stop.store(true, Ordering::Release);
    upstream.await.unwrap();
    result.unwrap().unwrap();
    let events = journal.history("original_action_id").unwrap();
    let events = events.as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "body_cover_handoff_consumed")
            .count(),
        1
    );
    let phases: Vec<_> = events
        .iter()
        .filter_map(|e| e["data"]["phase"].as_str())
        .collect();
    assert_eq!(
        phases,
        [
            "released_occluded_candidate",
            "terminal_unreleased",
            "running"
        ]
    );
}
