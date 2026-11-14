use super::*;
use std::sync::{Arc, Mutex};

// Public shape/size translation of F15 turn5. No original world, identity,
// player text, paths or prompt/context contents are published. Its raw32347
// and JSON-quoted34940 sizes reproduce the actual wire counterexample.
fn f15_counterexample() -> Value {
    let mut context = json!({"goal":"Retain the verified fixture result","goal_revision":"2",
        "completion_requirements":{"evaluation_phase":"final_success_claim_only","inventory_minimum":{"fixture:result":1}},
        "pending_action":"fixture-original-action","body":{"emergency":true,"scheduling_revision":7},
        "observation":{"player":{"uuid":"fixture-actor","life_id":"fixture-life","inventory":[]},
            "menu":{"menu_id":3,"state_id":9,"type":"fixture:menu"}},
        "last_tool_result":{"tool":"navigate","result":{"outcome":"uncertain","action_id":"fixture-original-action"}},
        "last_experiment":{"tool":"navigate"},"navigation_resume":{"paused":true},
        "recent":[{"text":""}],"fixture_retained_context":""});
    let raw = context.to_string();
    let extra = serde_json::to_vec(&raw).unwrap().len() - raw.len();
    let needed = 34940 - 32347 - extra;
    let quotes = needed / 2;
    let newline = needed % 2;
    let base = context["recent"].to_string().len();
    let text = format!(
        "{}{}{}",
        "\"".repeat(quotes),
        "\n".repeat(newline),
        "r".repeat(6771 - base - quotes * 2 - newline * 2)
    );
    context["recent"][0]["text"] = json!(text);
    let padding = 32347 - context.to_string().len();
    context["fixture_retained_context"] = json!("f".repeat(padding));
    let raw = context.to_string();
    assert_eq!(raw.len(), 32347);
    assert_eq!(serde_json::to_vec(&raw).unwrap().len(), 34940);
    context
}
fn selected_context() -> Value {
    // Explicit opt-in private replay, never skipped in CI: absent env uses the
    // equivalent public counterexample. Never print private context on failure.
    match std::env::var_os("MC_SDK_WIRE_FIXTURE") {
        Some(path) => {
            serde_json::from_slice(&crate::private_read(Path::new(&path), 131072).unwrap()).unwrap()
        }
        None => f15_counterexample(),
    }
}

#[tokio::test]
async fn f15_actual_serialized_contract_fits_before_one_owned_broker_admission() {
    use axum::{
        Json, Router,
        body::Bytes,
        extract::{DefaultBodyLimit, State},
        routing::post,
    };
    let context = selected_context();
    let mut format = session_schema(false);
    navigation_schema(&context, &mut format);
    let contract = wire::Contract {
        model: "gpt-6-luna",
        purpose: "plan",
        format: &format,
        prompt: include_str!("../system.md"),
        tokens: 4096,
    };
    let original_bytes = contract.encode(&context).unwrap().len();
    assert_eq!(
        original_bytes, 67968,
        "F15 source/serialized fixture contract drifted"
    );
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    async fn accept(State(seen): State<Arc<Mutex<Vec<Value>>>>, body: Bytes) -> Json<Value> {
        assert!(body.len() <= 65536);
        let request: Value = serde_json::from_slice(&body).unwrap();
        for message in request["messages"].as_array().unwrap() {
            assert!(message["content"].as_str().unwrap().len() <= 32768);
        }
        seen.lock().unwrap().push(request);
        let choice = json!({"tool":"report","input":"Owned fixture only","path":"","summary":"Fixture","notes":"","experiment":false});
        Json(json!({"done":true,"message":{"content":choice.to_string()}}))
    }
    let router = Router::new()
        .route("/chat", post(accept))
        .layer(DefaultBodyLimit::max(65536))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/chat", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let model = Model {
        http: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(),
        endpoint,
    };
    let result = model.choose(&context, false).await;
    server.abort();
    assert!(
        result.is_ok(),
        "F15 actual wire contract must be compacted before its first POST"
    );
    let (choice, usage) = result.unwrap();
    assert_eq!(choice.tool, "report");
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        1,
        "No oversized rejected first POST or added allowance"
    );
    let delivered: Value =
        serde_json::from_str(seen[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
    for key in [
        "goal",
        "goal_revision",
        "completion_requirements",
        "pending_action",
        "body",
        "observation",
        "last_tool_result",
        "last_experiment",
        "navigation_resume",
    ] {
        assert!(
            delivered[key] == context[key],
            "Protected F15 field changed: {key}"
        );
    }
    assert_eq!(usage["transport_attempts"], 1);
    assert_eq!(usage["host_tools_executed"], false);
    assert_eq!(usage["request_wire"]["original_wire_bytes"], 67968);
    assert_eq!(usage["request_wire"]["protected_context_unchanged"], true);
    assert_eq!(
        usage["context_sha256"],
        crate::digest(delivered.to_string().as_bytes())
    );
}

#[test]
fn wire_budget_counts_escaping_and_utf8_without_changing_current_contracts() {
    let mut context = f15_counterexample();
    context["recent"] = json!([{"text":"\"\\\n\t é🦀".repeat(2000)}]);
    let format = schema();
    let contract = wire::Contract {
        model: "gpt-6-luna",
        purpose: "plan",
        format: &format,
        prompt: include_str!("../system.md"),
        tokens: 4096,
    };
    assert!(contract.encode(&context).unwrap().len() > wire::MAX_BYTES);
    let before = context.clone();
    let prepared = contract.prepare(&context, wire::MAX_BYTES).unwrap();
    assert!(prepared.bytes.len() <= wire::MAX_BYTES);
    let request: Value = serde_json::from_slice(&prepared.bytes).unwrap();
    assert!(request["messages"][1]["content"].as_str().unwrap().len() <= 32768);
    assert!(context == before, "Caller/journal frame must be untouched");
    assert!(prepared.context["observation"] == before["observation"]);
    assert!(prepared.context["pending_action"] == before["pending_action"]);
    assert_eq!(prepared.context["recent"]["omitted_for_wire_budget"], true);
    assert!(!prepared.metadata.to_string().contains("🦀"));
}

#[test]
fn optional_plan_and_source_are_whole_or_honestly_omitted_never_partial() {
    let format = schema();
    let contract = wire::Contract {
        model: "gpt-6-luna",
        purpose: "plan",
        format: &format,
        prompt: include_str!("../system.md"),
        tokens: 4096,
    };
    for key in ["active_plan", "active_program_source"] {
        let mut context = json!({"goal":"fixture","goal_revision":"7","mandatory_fixture":""});
        let padding = wire::MAX_BYTES - 512 - contract.encode(&context).unwrap().len();
        context["mandatory_fixture"] = json!("m".repeat(padding));
        let mandatory = context.clone();
        let plan = json!({"next_step":"PRIVATE_PLAN_SENTINEL".repeat(30),"dependencies":[],"resources":[]});
        let content = serde_json::to_vec(&plan).unwrap();
        context[key] = if key == "active_plan" {
            json!({"active":true,"goal_revision":"7","plan":plan,"content_sha256":crate::digest(&content),"content_bytes":content.len()})
        } else {
            json!({"format":"active-program-source-v1","content_complete":true,"content":"PRIVATE_SOURCE_SENTINEL".repeat(100)})
        };
        let prepared = contract.prepare(&context, wire::MAX_BYTES).unwrap();
        assert_eq!(prepared.context[key]["omitted_for_wire_budget"], true);
        assert!(prepared.context[key]["plan"].is_null());
        assert!(prepared.context[key]["content"].is_null());
        let attestation = context_attestation::summarize(&prepared.context, &"a".repeat(64));
        assert_ne!(attestation["active_plan"]["complete_content"], true);
        assert_ne!(attestation["active_source"]["content_complete"], true);
        assert!(!prepared.metadata.to_string().contains("PRIVATE_"));
        let mut retained = prepared.context.clone();
        retained.as_object_mut().unwrap().remove(key);
        assert!(
            retained == mandatory,
            "Required frame changed for optional view"
        );
        // An omitted wire projection never changes the saved/caller plan.
        let content_key = if key == "active_plan" {
            "plan"
        } else {
            "content"
        };
        assert!(!context[key][content_key].is_null());
    }
}

#[tokio::test]
async fn mandatory_wire_overflow_stops_before_any_http_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = Model {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        endpoint: format!("http://{}/", listener.local_addr().unwrap()),
    };
    let context = json!({"goal":"fixture","observation":{"player":{"life_id":"fixture"},"mandatory":"\"".repeat(32700)}});
    let error = model.choose(&context, false).await.unwrap_err();
    assert!(error.downcast_ref::<wire::Unfit>().is_some());
    assert_eq!(
        failure_metadata(error.as_ref())["protected_context_unchanged"],
        true
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn typed_413_allows_one_changed_same_role_repair_then_stops_or_accepts() {
    use tokio::io::AsyncWriteExt;
    for (optional, accept_second, error_body, initial_fit) in [
        (true, true, 0, false),
        (true, true, 1, false),
        (true, true, 2, false),
        (true, false, 0, false),
        (false, false, 0, false),
        (true, true, 0, true),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let expected = if optional { 2 } else { 1 };
        let upstream = tokio::spawn(async move {
            let mut requests = Vec::new();
            for attempt in 0..expected {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(super::tests::read_request(&mut socket).await);
                if attempt == 0 && error_body == 1 {
                    // Known HTTP413 must not become an unchanged transport retry
                    // just because its error page was truncated after headers.
                    socket.write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 999\r\nConnection: close\r\n\r\n{").await.unwrap();
                    continue;
                }
                let (status, body) = if accept_second && attempt == 1 {
                    let choice = json!({"tool":"report","input":"Fixture only","path":"","summary":"Fixture","notes":"","experiment":false});
                    (
                        "200 OK",
                        json!({"done":true,"message":{"content":choice.to_string()}}).to_string(),
                    )
                } else if attempt == 0 && error_body == 2 {
                    ("413 Payload Too Large", "x".repeat(32769))
                } else {
                    (
                        "413 Payload Too Large",
                        "PRIVATE_PROXY_RESPONSE_SENTINEL".into(),
                    )
                };
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            requests
        });
        let model = Model {
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            endpoint,
        };
        let mut context = json!({"goal":"fixture","goal_revision":"2","pending_action":"fixture-original-id",
            "completion_requirements":{"evaluation_phase":"final_success_claim_only"},
            "body":{"emergency":true},"observation":{"player":{"life_id":"fixture-life","health":7}}});
        if initial_fit {
            context = f15_counterexample();
            context["memory"] = json!({"historical":"m".repeat(2048)});
        } else if optional {
            context["recent"] = json!([{"text":"retrievable old history".repeat(100)}]);
        }
        let answer = model.choose(&context, false).await;
        if accept_second {
            let (_, usage) = answer.unwrap();
            assert_eq!(usage["transport_attempts"], 2);
            assert_eq!(usage["payload_repair"]["repair_attempted"], true);
            assert_eq!(usage["payload_repair"]["original_failure"]["status"], 413);
        } else {
            let error = answer.unwrap_err();
            assert!(error.to_string().starts_with("model_http_413:"));
            assert!(retry_at(error.as_ref()).is_none());
            assert!(daily_reset(error.as_ref()).is_none());
            assert!(provider_investigation::ProviderFailure::from_error(error.as_ref()).is_none());
            let metadata = failure_metadata(error.as_ref());
            assert_eq!(metadata["repair_attempted"], optional);
            assert!(!metadata.to_string().contains("PRIVATE_"));
            if optional {
                assert_eq!(metadata["changed_repair_exhausted"], true);
            }
        }
        let requests = upstream.await.unwrap();
        assert_eq!(requests.len(), expected);
        if optional {
            let first = serde_json::to_vec(&requests[0]).unwrap();
            let second = serde_json::to_vec(&requests[1]).unwrap();
            assert!(
                second.len() < first.len() && second != first,
                "Unchanged request must never be replayed for413"
            );
            for key in ["model", "purpose", "format", "options"] {
                assert!(requests[0][key] == requests[1][key]);
            }
            assert!(requests[0]["messages"][0] == requests[1]["messages"][0]);
            let mut a: Value =
                serde_json::from_str(requests[0]["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            let mut b: Value =
                serde_json::from_str(requests[1]["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            if initial_fit {
                assert_eq!(a["recent"]["omitted_for_wire_budget"], true);
                assert!(
                    a["recent"] == b["recent"],
                    "Never compact an omission marker or replace original provenance"
                );
                assert_eq!(a["recent"]["original_bytes"], 6771);
                assert_eq!(b["memory"]["omitted_for_wire_budget"], true);
                assert_eq!(
                    b["memory"]["original_sha256"],
                    crate::digest(a["memory"].to_string().as_bytes())
                );
                a.as_object_mut().unwrap().remove("memory");
                b.as_object_mut().unwrap().remove("memory");
            }
            a.as_object_mut().unwrap().remove("recent");
            b.as_object_mut().unwrap().remove("recent");
            assert!(a == b, "Repair changed nonhistorical facts or contract");
        }
    }
}
