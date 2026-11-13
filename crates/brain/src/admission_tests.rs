//! Exercise the real Brain → SDK boundary, not a duplicate validator.
use super::*;
use minecraft_sdk::{Action, BlockPos, Credential, Hand, Receipt, ServerInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) fn fixture(endpoint: &str) -> (tempfile::TempDir, Brain) {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().to_path_buf();
    let cfg:Config=serde_json::from_value(json!({"profile":state.join("profile"),"model_key":state.join("model"),"model_endpoint":"https://example.invalid/","workspace":state,"state":state,"goal":"fixture"})).unwrap();
    let profile = Profile {
        endpoint: endpoint.into(),
        instance_id: "instance".into(),
        credential: Credential {
            bot_id: "own".into(),
            player_uuid: "actor".into(),
            key: "fixture".into(),
        },
        session: None,
    };
    let brain = Brain {
        server: ServerInfo {
            api_version: 1,
            instance_id: "instance".into(),
            minecraft_version: "fixture".into(),
            platform: "fixture".into(),
            adapter_version: "fixture".into(),
            ready: true,
            capabilities: vec!["minecraft:movement".into(), "minecraft:inventory".into()],
            limits: json!({}),
        },
        body: None,
        cfg,
        client: Client::with_profile(&profile)
            .unwrap()
            .without_local_arbiter(),
        journal: Journal::open(&state.join("brain.sqlite")).unwrap(),
        run_id: "a".repeat(24),
        observed: Observation {
            revision: "before".into(),
            player: json!({"uuid":"actor","inventory":[]}),
            world: json!({}),
            menu: json!({}),
        },
        goal: "fixture".into(),
        _lock: fs::File::create(state.join("controller.lock")).unwrap(),
        _actor_lock: ownership::acquire(&profile, &state.join("actors")).unwrap(),
    };
    (root, brain)
}
fn invalid_place() -> Action {
    Action::Place {
        block: BlockPos {
            x: 503,
            y: 65,
            z: 20,
        },
        face: 1,
        hand: Hand::Main,
        hit: [503.5, 66., 20.5],
    }
}
async fn request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.unwrap());
        assert!(header.len() < 8192);
    }
    let header = String::from_utf8(header).unwrap();
    let len = header
        .lines()
        .find_map(|s| {
            s.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(len < 8192);
    let mut bytes = vec![0; len];
    socket.read_exact(&mut bytes).await.unwrap();
    (
        header.lines().next().unwrap().to_owned(),
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}
async fn reply(socket: &mut tokio::net::TcpStream, status: &str, value: Value) {
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
#[tokio::test]
async fn unadvertised_direct_action_never_reaches_http_or_changes_pending_intentions() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_root, mut brain) = fixture(&format!("http://{}/", listener.local_addr().unwrap()));
    let action = || Action::Extension {
        capability: "fixture:optional".into(),
        input: json!({"operation":"fixture"}),
    };
    for previous in ["", "existing-durable-intention"] {
        brain.journal.set("pending_action", previous).unwrap();
        let error = brain.act(action()).await.unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<minecraft_sdk::ApiError>()
                .unwrap()
                .code,
            ErrorCode::UnsupportedCapability
        );
        assert_eq!(
            brain.journal.get("pending_action").unwrap().as_deref(),
            Some(previous)
        );
        let intents: i64 = brain
            .journal
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind='action_intent'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            intents, 0,
            "unsupported requests never become durable intentions"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "no unsupported request reached HTTP admission"
    );
    // Fresh positive-control bookkeeping: advertising a capability permits the
    // same typed action to reach the authoritative SDK boundary normally.
    brain.journal.set("pending_action", "").unwrap();
    brain.server.capabilities.push("fixture:optional".into());
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (line, input) = request(&mut socket).await;
        assert!(line.starts_with("POST ") && line.contains("/actions"));
        let admitted: ActionRequest = serde_json::from_value(input).unwrap();
        assert!(
            matches!(admitted.action, Action::Extension { capability, .. } if capability == "fixture:optional")
        );
        reply(&mut socket, "200 OK", json!({"action_id":admitted.action_id,"outcome":"observed","adapter_version":"fixture","evidence":{},"error":null})).await;
        let (mut socket, _) = listener.accept().await.unwrap();
        let (line, _) = request(&mut socket).await;
        assert!(line.starts_with("GET ") && line.contains("/observation"));
        reply(&mut socket,"200 OK",json!({"revision":"after","player":{"uuid":"actor","inventory":[]},"world":{},"menu":{}})).await;
    });
    assert_eq!(
        brain.act(action()).await.unwrap()["receipt"]["outcome"],
        "observed"
    );
    upstream.await.unwrap();
}
#[tokio::test]
async fn invalid_local_action_does_not_poison_the_next_action_or_hide_uncertainty() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_root, mut brain) = fixture(&format!("http://{}/", listener.local_addr().unwrap()));
    assert!(brain.act(invalid_place()).await.is_err());
    assert!(
        brain
            .journal
            .get("pending_action")
            .unwrap()
            .unwrap_or_default()
            .is_empty(),
        "Locally rejected input must not create a pending admission"
    );
    let intents: i64 = brain
        .journal
        .db
        .query_row(
            "SELECT count(*) FROM events WHERE kind='action_intent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(intents, 0);
    let upstream = tokio::spawn(async move {
        let mut actions = Vec::new();
        for index in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (line, input) = request(&mut socket).await;
            if index == 1 {
                assert!(line.starts_with("GET ") && line.contains("/observation"));
                reply(&mut socket,"200 OK",json!({"revision":"after","player":{"uuid":"actor","inventory":[]},"world":{},"menu":{}})).await;
            } else {
                assert!(line.starts_with("POST ") && line.contains("/actions"));
                let action: ActionRequest = serde_json::from_value(input).unwrap();
                actions.push(action.action.clone());
                if index == 0 {
                    reply(
                        &mut socket,
                        "200 OK",
                        serde_json::to_value(Receipt {
                            action_id: action.action_id,
                            outcome: Outcome::Observed,
                            adapter_version: "fixture".into(),
                            evidence: json!({"native_effect_verified":true}),
                            error: None,
                        })
                        .unwrap(),
                    )
                    .await;
                }
                // Second mutation loses its response after actual HTTP admission.
            }
        }
        actions
    });
    assert_eq!(
        brain
            .act(Action::Look { yaw: 1., pitch: 2. })
            .await
            .unwrap()["receipt"]["outcome"],
        "observed"
    );
    assert!(
        brain
            .journal
            .get("pending_action")
            .unwrap()
            .unwrap_or_default()
            .is_empty()
    );
    assert!(
        brain
            .act(Action::Look { yaw: 3., pitch: 4. })
            .await
            .is_err()
    );
    assert!(
        !brain
            .journal
            .get("pending_action")
            .unwrap()
            .unwrap_or_default()
            .is_empty(),
        "Genuine interrupted dispatch retains its durable ID"
    );
    let actions = upstream.await.unwrap();
    assert_eq!(actions.len(), 2, "No mutation was replayed");
}

#[tokio::test]
async fn old_invalid_intention_requires_scoped_not_found_and_preserves_valid_uncertainty() {
    let (_root, brain) = fixture("http://127.0.0.1:1/");
    let journal = &brain.journal;
    journal
        .event(
            "brain_started",
            json!({"run":brain.run_id,"instance":"instance","uuid":"actor"}),
        )
        .unwrap();
    let sequence = journal
        .event("action_intent", json!({"action":invalid_place()}))
        .unwrap();
    let id = format!("brain-{}-{sequence}", brain.run_id);
    journal.note_intention(&id, false).unwrap();
    let missing = minecraft_sdk::ApiError::new(ErrorCode::NotFound, "fixture missing");
    for code in [
        ErrorCode::UncertainOutcome,
        ErrorCode::Unauthorized,
        ErrorCode::NotReady,
    ] {
        assert!(
            admission::reconcile_invalid(
                journal,
                &id,
                "instance",
                "actor",
                &minecraft_sdk::ApiError::new(code, "fixture")
            )
            .unwrap()
            .is_none()
        );
    }
    for (instance, actor) in [("other", "actor"), ("instance", "other"), ("instance", "")] {
        assert!(
            admission::reconcile_invalid(journal, &id, instance, actor, &missing)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        journal.get("pending_action").unwrap().as_deref(),
        Some(id.as_str())
    );
    let proof = admission::reconcile_invalid(journal, &id, "instance", "actor", &missing)
        .unwrap()
        .unwrap();
    assert_eq!(proof["replayed"], false);
    assert_eq!(proof["native_effect_verified"], false);
    assert_eq!(proof["historical_effect_verified"], false);
    assert_eq!(journal.get("pending_action").unwrap().as_deref(), Some(""));
    assert_eq!(
        journal
            .db
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM events WHERE id=?1 AND kind='action_intent'",
                [sequence],
                |r| r.get(0)
            )
            .unwrap(),
        1,
        "Reconciliation preserves the failed original intent"
    );
    let sequence = journal
        .event(
            "action_intent",
            json!({"action":Action::Look{yaw:1.,pitch:2.}}),
        )
        .unwrap();
    let valid_id = format!("brain-{}-{sequence}", brain.run_id);
    journal.note_intention(&valid_id, false).unwrap();
    assert!(
        admission::reconcile_invalid(journal, &valid_id, "instance", "actor", &missing)
            .unwrap()
            .is_none(),
        "Valid but unconfirmed mutations require ordinary uncertainty handling"
    );
    assert_eq!(
        journal.get("pending_action").unwrap().as_deref(),
        Some(valid_id.as_str())
    );
}

#[tokio::test]
async fn status_lookup_resumes_only_a_locally_invalid_old_intention_without_replay() {
    for invalid in [true, false] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_root, mut brain) = fixture(&format!("http://{}/", listener.local_addr().unwrap()));
        brain
            .journal
            .event(
                "brain_started",
                json!({"run":brain.run_id,"instance":"instance","uuid":"actor"}),
            )
            .unwrap();
        let sequence = brain
            .journal
            .event(
                "action_intent",
                json!({"action":if invalid{invalid_place()}else{Action::SelectSlot{slot:1}}}),
            )
            .unwrap();
        let old_id = format!("brain-{}-{sequence}", brain.run_id);
        brain.journal.note_intention(&old_id, false).unwrap();
        let expected_id = old_id.clone();
        let upstream = tokio::spawn(async move {
            let mut mutations = Vec::new();
            for index in 0..if invalid { 3 } else { 1 } {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (line, input) = request(&mut socket).await;
                match index {
                    0 => {
                        assert!(line.starts_with("GET ") && line.contains(&expected_id));
                        reply(
                            &mut socket,
                            "404 Not Found",
                            serde_json::to_value(minecraft_sdk::ApiError::new(
                                ErrorCode::NotFound,
                                "fixture missing",
                            ))
                            .unwrap(),
                        )
                        .await;
                    }
                    1 => {
                        assert!(line.starts_with("POST "));
                        let action: ActionRequest = serde_json::from_value(input).unwrap();
                        mutations.push(action.action);
                        reply(&mut socket,"200 OK",json!({"action_id":action.action_id,"outcome":"observed","adapter_version":"fixture","evidence":{},"error":null})).await;
                    }
                    _ => {
                        assert!(line.starts_with("GET "));
                        reply(&mut socket,"200 OK",json!({"revision":"after","player":{"uuid":"actor","inventory":[]},"world":{},"menu":{}})).await;
                    }
                }
            }
            mutations
        });
        let result = brain.act(Action::SelectSlot { slot: 2 }).await;
        let mutations = upstream.await.unwrap();
        if invalid {
            assert!(result.is_ok());
            assert_eq!(mutations.len(), 1);
            assert!(matches!(mutations[0], Action::SelectSlot { slot: 2 }));
            assert_eq!(
                brain.journal.get("pending_action").unwrap().as_deref(),
                Some("")
            );
            assert_eq!(
                brain
                    .journal
                    .db
                    .query_row::<i64, _, _>(
                        "SELECT count(*) FROM events WHERE kind='local_intention_reconciled'",
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                1
            );
        } else {
            assert!(result.is_err());
            assert!(mutations.is_empty());
            assert_eq!(
                brain.journal.get("pending_action").unwrap().as_deref(),
                Some(old_id.as_str())
            );
        }
    }
}
