use super::*;
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
        mutation: Arc::new(tokio::sync::Mutex::new(())),
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
    let queued = tokio::spawn(async move { proxy(server, c, s).await });
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
    tokio::time::timeout(Duration::from_secs(1), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), http.accept())
            .await
            .is_err(),
        "abandoned intention must never reach the gateway"
    );
    assert!(shared.mutation.try_lock().is_ok());
}
