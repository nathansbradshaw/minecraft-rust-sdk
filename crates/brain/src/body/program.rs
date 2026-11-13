//! Hand snapshots stay fenced until every dispatched durable ID resolves.
use super::{Ordering, Result, Shared};
use minecraft_sdk::Receipt;

impl Shared {
    /// The caller owns `mutation`. Recording before HTTP dispatch also covers
    /// cancellation: dropping its future cannot prove native work stopped.
    pub(crate) fn program_dispatch(&self, id: &str) -> Result<()> {
        let mut pending = self
            .program_pending
            .lock()
            .map_err(|_| "Program fence unavailable")?;
        pending.insert(id.to_owned());
        let clean = self
            .program_generation
            .load(Ordering::Acquire)
            .is_multiple_of(2);
        self.program_generation
            .fetch_add(if clean { 1 } else { 2 }, Ordering::AcqRel);
        Ok(())
    }

    pub(crate) fn resolve_program_receipt(&self, id: &str, receipt: &Receipt) -> Result<bool> {
        if receipt.action_id != id || !receipt.outcome.terminal() {
            return Ok(false);
        }
        let mut pending = self
            .program_pending
            .lock()
            .map_err(|_| "Program fence unavailable")?;
        if !pending.remove(&receipt.action_id) {
            return Ok(false);
        }
        // Serialize parity with insertion/removal, invalidating snapshots taken
        // before resolution even when another intention remains outstanding.
        self.program_generation
            .fetch_add(if pending.is_empty() { 1 } else { 2 }, Ordering::Release);
        Ok(true)
    }

    pub(crate) fn pending_program(&self) -> Result<Option<String>> {
        Ok(self
            .program_pending
            .lock()
            .map_err(|_| "Program fence unavailable")?
            .first()
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::{Arc, AtomicBool, AtomicU64, Handle, Mutex};
    use minecraft_sdk::{Action, ActionRequest, Outcome};
    use serde_json::{Value, json};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn incoming(
        listener: &tokio::net::TcpListener,
    ) -> (tokio::net::TcpStream, String, Value) {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(socket.read_u8().await.unwrap());
            assert!(headers.len() < 8192);
        }
        let header = std::str::from_utf8(&headers).unwrap();
        let size: usize = header
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        assert!(size < 8192);
        let mut bytes = vec![0; size];
        socket.read_exact(&mut bytes).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (socket, header.lines().next().unwrap().to_owned(), value)
    }
    async fn reply(socket: &mut tokio::net::TcpStream, value: Value) {
        let body = value.to_string();
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
    }
    fn receipt(id: &str, outcome: Outcome) -> Receipt {
        Receipt {
            action_id: id.into(),
            outcome,
            adapter_version: "fixture".into(),
            evidence: json!({}),
            error: None,
        }
    }

    #[tokio::test]
    async fn direct_brain_dispatch_fences_food_until_exact_id_status_resolves_cancel_or_error() {
        // Real Brain::act/inspect → SDK HTTP boundary. This proves transport
        // bookkeeping only, not gameplay or eating competence.
        for finish in ["success", "error", "cancel", "reconcile"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (_root, mut brain) = crate::admission_tests::fixture(&format!(
                "http://{}/",
                listener.local_addr().unwrap()
            ));
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
            brain.body = Some(Handle {
                shared: shared.clone(),
                socket: _root.path().join("unused.sock"),
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
            });
            let old_snapshot = shared.program_generation.load(Ordering::Acquire);
            let mut action = Box::pin(brain.act(Action::SelectSlot { slot: 1 }));
            let (mut socket, line, input) = tokio::select! {
                result = &mut action => panic!("dispatch ended before upstream admission: {result:?}"),
                accepted = incoming(&listener) => accepted,
            };
            assert!(line.starts_with("POST /v1/bots/own/actions "));
            let request: ActionRequest = serde_json::from_value(input).unwrap();
            assert!(matches!(request.action, Action::SelectSlot { slot: 1 }));
            assert_eq!(
                shared.program_generation.load(Ordering::Acquire),
                old_snapshot + 1
            );
            assert!(shared.mutation.try_lock().is_err());

            if finish == "cancel" {
                drop(action);
            } else if finish == "success" {
                reply(
                    &mut socket,
                    serde_json::to_value(receipt(&request.action_id, Outcome::Observed)).unwrap(),
                )
                .await;
                drop(socket);
                let upstream = async {
                    let (mut next, line, _) = incoming(&listener).await;
                    assert!(line.starts_with("GET ") && line.contains("/observation"));
                    reply(&mut next, json!({"revision":"after","player":{"uuid":"actor","inventory":[]},"world":{},"menu":{}})).await;
                };
                let (result, ()) = tokio::join!(action, upstream);
                result.unwrap();
            } else {
                drop(socket);
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), action)
                        .await
                        .unwrap()
                        .is_err()
                );
            }
            assert!(shared.mutation.try_lock().is_ok(), "body mutex released");
            let generation = shared.program_generation.load(Ordering::Acquire);
            assert_ne!(generation, old_snapshot, "old hand snapshot invalidated");
            if finish == "success" {
                assert!(generation.is_multiple_of(2));
                continue;
            }
            assert!(
                !generation.is_multiple_of(2),
                "uncertain work fences fresh food snapshots"
            );
            for (id, outcome) in [
                (&*request.action_id, Outcome::Running),
                (&*request.action_id, Outcome::Uncertain),
                ("other-id", Outcome::Observed),
            ] {
                assert!(
                    !shared
                        .resolve_program_receipt(&request.action_id, &receipt(id, outcome))
                        .unwrap()
                );
                assert_eq!(
                    shared.program_generation.load(Ordering::Acquire),
                    generation
                );
            }
            // A second intention can finish without certifying the first.
            shared.program_dispatch("other-id").unwrap();
            assert!(
                !shared
                    .resolve_program_receipt(
                        &request.action_id,
                        &receipt("other-id", Outcome::Observed)
                    )
                    .unwrap()
            );
            shared
                .resolve_program_receipt("other-id", &receipt("other-id", Outcome::Observed))
                .unwrap();
            assert!(
                !shared
                    .program_generation
                    .load(Ordering::Acquire)
                    .is_multiple_of(2)
            );
            let status = async {
                let (mut socket, line, _) = incoming(&listener).await;
                assert!(
                    line.starts_with(&format!("GET /v1/bots/own/actions/{} ", request.action_id))
                );
                reply(
                    &mut socket,
                    serde_json::to_value(receipt(
                        &request.action_id,
                        if finish == "reconcile" {
                            Outcome::Uncertain
                        } else {
                            Outcome::Observed
                        },
                    ))
                    .unwrap(),
                )
                .await;
                drop(socket);
                if finish == "reconcile" {
                    let (mut socket, line, _) = incoming(&listener).await;
                    assert!(line.starts_with(&format!(
                        "POST /v1/bots/own/actions/{}/reconcile ",
                        request.action_id
                    )));
                    reply(
                        &mut socket,
                        serde_json::to_value(receipt(&request.action_id, Outcome::Reconciled))
                            .unwrap(),
                    )
                    .await;
                }
            };
            let resolve = async {
                if finish == "reconcile" {
                    brain.settle(&request.action_id).await.unwrap();
                } else {
                    brain.inspect(&request.action_id).await.unwrap();
                }
            };
            tokio::join!(resolve, status);
            assert!(
                shared
                    .program_generation
                    .load(Ordering::Acquire)
                    .is_multiple_of(2)
            );
            assert!(shared.pending_program().unwrap().is_none());
            assert!(
                brain
                    .journal
                    .get("pending_action")
                    .unwrap()
                    .unwrap_or_default()
                    .is_empty()
            );
        }
    }
}
