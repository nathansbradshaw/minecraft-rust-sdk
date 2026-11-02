use minecraft_sdk_core::*;
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, Command},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterConfig {
    pub executable: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}
pub struct Adapter {
    child: Option<Child>,
    input: Box<dyn AsyncWrite + Unpin + Send>,
    output: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    failed: bool,
    handshaken: bool,
    channel: String,
    sequence: u64,
    pub health: Arc<AtomicBool>,
}
impl Adapter {
    pub fn start(config: &AdapterConfig) -> std::io::Result<Self> {
        if !config.executable.is_absolute() {
            return Err(std::io::Error::other("Adapter executable must be absolute"));
        }
        let mut child = Command::new(&config.executable)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|_| std::io::Error::other("OS entropy unavailable"))?;
        Ok(Self {
            input: Box::new(child.stdin.take().unwrap()),
            output: BufReader::new(Box::new(child.stdout.take().unwrap())),
            child: Some(child),
            failed: false,
            handshaken: false,
            channel: nonce.iter().map(|b| format!("{b:02x}")).collect(),
            sequence: 0,
            health: Arc::new(AtomicBool::new(true)),
        })
    }
    pub async fn request(&mut self, request: &AdapterRequest) -> Result<AdapterResponse> {
        if self.failed {
            return Err(ApiError::new(
                ErrorCode::NotReady,
                "Adapter is quarantined; operator restart required",
            ));
        }
        let result = tokio::time::timeout(Duration::from_secs(30), self.exchange(request)).await;
        match result {
            Ok(Ok(response)) => Ok(response),
            _ => {
                self.failed = true;
                self.health.store(false, Ordering::Release);
                if let Some(child) = &mut self.child {
                    let _ = child.kill().await;
                }
                Err(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Adapter communication failed; do not replay mutations",
                ))
            }
        }
    }
    async fn exchange(&mut self, request: &AdapterRequest) -> std::io::Result<AdapterResponse> {
        if !self.handshaken && !matches!(request.operation, AdapterOperation::Discover) {
            return Err(std::io::Error::other("Adapter Discover handshake required"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("Adapter request ID exhausted"))?;
        let mut bytes = if self.handshaken {
            serde_json::to_vec(&AdapterFrameRequest {
                protocol: ADAPTER_VERSION,
                channel_id: self.channel.clone(),
                request_id: self.sequence,
                request: request.clone(),
            })?
        } else {
            serde_json::to_vec(request)?
        };
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        let mut line = Vec::new();
        // Bound before allocation, including a hostile/malfunctioning trusted plugin.
        loop {
            let buffer = self.output.fill_buf().await?;
            if buffer.is_empty() {
                return Err(std::io::Error::other("Adapter closed"));
            }
            let n = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map(|n| n + 1)
                .unwrap_or(buffer.len());
            if line.len() + n > 8 * 1_048_576 {
                return Err(std::io::Error::other("Adapter response too large"));
            }
            let end = buffer[n - 1] == b'\n';
            line.extend_from_slice(&buffer[..n]);
            self.output.consume(n);
            if end {
                break;
            }
        }
        if !self.handshaken {
            let response: AdapterResponse = serde_json::from_slice(&line)?;
            if let AdapterResponse::Info(info) = &response {
                if info.limits["process_protocol"] != ADAPTER_VERSION {
                    return Err(std::io::Error::other(
                        "Unsupported adapter process protocol",
                    ));
                }
                self.handshaken = true;
            }
            return Ok(response);
        }
        let response: AdapterFrameResponse = serde_json::from_slice(&line)?;
        if response.protocol != ADAPTER_VERSION
            || response.channel_id != self.channel
            || response.request_id != self.sequence
            || response.instance_id != request.instance_id
            || response.bot_id != request.bot_id
            || response.player_uuid != request.player_uuid
        {
            return Err(std::io::Error::other(
                "Adapter response correlation/actor mismatch",
            ));
        }
        let valid = match (&request.operation, &response.response) {
            (_, AdapterResponse::Error(_)) => true,
            (AdapterOperation::Discover, AdapterResponse::Info(_)) => true,
            (AdapterOperation::Connect, AdapterResponse::Connected) => true,
            (AdapterOperation::Disconnect, AdapterResponse::Disconnected) => true,
            (AdapterOperation::Observe, AdapterResponse::Observation(o)) => {
                o.player["uuid"] == request.player_uuid
            }
            (AdapterOperation::ReadObservation { .. }, AdapterResponse::ObservationData(_)) => true,
            (AdapterOperation::Action { request: action }, AdapterResponse::Receipt(receipt)) => {
                receipt.action_id == action.action_id
            }
            (AdapterOperation::Reconcile { action_id }, AdapterResponse::Receipt(receipt)) => {
                &receipt.action_id == action_id
            }
            _ => false,
        };
        if !valid {
            return Err(std::io::Error::other(
                "Adapter response payload/operation mismatch",
            ));
        }
        Ok(response.response)
    }
}

struct ActorSlot {
    identity: (String, String, String),
    adapter: tokio::sync::Mutex<Option<Adapter>>,
}
type Slot = Arc<ActorSlot>;
type Factory = Arc<dyn Fn() -> std::io::Result<Adapter> + Send + Sync>;
/// One configured process per connected character. Registry entries and queued
/// requests are separate from live player capacity; no SDK bot-count policy.
pub struct AdapterPool {
    factory: Factory,
    bootstrap: tokio::sync::Mutex<Adapter>,
    slots: Mutex<HashMap<String, Slot>>,
    version: String,
    instance: String,
}
impl AdapterPool {
    pub fn new(config: AdapterConfig, bootstrap: Adapter, info: &ServerInfo) -> Self {
        Self {
            factory: Arc::new(move || Adapter::start(&config)),
            bootstrap: tokio::sync::Mutex::new(bootstrap),
            slots: Mutex::new(HashMap::new()),
            version: info.adapter_version.clone(),
            instance: info.instance_id.clone(),
        }
    }
    pub async fn request(&self, request: &AdapterRequest) -> Result<AdapterResponse> {
        if request.bot_id.is_empty() && matches!(request.operation, AdapterOperation::Discover) {
            return self.bootstrap.lock().await.request(request).await;
        }
        let connect = matches!(request.operation, AdapterOperation::Connect);
        let slot = {
            let mut slots = self.slots.lock().map_err(|_| {
                ApiError::new(ErrorCode::Internal, "Adapter lane registry unavailable")
            })?;
            if connect {
                slots
                    .entry(request.bot_id.clone())
                    .or_insert_with(|| {
                        Arc::new(ActorSlot {
                            identity: (
                                request.instance_id.clone(),
                                request.player_uuid.clone(),
                                request.player_name.clone(),
                            ),
                            adapter: tokio::sync::Mutex::new(None),
                        })
                    })
                    .clone()
            } else {
                slots.get(&request.bot_id).cloned().ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::SessionRequired,
                        "Character adapter is not connected",
                    )
                })?
            }
        };
        if slot.identity
            != (
                request.instance_id.clone(),
                request.player_uuid.clone(),
                request.player_name.clone(),
            )
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Character lane identity changed",
            ));
        }
        let mut owned = slot.adapter.lock().await;
        let result = async {
            if owned.is_none() {
                if !connect {
                    return Err(ApiError::new(
                        ErrorCode::SessionRequired,
                        "Character adapter ended",
                    ));
                }
                let mut child = (self.factory)().map_err(|_| {
                    ApiError::new(ErrorCode::NotReady, "Cannot start character adapter")
                })?;
                let response = child
                    .request(&AdapterRequest {
                        protocol: ADAPTER_VERSION,
                        instance_id: self.instance.clone(),
                        bot_id: String::new(),
                        player_uuid: String::new(),
                        player_name: String::new(),
                        operation: AdapterOperation::Discover,
                    })
                    .await?;
                let AdapterResponse::Info(info) = response else {
                    return Err(ApiError::new(
                        ErrorCode::UnsupportedVersion,
                        "Character adapter handshake failed",
                    ));
                };
                if info.adapter_version != self.version || info.api_version != API_VERSION {
                    return Err(ApiError::new(
                        ErrorCode::UnsupportedVersion,
                        "Adapter version changed; active sessions stay pinned",
                    ));
                }
                *owned = Some(child);
            }
            owned.as_mut().unwrap().request(request).await
        }
        .await;
        let retire = matches!(request.operation, AdapterOperation::Disconnect)
            || result.is_err()
            || matches!(
                &result,
                Ok(AdapterResponse::Error(ApiError {
                    code: ErrorCode::SessionRequired,
                    ..
                }))
            )
            || (connect && !matches!(&result, Ok(AdapterResponse::Connected)));
        if retire {
            owned.take();
            let mut slots = self.slots.lock().map_err(|_| {
                ApiError::new(ErrorCode::Internal, "Adapter lane registry unavailable")
            })?;
            if slots
                .get(&request.bot_id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &slot))
            {
                slots.remove(&request.bot_id);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    struct Fixture {
        started: tokio::sync::Notify,
    }
    fn info(instance: &str) -> ServerInfo {
        ServerInfo {
            api_version: API_VERSION,
            instance_id: instance.into(),
            minecraft_version: "1.21.1".into(),
            platform: "fixture".into(),
            adapter_version: "fixture-v2".into(),
            ready: true,
            capabilities: vec!["minecraft:movement".into()],
            limits: json!({"process_protocol":ADAPTER_VERSION}),
        }
    }
    fn request(bot: &str, operation: AdapterOperation) -> AdapterRequest {
        AdapterRequest {
            protocol: ADAPTER_VERSION,
            instance_id: "f".repeat(64),
            bot_id: bot.into(),
            player_uuid: format!("{bot}-uuid"),
            player_name: format!("bot_{bot}"),
            operation,
        }
    }
    fn fixture(state: Arc<Fixture>) -> Adapter {
        fixture_version(state, "fixture-v2")
    }
    fn fixture_version(state: Arc<Fixture>, version: &'static str) -> Adapter {
        let (client, server) = tokio::io::duplex(262144);
        let (input, mut output) = tokio::io::split(server);
        tokio::spawn(async move {
            let mut input = BufReader::new(input);
            loop {
                let mut line = String::new();
                if input.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let raw: Value = serde_json::from_str(&line).unwrap();
                let frame: Option<AdapterFrameRequest> = raw
                    .get("request")
                    .map(|_| serde_json::from_value(raw.clone()).unwrap());
                let req: AdapterRequest = frame
                    .as_ref()
                    .map(|f| f.request.clone())
                    .unwrap_or_else(|| serde_json::from_value(raw).unwrap());
                if req.bot_id == "socket" && matches!(req.operation, AdapterOperation::Observe) {
                    break;
                }
                let response = match &req.operation {
                    AdapterOperation::Discover => {
                        let mut value = info(&req.instance_id);
                        value.adapter_version = version.into();
                        AdapterResponse::Info(value)
                    }
                    AdapterOperation::Connect => AdapterResponse::Connected,
                    AdapterOperation::Disconnect => AdapterResponse::Disconnected,
                    AdapterOperation::Observe => AdapterResponse::Observation(Observation {
                        revision: "fixture".into(),
                        player: json!({"uuid":if req.bot_id=="payload" {"foreign-uuid".into()}else{req.player_uuid.clone()}}),
                        world: Value::Null,
                        menu: Value::Null,
                    }),
                    AdapterOperation::Action { request } => {
                        if req.bot_id == "slow" {
                            state.started.notify_one();
                            tokio::time::sleep(Duration::from_millis(350)).await;
                        }
                        AdapterResponse::Receipt(Receipt {
                            action_id: request.action_id.clone(),
                            outcome: Outcome::Observed,
                            adapter_version: "fixture-v2".into(),
                            evidence: json!({"fixture_only":true}),
                            error: None,
                        })
                    }
                    _ => AdapterResponse::Error(ApiError::new(
                        ErrorCode::UnsupportedCapability,
                        "Fixture operation",
                    )),
                };
                let value = if let Some(frame) = frame {
                    let mut result = AdapterFrameResponse {
                        protocol: ADAPTER_VERSION,
                        channel_id: frame.channel_id,
                        request_id: frame.request_id,
                        instance_id: req.instance_id,
                        bot_id: req.bot_id.clone(),
                        player_uuid: req.player_uuid,
                        response,
                    };
                    if req.bot_id == "forged" && matches!(req.operation, AdapterOperation::Observe)
                    {
                        result.request_id += 1;
                    }
                    if req.bot_id == "actor" && matches!(req.operation, AdapterOperation::Observe) {
                        result.bot_id = "foreign".into();
                    }
                    serde_json::to_vec(&result).unwrap()
                } else {
                    serde_json::to_vec(&response).unwrap()
                };
                if output.write_all(&value).await.is_err() || output.write_all(b"\n").await.is_err()
                {
                    break;
                }
            }
        });
        let (input, output) = tokio::io::split(client);
        Adapter {
            child: None,
            input: Box::new(output),
            output: BufReader::new(Box::new(input)),
            failed: false,
            handshaken: false,
            channel: "a".repeat(64),
            sequence: 0,
            health: Arc::new(AtomicBool::new(true)),
        }
    }
    fn pool(state: Arc<Fixture>) -> Arc<AdapterPool> {
        let initial = fixture(state.clone());
        Arc::new(AdapterPool {
            factory: Arc::new(move || Ok(fixture(state.clone()))),
            bootstrap: tokio::sync::Mutex::new(initial),
            slots: Mutex::new(HashMap::new()),
            version: "fixture-v2".into(),
            instance: "f".repeat(64),
        })
    }
    #[tokio::test]
    async fn four_lanes_make_progress_while_one_actor_runs_a_long_action() {
        let state = Arc::new(Fixture {
            started: tokio::sync::Notify::new(),
        });
        let pool = pool(state.clone());
        for bot in ["slow", "fast-a", "fast-b", "fast-c"] {
            assert!(matches!(
                pool.request(&request(bot, AdapterOperation::Connect))
                    .await
                    .unwrap(),
                AdapterResponse::Connected
            ));
        }
        let action = request(
            "slow",
            AdapterOperation::Action {
                request: ActionRequest {
                    action_id: "slow-1".into(),
                    action: Action::Look {
                        yaw: 15.,
                        pitch: 0.,
                    },
                },
            },
        );
        let active = pool.clone();
        let slow = tokio::spawn(async move { active.request(&action).await });
        state.started.notified().await;
        for bot in ["fast-a", "fast-b", "fast-c"] {
            let response = tokio::time::timeout(
                Duration::from_millis(150),
                pool.request(&request(bot, AdapterOperation::Observe)),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(matches!(response, AdapterResponse::Observation(_)));
        }
        assert!(!slow.is_finished());
        assert!(slow.await.unwrap().is_ok());
    }
    #[tokio::test]
    async fn socket_or_forged_replies_quarantine_only_their_own_character() {
        let pool = pool(Arc::new(Fixture {
            started: tokio::sync::Notify::new(),
        }));
        for bot in ["socket", "forged", "actor", "payload", "healthy"] {
            pool.request(&request(bot, AdapterOperation::Connect))
                .await
                .unwrap();
        }
        for bot in ["socket", "forged", "actor", "payload"] {
            assert!(
                pool.request(&request(bot, AdapterOperation::Observe))
                    .await
                    .is_err()
            );
            assert!(matches!(
                pool.request(&request("healthy", AdapterOperation::Observe))
                    .await
                    .unwrap(),
                AdapterResponse::Observation(_)
            ));
        }
    }
    #[tokio::test]
    async fn a_lane_cannot_be_rebound_to_a_different_identity() {
        let pool = pool(Arc::new(Fixture {
            started: tokio::sync::Notify::new(),
        }));
        pool.request(&request("healthy", AdapterOperation::Connect))
            .await
            .unwrap();
        let mut forged = request("healthy", AdapterOperation::Observe);
        forged.player_uuid = "someone-else".into();
        assert_eq!(
            pool.request(&forged).await.unwrap_err().code,
            ErrorCode::Conflict
        );
        assert!(
            pool.request(&request("healthy", AdapterOperation::Observe))
                .await
                .is_ok()
        );
        pool.request(&request("healthy", AdapterOperation::Disconnect))
            .await
            .unwrap();
        assert!(!pool.slots.lock().unwrap().contains_key("healthy"));
    }
    #[tokio::test]
    async fn changed_binary_versions_cannot_join_existing_pinned_sessions() {
        let state = Arc::new(Fixture {
            started: tokio::sync::Notify::new(),
        });
        let mut pool = pool(state.clone());
        pool.request(&request("existing", AdapterOperation::Connect))
            .await
            .unwrap();
        Arc::get_mut(&mut pool).unwrap().factory =
            Arc::new(move || Ok(fixture_version(state.clone(), "changed-version")));
        assert_eq!(
            pool.request(&request("new", AdapterOperation::Connect))
                .await
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedVersion
        );
        assert!(
            pool.request(&request("existing", AdapterOperation::Observe))
                .await
                .is_ok()
        );
        assert!(!pool.slots.lock().unwrap().contains_key("new"));
    }
    #[tokio::test]
    async fn abandoned_http_waiters_still_drain_replies_and_hold_queue_budget() {
        let state = Arc::new(Fixture {
            started: tokio::sync::Notify::new(),
        });
        let pool = pool(state.clone());
        let pool = Arc::try_unwrap(pool).ok().unwrap();
        let dir = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = crate::store::Store::open(&dir.path().join("gateway.sqlite")).unwrap();
        let gateway = Arc::new(crate::Gateway {
            store: Mutex::new(store),
            adapter: pool,
            info: info(&"f".repeat(64)),
            registration_times: Mutex::new(std::collections::VecDeque::new()),
            registration_limit: 10,
            lanes: Mutex::new(HashMap::new()),
            health: Arc::new(AtomicBool::new(true)),
            adapter_budget: Arc::new(tokio::sync::Semaphore::new(2)),
        });
        let actor = crate::store::Actor {
            bot_id: "slow".into(),
            uuid: "slow-uuid".into(),
            name: "bot_slow".into(),
        };
        gateway
            .call(&actor, AdapterOperation::Connect)
            .await
            .unwrap();
        let active = gateway.clone();
        let a = actor.clone();
        let waiter = tokio::spawn(async move {
            active
                .call(
                    &a,
                    AdapterOperation::Action {
                        request: ActionRequest {
                            action_id: "drain-1".into(),
                            action: Action::Look {
                                yaw: 15.,
                                pitch: 0.,
                            },
                        },
                    },
                )
                .await
        });
        state.started.notified().await;
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(gateway.adapter_budget.available_permits(), 1);
        let response = gateway
            .call(&actor, AdapterOperation::Observe)
            .await
            .unwrap();
        assert!(matches!(response, AdapterResponse::Observation(_)));
        assert_eq!(gateway.adapter_budget.available_permits(), 2);
    }
}
