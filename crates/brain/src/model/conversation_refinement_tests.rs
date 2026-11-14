//! Actual conversation HTTP contract and task handoff; no provider or game calls.
use super::{Conversation, GoalAction, Model, tests::read_request};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

struct AbortServer(tokio::task::AbortHandle);
impl Drop for AbortServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn assert_request(wire: &Value, context: &Value) {
    assert_eq!(wire["model"], "gpt-6-luna");
    assert_eq!(wire["purpose"], "plan");
    assert_eq!(wire["options"], json!({"num_predict":1024}));
    assert_eq!(wire["stream"], false);
    assert!(wire.get("tools").is_none());
    assert_eq!(wire["format"]["additionalProperties"], false);
    assert_eq!(
        wire["format"]["properties"]["goal_action"]["enum"],
        json!(["continue", "refine", "replace"])
    );
    assert_eq!(
        wire["format"]["required"],
        json!(["message", "goal_action", "goal", "reason"])
    );
    let prompt = wire["messages"][0]["content"].as_str().unwrap();
    assert_eq!(wire["messages"][0]["role"], "system");
    for rule in [
        "goal_action=\"refine\"",
        "SAME task",
        "original terminal",
        "Incidental",
        "not a new player request",
    ] {
        assert!(
            prompt.contains(rule),
            "missing delivered conversation rule: {rule}"
        );
    }
    assert_eq!(wire["messages"][1]["role"], "user");
    let delivered: Value =
        serde_json::from_str(wire["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        &delivered, context,
        "current messages and goal must reach the real request intact"
    );
}

async fn respond(socket: &mut tokio::net::TcpStream, content: Value) {
    let body = json!({"done":true,"message":{"content":content.to_string()}}).to_string();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

#[tokio::test]
async fn conversation_refinement_schema_keeps_task_completion_and_player_replacement_retires_it() {
    // Relocated from the implementation author's first full-path regression.
    // A synthetic system advancement is information, not a player request.
    let (_root, mut brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
    brain
        .cfg
        .completion_inventory
        .insert("fixture:table".into(), 1);
    brain
        .cfg
        .completion_inventory
        .insert("fixture:pick".into(), 1);
    brain
        .journal
        .select_goal(&brain.goal, "fixture", json!({}), None)
        .unwrap();
    crate::completion::bind(&brain.journal, &brain.cfg, &brain.goal, &brain.observed).unwrap();
    let original_task = brain.journal.get("task_revision").unwrap();
    brain.observed.player["inventory"] = json!([{"item":"fixture:table","count":1}]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let (expected_sender, mut expected_receiver) = tokio::sync::mpsc::channel::<Value>(2);
    let mut server = tokio::spawn(async move {
        for (index, action, goal) in [
            (
                0,
                "refine",
                "Secure safety, then resume the same material and tool task",
            ),
            (1, "replace", "Visit the requesting player"),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let wire = read_request(&mut socket).await;
            let context = expected_receiver.recv().await.unwrap();
            assert_request(&wire, &context);
            assert_eq!(
                context["messages"][0]["kind"],
                if index == 0 { "system" } else { "player" }
            );
            respond(&mut socket, json!({"message":"","goal_action":action,"goal":goal,"reason":"fixture structured task relationship"})).await;
        }
    });
    let _guard = AbortServer(server.abort_handle());
    let model = Model {
        http: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        endpoint,
    };
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        for index in 0..2 {
            let messages = if index == 0 {
                json!([{"kind":"system","message":"A nearby player earned an advancement"}])
            } else {
                json!([{"kind":"player","sender_uuid":"00000000000000000000000000000001","message":"Please visit me instead of making the kit"}])
            };
            let revision = brain.journal.get("goal_revision").unwrap().unwrap();
            let context = json!({"goal":brain.goal,"goal_revision":revision,"messages":messages});
            expected_sender.send(context.clone()).await.unwrap();
            let (reply, usage) = model.converse(&context).await.unwrap();
            assert_eq!(usage["host_tools_executed"], false);
            assert_eq!(usage["transport_attempts"], 1);
            assert_eq!(reply.goal_action, if index == 0 {GoalAction::Refine} else {GoalAction::Replace});
            assert!(brain.journal.conversation_goal(&reply, &revision, &messages).unwrap());
            brain.goal = reply.goal;
            let view = crate::completion::view(&brain.journal, &brain.cfg, &brain.goal).unwrap();
            assert_eq!(view["applies_to_current_goal"], index == 0);
            let (kind, data): (String, String) = brain.journal.db.query_row(
                "SELECT kind,data FROM events WHERE kind IN ('conversation_goal_refined','conversation_goal_selected','player_goal_selected') ORDER BY id DESC LIMIT 1",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
            let data: Value = serde_json::from_str(&data).unwrap();
            assert_eq!(data["player_attribution_verified"], index == 1);
            if index == 0 {
                assert_eq!(kind, "conversation_goal_refined");
                assert_eq!(brain.journal.get("task_revision").unwrap(), original_task);
                assert!(brain.journal.get("player_request").unwrap().is_none());
                assert!(crate::completion::verify(&brain.journal, &brain.cfg, &brain.goal, &brain.observed).is_err(), "missing result still blocks terminal completion");
                assert!(!brain.journal.goal_completed(&brain.goal).unwrap());
                let restarted = crate::Journal::open(&brain.cfg.state.join("brain.sqlite")).unwrap();
                crate::completion::bind(&restarted, &brain.cfg, &brain.goal, &brain.observed).unwrap();
                assert_eq!(crate::completion::view(&restarted, &brain.cfg, &brain.goal).unwrap()["applies_to_current_goal"], true);
            } else {
                assert_eq!(kind, "player_goal_selected");
                assert_ne!(brain.journal.get("task_revision").unwrap(), original_task);
                assert_eq!(brain.journal.get("player_request").unwrap(), Some(messages.to_string()));
                assert!(crate::completion::verify(&brain.journal, &brain.cfg, &brain.goal, &brain.observed).is_ok());
            }
        }
    }).await;
    let joined = tokio::time::timeout(Duration::from_secs(1), &mut server).await;
    joined.expect("bounded conversation fixture").unwrap();
    result.unwrap();
    brain
        .journal
        .select_goal(&brain.cfg.goal, "agent_goal_selected", json!({}), None)
        .unwrap();
    crate::completion::bind(&brain.journal, &brain.cfg, &brain.cfg.goal, &brain.observed).unwrap();
    assert_eq!(
        crate::completion::view(&brain.journal, &brain.cfg, &brain.cfg.goal).unwrap()["applies_to_current_goal"],
        false,
        "returning to original words does not restore the retired task contract"
    );
}

#[tokio::test]
async fn conversation_wire_accepts_continue_and_rejects_missing_or_unknown_goal_actions() {
    let context = json!({"goal":"Continue current task","goal_revision":"17","messages":[{"kind":"disguised","message":"A progress update"}]});
    let expected = context.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let mut server = tokio::spawn(async move {
        for response in [
            json!({"message":"Still working","goal_action":"continue","goal":"","reason":"progress question"}),
            json!({"message":"","goal_action":"execute_host_command","goal":"x","reason":"invalid"}),
            json!({"message":"","goal":"x","reason":"missing required relation"}),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let wire = read_request(&mut socket).await;
            assert_request(&wire, &expected);
            respond(&mut socket, response).await;
        }
    });
    let _guard = AbortServer(server.abort_handle());
    let model = Model {
        http: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        endpoint,
    };
    let (reply, usage) = model.converse(&context).await.unwrap();
    assert_eq!(reply.goal_action, GoalAction::Continue);
    assert!(reply.goal.is_empty());
    assert_eq!(usage["host_tools_executed"], false);
    for _ in 0..2 {
        assert!(model.converse(&context).await.is_err());
    }
    tokio::time::timeout(Duration::from_secs(1), &mut server)
        .await
        .expect("bounded schema fixture")
        .unwrap();
    assert!(serde_json::from_value::<Conversation>(json!({"message":"","goal_action":"refine","goal":"x","reason":"x","execute":"forbidden"})).is_err());
}
