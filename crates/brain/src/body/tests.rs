use super::*;
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
}
