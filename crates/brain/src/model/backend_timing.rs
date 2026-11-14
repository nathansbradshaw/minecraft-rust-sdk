//! Optional backend self-reported durations; never request or gameplay authority.
use serde_json::{Value, json};

const MAX_NS: u64 = 86_400_000_000_000; // One day; absurd durations stay unknown.
const PHASES: [&str; 6] = [
    "request_total_ns",
    "mcp_discovery_ns",
    "mcp_disabled_verification_ns",
    "exec_spawn_ns",
    "exec_wait_drain_ns",
    "validation_ns",
];
fn duration(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= MAX_NS)
}
fn timing(value: &Value) -> Option<Value> {
    let fields = value.as_object()?;
    if fields.len() != PHASES.len() + 2
        || fields.get("format")?.as_str()? != "planner_phase_timing_v1"
        || fields.get("scope")?.as_str()? != "request_attempt"
    {
        return None;
    }
    let mut view = json!({"format":"planner_phase_timing_v1","scope":"request_attempt"});
    for key in PHASES {
        let value = fields.get(key)?;
        view[key] = if value.is_null() {
            Value::Null
        } else {
            json!(duration(value)?)
        };
    }
    Some(view)
}

pub(super) fn usage(envelope: &Value) -> Value {
    let request_id = envelope["request_id"].as_str().filter(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    json!({
        "backend_request_id":request_id,
        "backend_timing":timing(&envelope["timing"]),
        "backend_total_duration_ns":duration(&envelope["total_duration"])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Model, tests::read_request};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    struct Stop(tokio::task::AbortHandle);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    fn phases() -> Value {
        json!({"format":"planner_phase_timing_v1","scope":"request_attempt",
            "request_total_ns":1_500_000_000u64,"mcp_discovery_ns":20_000,
            "mcp_disabled_verification_ns":null,"exec_spawn_ns":100_000,
            "exec_wait_drain_ns":1_400_000_000u64,"validation_ns":0})
    }
    #[test]
    fn optional_timing_keeps_only_the_closed_validated_fields_and_unknowns() {
        let envelope = json!({"request_id":"0123456789abcdef0123456789abcdef","timing":phases(),"total_duration":1_500_000_000u64,
            "private_prompt":"never-copy-this","unknown_root":{"secret":"never-copy-this"}});
        let view = usage(&envelope);
        assert_eq!(view["backend_request_id"], envelope["request_id"]);
        assert_eq!(view["backend_timing"], phases());
        assert_eq!(view["backend_timing"]["validation_ns"], 0);
        assert_eq!(
            view["backend_timing"]["mcp_disabled_verification_ns"],
            Value::Null
        );
        assert_eq!(view["backend_total_duration_ns"], 1_500_000_000u64);
        assert_eq!(view.as_object().unwrap().len(), 3);
        assert!(!view.to_string().contains("never-copy-this"));
        assert!(view.to_string().len() < 768);
        assert_eq!(
            usage(&json!({})),
            json!({"backend_request_id":null,"backend_timing":null,"backend_total_duration_ns":null})
        );
        for invalid in [
            json!(-1),
            json!(1.5),
            json!("50"),
            json!(true),
            json!(MAX_NS + 1),
            json!(u64::MAX),
            json!({"private":"x".repeat(10000)}),
        ] {
            let mut broken = envelope.clone();
            broken["timing"]["exec_spawn_ns"] = invalid.clone();
            broken["total_duration"] = invalid;
            let view = usage(&broken);
            assert!(view["backend_timing"].is_null());
            assert!(view["backend_total_duration_ns"].is_null());
            assert!(!view.to_string().contains("private"));
        }
        for id in [
            "x".repeat(10000),
            "A".repeat(32),
            "g".repeat(32),
            "0".repeat(31),
            "é".repeat(16),
        ] {
            assert!(usage(&json!({"request_id":id}))["backend_request_id"].is_null());
        }
        for broken in [
            {
                let mut t = phases();
                t["private"] = json!("never-copy-this");
                t
            },
            {
                let mut t = phases();
                t.as_object_mut().unwrap().remove("validation_ns");
                t
            },
            {
                let mut t = phases();
                t["scope"] = json!("whole_session");
                t
            },
            {
                let mut t = phases();
                t["format"] = json!("planner_phase_timing_v2");
                t
            },
            Value::Null,
        ] {
            assert!(usage(&json!({"timing":broken}))["backend_timing"].is_null());
        }
    }

    #[tokio::test]
    async fn accepted_http_response_retains_valid_backend_metadata_without_changing_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let context = json!({"goal":"Synthetic metadata reader only","pending_action":"unchanged-original-id"});
        let expected = context.clone();
        let mut server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert_eq!(request["model"], "gpt-6-luna");
            assert_eq!(request["purpose"], "plan");
            assert_eq!(request["options"], json!({"num_predict":16}));
            assert_eq!(request.as_object().unwrap().len(), 6);
            assert_eq!(request["messages"][0]["content"], "Synthetic prompt");
            assert_eq!(
                serde_json::from_str::<Value>(request["messages"][1]["content"].as_str().unwrap())
                    .unwrap(),
                expected
            );
            let response=json!({"done":true,"message":{"content":"accepted"},"request_id":"0123456789abcdef0123456789abcdef",
                "timing":phases(),"total_duration":1_500_000_000u64,"secret_extra":"never-copy-this"}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
        });
        let _stop = Stop(server.abort_handle());
        let model = Model {
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            endpoint,
        };
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            model.request(
                &context,
                "gpt-6-luna",
                "plan",
                json!({"type":"object"}),
                "Synthetic prompt",
                16,
            ),
        )
        .await;
        let joined = match tokio::time::timeout(Duration::from_secs(1), &mut server).await {
            Ok(joined) => joined,
            Err(_) => {
                server.abort();
                panic!("bounded HTTP fixture did not finish");
            }
        };
        joined.unwrap();
        let (content, usage) = result.unwrap().unwrap();
        assert_eq!(content, "accepted");
        assert_eq!(
            usage["backend_request_id"],
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(usage["backend_timing"], phases());
        assert_eq!(usage["backend_total_duration_ns"], 1_500_000_000u64);
        assert_eq!(usage["transport_attempts"], 1);
        assert_eq!(usage["host_tools_executed"], false);
        assert!(!usage.to_string().contains("never-copy-this"));
        let root = tempfile::tempdir().unwrap();
        let journal = crate::Journal::open(&root.path().join("brain.sqlite")).unwrap();
        let event = json!({"choice":{"tool":"report","input":"Synthetic operator metadata only","path":"","summary":"Fixture","notes":"","experiment":false},"usage":usage});
        let mut before = event.clone();
        for key in [
            "backend_request_id",
            "backend_timing",
            "backend_total_duration_ns",
        ] {
            before["usage"].as_object_mut().unwrap().remove(key);
        }
        journal.event("choice", before.clone()).unwrap();
        let prior = journal.recent().unwrap()[0].clone();
        journal.event("choice", event.clone()).unwrap();
        assert_eq!(
            journal
                .recent()
                .unwrap()
                .as_array()
                .unwrap()
                .last()
                .unwrap(),
            &prior,
            "operator telemetry must not change automatic future model history"
        );
        let raw: String = journal
            .db
            .query_row(
                "SELECT data FROM events WHERE kind='choice' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&raw).unwrap(),
            event,
            "authoritative operator evidence stays complete"
        );
    }
}
