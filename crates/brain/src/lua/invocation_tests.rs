//! Actual run-tool → Lua → HTTP/journal seam; synthetic SDK, no game/model calls.
use super::*;
use std::sync::{Mutex, atomic::AtomicBool};

struct Server {
    requests: Mutex<Vec<Value>>,
    fail: AtomicBool,
    running: AtomicBool,
    release_known: AtomicBool,
}
struct Stop(tokio::task::AbortHandle);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn observation() -> axum::Json<Value> {
    axum::Json(
        json!({"revision":"fixture","player":{"uuid":"actor","inventory":[]},"world":{},"menu":{}}),
    )
}
async fn action(
    axum::extract::State(server): axum::extract::State<Arc<Server>>,
    axum::Json(request): axum::Json<Value>,
) -> (axum::http::StatusCode, axum::Json<Value>) {
    server.requests.lock().unwrap().push(request.clone());
    if server.fail.load(Ordering::Relaxed) {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"code":"internal","message":"synthetic lost dispatch response"})),
        );
    }
    (
        axum::http::StatusCode::OK,
        receipt(&server, request["action_id"].clone()),
    )
}
fn receipt(server: &Server, id: Value) -> axum::Json<Value> {
    axum::Json(
        json!({"action_id":id,"outcome":if server.running.load(Ordering::Relaxed){"running"}else{"observed"},"adapter_version":"fixture","evidence":{"input_released":if server.release_known.load(Ordering::Relaxed){json!(!server.running.load(Ordering::Relaxed))}else{Value::Null}},"error":null}),
    )
}
async fn status(
    axum::extract::State(server): axum::extract::State<Arc<Server>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::Json<Value> {
    receipt(&server, json!(id))
}
async fn fixture(source: &str, fail: bool) -> (tempfile::TempDir, Brain, Arc<Server>, Stop) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (root, brain) =
        crate::admission_tests::fixture(&format!("http://{}/", listener.local_addr().unwrap()));
    fs::write(root.path().join("behavior.lua"), source).unwrap();
    let server = Arc::new(Server {
        requests: Mutex::new(Vec::new()),
        fail: AtomicBool::new(fail),
        running: AtomicBool::new(false),
        release_known: AtomicBool::new(true),
    });
    let app = axum::Router::new()
        .route("/v1/bots/own/observations", axum::routing::get(observation))
        .route("/v1/bots/own/actions", axum::routing::post(action))
        .route("/v1/bots/own/actions/{id}", axum::routing::get(status))
        .with_state(server.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (root, brain, server, Stop(handle.abort_handle()))
}
fn choice(input: Value) -> Choice {
    Choice {
        tool: "run".into(),
        input: input.to_string(),
        path: String::new(),
        summary: "Synthetic parameterized behavior".into(),
        notes: String::new(),
        experiment: true,
    }
}
async fn run(brain: &mut Brain, choice: &Choice) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(5), crate::tools::execute(brain, choice))
        .await
        .map_err(|_| "bounded synthetic invocation timed out")?
}
const PROGRAM: &str = "return {version=1,step=function(ctx) local s=ctx.state; if s.started then assert(ctx.args.slot==s.slot,'arguments changed between steps'); return {state=s,done=true} end; s.started=true; s.slot=ctx.args.slot; ctx.args.slot=88; return {state=s,command={tool='sdk',input={kind='select_slot',slot=s.slot}}} end}";

#[tokio::test]
async fn parameterized_run_reuses_source_with_distinct_arguments_and_resumes_after_restart() {
    let (root, mut brain, server, _stop) = fixture(PROGRAM, false).await;
    let original_hash = digest(PROGRAM.as_bytes());
    for (id, slot) in [("first", 2), ("second", 5)] {
        let result = run(
            &mut brain,
            &choice(json!({"path":"behavior.lua","invocation_id":id,"args":{"slot":slot}})),
        )
        .await
        .unwrap();
        assert_eq!(result["success"], true, "{result}");
        assert_eq!(result["done"], true);
        assert_eq!(result["state"]["slot"], slot);
    }
    assert_eq!(
        digest(&fs::read(root.path().join("behavior.lua")).unwrap()),
        original_hash
    );
    brain.journal = crate::Journal::open(&root.path().join("brain.sqlite")).unwrap();
    let resumed = run(
        &mut brain,
        &choice(json!({"path":"behavior.lua","invocation_id":"first","args":{"slot":2}})),
    )
    .await
    .unwrap();
    assert_eq!(resumed["done"], true);
    assert_eq!(resumed["steps"], 0);
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r["action"]["slot"].clone())
            .collect::<Vec<_>>(),
        vec![json!(2), json!(5)]
    );
    for changed in [
        json!({"path":"behavior.lua","invocation_id":"first","args":{"slot":3}}),
        json!({"path":"other.lua","invocation_id":"first","args":{"slot":2}}),
    ] {
        let result = run(&mut brain, &choice(changed)).await;
        assert!(
            result.is_err() || result.as_ref().is_ok_and(|v| v["success"] == false),
            "{result:?}"
        );
    }
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn new_invocation_cannot_escape_uncertain_dispatch_or_its_durable_checkpoint() {
    let (root, mut brain, server, _stop) = fixture(PROGRAM, true).await;
    let first = choice(json!({"path":"behavior.lua","invocation_id":"failed","args":{"slot":2}}));
    let result = run(&mut brain, &first).await.unwrap();
    assert_eq!(result["success"], false);
    let pending = brain.journal.get("pending_action").unwrap().unwrap();
    assert!(!pending.is_empty());
    brain.journal = crate::Journal::open(&root.path().join("brain.sqlite")).unwrap();
    for id in ["failed", "another"] {
        let result = run(
            &mut brain,
            &choice(json!({"path":"behavior.lua","invocation_id":id,"args":{"slot":2}})),
        )
        .await;
        assert!(
            result.is_err() || result.as_ref().is_ok_and(|v| v["success"] == false),
            "{result:?}"
        );
        assert_eq!(
            brain.journal.get("pending_action").unwrap().as_deref(),
            Some(pending.as_str())
        );
    }
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    fs::write(root.path().join("reviewed-successor.lua"), PROGRAM).unwrap();
    let successor = choice(
        json!({"path":"reviewed-successor.lua","invocation_id":"repaired","args":{"slot":5}}),
    );
    let blocked = run(&mut brain, &successor).await.unwrap();
    assert_eq!(blocked["success"], false);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    // Explicit original-ID reconciliation, never an ID reset or replay.
    server.release_known.store(false, Ordering::Relaxed);
    let settled = brain.settle(&pending).await.unwrap();
    assert_eq!(settled["outcome"], "observed");
    assert_eq!(
        brain.journal.get("pending_action").unwrap().as_deref(),
        Some("")
    );
    let unknown_release = run(&mut brain, &successor).await.unwrap();
    assert_eq!(
        unknown_release["success"], false,
        "cleared global pending alone is not release proof"
    );
    server.release_known.store(true, Ordering::Relaxed);
    brain.settle(&pending).await.unwrap();
    server.fail.store(false, Ordering::Relaxed);
    let same_path = run(
        &mut brain,
        &choice(json!({"path":"behavior.lua","invocation_id":"not-a-repair","args":{"slot":5}})),
    )
    .await
    .unwrap();
    assert_eq!(same_path["success"], false);
    let repaired = run(&mut brain, &successor).await.unwrap();
    assert_eq!(repaired["done"], true, "{repaired}");
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(server.requests.lock().unwrap()[0]["action_id"], pending);
    // Even after a successful successor the old unknown script step stays
    // fenced and its historical checkpoint is not cleared or replayed.
    let old = run(&mut brain, &first).await.unwrap();
    assert_eq!(old["success"], false);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn running_original_must_settle_before_switching_invocations() {
    let program = "return {version=1,step=function(ctx) local s=ctx.state; if s.started then return {state=s,done=true} end; s.started=true; return {state=s,command={tool='sdk',input={kind='start_move',target={x=ctx.args.slot,y=64,z=1},jump=false,max_ticks=5}}} end}";
    let (_root, mut brain, server, _stop) = fixture(program, false).await;
    brain
        .server
        .capabilities
        .push("minecraft:finite_locomotion".into());
    server.running.store(true, Ordering::Relaxed);
    brain.cfg.tool_timeout_secs = 1;
    let original =
        choice(json!({"path":"behavior.lua","invocation_id":"running","args":{"slot":2}}));
    let first = run(&mut brain, &original).await.unwrap();
    assert_eq!(first["waiting_for_action"], true, "{first}");
    let blocked = run(
        &mut brain,
        &choice(json!({"path":"behavior.lua","invocation_id":"different","args":{"slot":5}})),
    )
    .await;
    assert!(blocked.is_err() || blocked.as_ref().is_ok_and(|v| v["success"] == false));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    server.running.store(false, Ordering::Relaxed);
    brain.cfg.tool_timeout_secs = 5;
    let settled = run(&mut brain, &original).await.unwrap();
    assert_eq!(settled["done"], true);
    let second = run(
        &mut brain,
        &choice(json!({"path":"behavior.lua","invocation_id":"different","args":{"slot":5}})),
    )
    .await
    .unwrap();
    assert_eq!(second["done"], true);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_ne!(requests[0]["action_id"], requests[1]["action_id"]);
}

#[tokio::test]
async fn invocation_input_bounds_are_closed_and_legacy_paths_keep_their_existing_state() {
    assert_eq!(
        crate::lua::path("{legacy}.lua").unwrap().as_deref(),
        Some("{legacy}.lua")
    );
    let source = "return {version=1,step=function(ctx) assert(ctx.args==nil); local s=ctx.state; if s.n then return {state=s,done=true} end; return {state={n=1},command={tool='sdk',input={kind='select_slot',slot=1}}} end}";
    let (_root, mut brain, server, _stop) = fixture(source, false).await;
    let mut legacy = choice(Value::Null);
    legacy.input = "behavior.lua".into();
    let first = run(&mut brain, &legacy).await.unwrap();
    assert_eq!(first["done"], true);
    assert_eq!(run(&mut brain, &legacy).await.unwrap()["steps"], 0);
    for input in [
        json!({"path":"behavior.lua","invocation_id":"id","args":"x".repeat(8193)}),
        json!({"path":"behavior.lua","invocation_id":"x".repeat(97),"args":{}}),
        json!({"path":"behavior.lua","invocation_id":"../bad","args":{}}),
        json!({"path":"../behavior.lua","invocation_id":"id","args":{}}),
        json!({"path":"behavior.lua","invocation_id":"id","args":{},"profile":"other"}),
        json!({"path":"behavior.lua","invocation_id":"id"}),
    ] {
        let result = run(&mut brain, &choice(input)).await;
        assert!(
            result.is_err(),
            "invalid invocation was not rejected before tools: {result:?}"
        );
    }
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn known_read_only_failure_can_use_reviewed_successor_without_inventing_a_mutation_id() {
    let source = "return {version=1,step=function(ctx) return {state={},command={tool='query',input={capability='fixture:read_only_missing',input={}}}} end}";
    let (root, mut brain, server, _stop) = fixture(source, false).await;
    let failed = run(
        &mut brain,
        &choice(json!({"path":"behavior.lua","invocation_id":"read-failed","args":{}})),
    )
    .await
    .unwrap();
    assert_eq!(failed["success"], false);
    assert!(
        brain
            .journal
            .get("pending_action")
            .unwrap()
            .is_none_or(|id| id.is_empty())
    );
    let same = run(
        &mut brain,
        &choice(json!({"path":"behavior.lua","invocation_id":"not-repair","args":{}})),
    )
    .await
    .unwrap();
    assert_eq!(same["success"], false);
    fs::write(root.path().join("reviewed-successor.lua"), PROGRAM).unwrap();
    let repaired = run(
        &mut brain,
        &choice(
            json!({"path":"reviewed-successor.lua","invocation_id":"repair","args":{"slot":4}}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(repaired["done"], true, "{repaired}");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refused_sdk_capability_before_intention_is_not_an_unknown_dispatched_mutation() {
    let source = "return {version=1,step=function(ctx) return {state={},command={tool='sdk',input={kind='extension',capability='fixture:missing',input={}}}} end}";
    let (root, mut brain, server, _stop) = fixture(source, false).await;
    let failed = run(
        &mut brain,
        &choice(json!({"path":"behavior.lua","invocation_id":"not-admitted","args":{}})),
    )
    .await
    .unwrap();
    assert_eq!(failed["success"], false);
    assert!(server.requests.lock().unwrap().is_empty());
    fs::write(root.path().join("reviewed-successor.lua"), PROGRAM).unwrap();
    let repaired = run(
        &mut brain,
        &choice(
            json!({"path":"reviewed-successor.lua","invocation_id":"repair","args":{"slot":6}}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(repaired["done"], true, "{repaired}");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn legacy_failure_cannot_bypass_unknown_release_by_switching_to_first_named_invocation() {
    let source = "return {version=1,step=function(ctx) return {state={},command={tool='sdk',input={kind='select_slot',slot=2}}} end}";
    let (root, mut brain, server, _stop) = fixture(source, true).await;
    let mut legacy = choice(Value::Null);
    legacy.input = "behavior.lua".into();
    assert_eq!(run(&mut brain, &legacy).await.unwrap()["success"], false);
    let original = brain.journal.get("pending_action").unwrap().unwrap();
    brain.journal = crate::Journal::open(&root.path().join("brain.sqlite")).unwrap();
    fs::write(root.path().join("reviewed-successor.lua"), PROGRAM).unwrap();
    let successor = choice(
        json!({"path":"reviewed-successor.lua","invocation_id":"first-named","args":{"slot":7}}),
    );
    server.release_known.store(false, Ordering::Relaxed);
    brain.settle(&original).await.unwrap();
    assert_eq!(
        brain.journal.get("pending_action").unwrap().as_deref(),
        Some("")
    );
    let blocked = run(&mut brain, &successor).await.unwrap();
    assert_eq!(
        blocked["success"], false,
        "legacy unknown input release must fence named successors"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    server.release_known.store(true, Ordering::Relaxed);
    brain.settle(&original).await.unwrap();
    server.fail.store(false, Ordering::Relaxed);
    assert_eq!(run(&mut brain, &successor).await.unwrap()["done"], true);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(server.requests.lock().unwrap()[0]["action_id"], original);
    assert_eq!(run(&mut brain, &legacy).await.unwrap()["success"], false);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}
