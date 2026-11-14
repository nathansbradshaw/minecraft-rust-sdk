//! Worker/SDK/arbiter integration with synthetic native facts, not Minecraft.
//! The HTTP fixture rejects direct action POSTs: mutations must use the socket.
use axum::{
    Json, Router,
    extract::{Path as RoutePath, State},
    http::StatusCode,
    routing::{get, post},
};
use minecraft_sdk::{
    ActionRequest, ApiError, ErrorCode, LocalActionRequest, LocalActionResponse, Outcome, Receipt,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[derive(Default)]
struct Fixture {
    step: u8,
    block_store: bool,
    lose_take_reply: bool,
    mutations: Vec<String>,
    receipts: BTreeMap<String, Receipt>,
    direct_posts: usize,
    status_reads: Vec<String>,
}
fn observation(step: u8) -> Value {
    let (input, output, grid, cursor, stored) = match step {
        0 => (8, 0, 0, 0, 0),
        1 => (7, 4, 1, 0, 0),
        2 => (7, 0, 0, 4, 0),
        _ => (7, 0, 0, 0, 4),
    };
    json!({"revision":step.to_string(),"player":{"uuid":"own","life_id":"life",
        "inventory":[{"slot":9,"item":"fixture:input","count":input},
        {"slot":10,"item":if stored==0{"minecraft:air"}else{"fixture:result"},"count":stored}]},
        "world":{},"menu":{"menu_id":17,"state_id":step,"type":"fixture:table",
        "carried":{"item":if cursor==0{"minecraft:air"}else{"fixture:result"},"count":cursor},
        "slots":[{"slot":0,"player_inventory":false,"item":if output==0{"minecraft:air"}else{"fixture:result"},"count":output,"may_pickup":true,"may_place_carried":false},
        {"slot":1,"player_inventory":false,"item":if grid==0{"minecraft:air"}else{"fixture:input"},"count":grid,"may_pickup":true,"may_place_carried":true},
        {"slot":2,"player_inventory":true,"item":"fixture:input","count":input,"may_pickup":true,"may_place_carried":true},
        {"slot":3,"player_inventory":true,"item":if stored==0{"minecraft:air"}else{"fixture:result"},"count":stored,"may_pickup":true,"may_place_carried":true}]}})
}
type Shared = Arc<Mutex<Fixture>>;
async fn discover() -> Json<Value> {
    Json(
        json!({"api_version":1,"instance_id":"instance","minecraft_version":"fixture",
        "platform":"fixture","adapter_version":"fixture","ready":true,
        "capabilities":["minecraft:fresh_observation"],"limits":{}}),
    )
}
async fn fresh(State(state): State<Shared>, Json(query): Json<Value>) -> Json<Value> {
    assert_eq!(query["capability"], "minecraft:fresh_observation");
    Json(json!({"format":"minecraft-fresh-observation-v1",
        "observation":observation(state.lock().unwrap().step)}))
}
async fn direct(State(state): State<Shared>) -> (StatusCode, Json<Value>) {
    state.lock().unwrap().direct_posts += 1;
    (
        StatusCode::FORBIDDEN,
        Json(json!({"code":"permission_denied","message":"Use fixture arbiter"})),
    )
}
async fn status(
    State(state): State<Shared>,
    RoutePath((_bot, id)): RoutePath<(String, String)>,
) -> Json<Value> {
    let mut state = state.lock().unwrap();
    state.status_reads.push(id.clone());
    Json(
        serde_json::to_value(
            state
                .receipts
                .get(&id)
                .expect("inspect original submitted ID"),
        )
        .unwrap(),
    )
}
struct Peer {
    directory: tempfile::TempDir,
    request: PathBuf,
    result: PathBuf,
    socket: PathBuf,
    state: Shared,
    http: tokio::task::JoinHandle<()>,
    arbiter: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn start(fixture: Fixture) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let state = Arc::new(Mutex::new(fixture));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/v1/server", get(discover))
            .route("/v1/bots/{bot}/observations/query", post(fresh))
            .route("/v1/bots/{bot}/actions", post(direct))
            .route("/v1/bots/{bot}/actions/{id}", get(status))
            .with_state(state.clone());
        let http = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let socket = root.join("a.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let shared = state.clone();
        let arbiter = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio::io::BufReader::new(socket);
                let mut line = String::new();
                socket.read_line(&mut line).await.unwrap();
                let request: LocalActionRequest = serde_json::from_str(&line).unwrap();
                let ActionRequest { action_id, action } = request.request;
                let response = {
                    let mut state = shared.lock().unwrap();
                    if action_id.ends_with("-store") && state.block_store {
                        Some(LocalActionResponse::Error(ApiError::new(
                            ErrorCode::LocalPreempted,
                            "Synthetic urgent body: no native mutation submitted",
                        )))
                    } else {
                        assert!(
                            !state.receipts.contains_key(&action_id),
                            "duplicate mutation"
                        );
                        let kind = serde_json::to_value(action).unwrap();
                        match state.step {
                            0 => assert_eq!(kind["kind"], "craft"),
                            1 => assert_eq!(kind["slot"], 0),
                            2 => assert_eq!(kind["slot"], 3),
                            _ => panic!("extra batch"),
                        }
                        let prepare = state.step == 0;
                        let receipt = Receipt {
                            action_id: action_id.clone(),
                            outcome: Outcome::Observed,
                            adapter_version: "fixture".into(),
                            error: None,
                            evidence: json!({"native_effect_verified":true,"effect":if prepare {
                                json!({"kind":"recipe_preparation","recipe":"fixture:recipe",
                                    "menu_id":17,"menu_type":"fixture:table","output_slot":0,
                                    "result":{"item":"fixture:result","count":4},
                                    "verified_fields":["item","count"],"components_verified":false})
                            }else{Value::Null}}),
                        };
                        state.step += 1;
                        state.mutations.push(action_id.clone());
                        state.receipts.insert(action_id.clone(), receipt.clone());
                        if state.lose_take_reply && action_id.ends_with("-take") {
                            state.lose_take_reply = false;
                            None
                        } else {
                            Some(LocalActionResponse::Receipt(receipt))
                        }
                    }
                };
                if let Some(response) = response {
                    let mut bytes = serde_json::to_vec(&response).unwrap();
                    bytes.push(b'\n');
                    socket.get_mut().write_all(&bytes).await.unwrap();
                }
            }
        });
        let profile = root.join("profile.json");
        minecraft_sdk_brain::private_write(&profile,&serde_json::to_vec(&json!({
            "endpoint":endpoint,"instance_id":"instance","credential":{"bot_id":"bot",
            "player_uuid":"own","key":"fixture-only"},"session":{"session_id":"session","adapter_version":"fixture"}})).unwrap()).unwrap();
        let result = root.join("result.json");
        let request = root.join("request.json");
        minecraft_sdk_brain::private_write(
            &request,
            &serde_json::to_vec(&json!({"version":1,
            "profile":profile,"recipe":"fixture:recipe","prefix":"fixture-batch",
            "journal":root.join("craft.json"),"result":result,"wait_ms":5000}))
            .unwrap(),
        )
        .unwrap();
        Self {
            directory,
            request,
            result,
            socket,
            state,
            http,
            arbiter,
        }
    }
    async fn invoke(&self, executable: &str) -> Value {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(executable)
                .arg("craft-worker")
                .arg(&self.request)
                .env("MC_SDK_ARBITER", &self.socket)
                .env_remove("MC_SDK_CA_CERT")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&std::fs::read(&self.result).unwrap()).unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.http.abort();
        self.arbiter.abort();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn craft_worker_pauses_after_pickup_and_resumes_only_storage_through_arbiter() {
    check_pause_resume(env!("CARGO_BIN_EXE_minecraft-sdk-brain")).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_probe_craft_worker_pauses_after_pickup_and_resumes_only_storage_through_arbiter() {
    check_pause_resume(env!("CARGO_BIN_EXE_body-probe")).await;
}
async fn check_pause_resume(executable: &str) {
    let peer = Peer::start(Fixture {
        block_store: true,
        ..Fixture::default()
    })
    .await;
    let first = peer.invoke(executable).await;
    assert_eq!(first["report"]["status"], "local_preempted");
    assert_eq!(
        peer.state.lock().unwrap().step,
        2,
        "output preserved on cursor"
    );
    peer.state.lock().unwrap().block_store = false;
    let resumed = peer.invoke(executable).await;
    assert_eq!(resumed["report"]["status"], "completed");
    assert_eq!(resumed["report"]["current_inventory_count"], 4);
    let again = peer.invoke(executable).await;
    assert_eq!(again["report"]["status"], "already_completed");
    let state = peer.state.lock().unwrap();
    assert_eq!(
        state.mutations,
        vec![
            "fixture-batch-prepare",
            "fixture-batch-take",
            "fixture-batch-store"
        ]
    );
    assert_eq!(state.direct_posts, 0, "no direct-gateway mutation fallback");
    assert!(peer.directory.path().join("craft.json").is_file());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn craft_worker_lost_take_reply_inspects_original_id_without_replaying_mutation() {
    check_lost_reply(env!("CARGO_BIN_EXE_minecraft-sdk-brain")).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_probe_craft_worker_lost_take_reply_inspects_original_id_without_replaying_mutation() {
    check_lost_reply(env!("CARGO_BIN_EXE_body-probe")).await;
}
async fn check_lost_reply(executable: &str) {
    let peer = Peer::start(Fixture {
        lose_take_reply: true,
        ..Fixture::default()
    })
    .await;
    let first = peer.invoke(executable).await;
    assert_eq!(first["report"]["status"], "needs_inspection");
    assert_eq!(peer.state.lock().unwrap().step, 2);
    let resumed = peer.invoke(executable).await;
    assert_eq!(resumed["report"]["status"], "completed");
    let state = peer.state.lock().unwrap();
    assert_eq!(
        state.mutations,
        vec![
            "fixture-batch-prepare",
            "fixture-batch-take",
            "fixture-batch-store"
        ]
    );
    assert_eq!(state.status_reads, vec!["fixture-batch-take"]);
    assert_eq!(state.direct_posts, 0);
}
#[tokio::test]
async fn craft_worker_refuses_missing_arbiter_before_even_reading_profile() {
    check_missing_arbiter(env!("CARGO_BIN_EXE_minecraft-sdk-brain")).await;
}
#[tokio::test]
async fn body_probe_craft_worker_refuses_missing_arbiter_before_even_reading_profile() {
    check_missing_arbiter(env!("CARGO_BIN_EXE_body-probe")).await;
}
async fn check_missing_arbiter(executable: &str) {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(executable)
            .arg("craft-worker")
            .arg(Path::new("/nonexistent/fixture-request"))
            .env_remove("MC_SDK_ARBITER")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("requires the container body arbiter")
    );
}
