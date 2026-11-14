//! Pure Lua decisions, resident Rust execution. Reload only at settled boundaries.
use crate::{Brain, Result, digest, ensure, model::Choice};
use mlua::{HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, VmState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

const SOURCE_LIMIT: usize = 32768;
const STATE_LIMIT: usize = 16384;
const STEP_LIMIT: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    state: Value,
    command: Option<Command>,
    #[serde(default)]
    done: bool,
}
#[derive(Deserialize)]
#[serde(
    tag = "tool",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Command {
    Observe,
    Query(minecraft_sdk::ObservationQuery),
    Sdk(minecraft_sdk::Action),
    Status(String),
    Reconcile(String),
    Navigate(crate::navigation::ToolGoal),
}
impl Command {
    fn choice(&self) -> Result<Choice> {
        let (tool, input) = match self {
            Self::Observe => ("observe", String::new()),
            Self::Query(query) => {
                query.validate()?;
                ("query", serde_json::to_string(query)?)
            }
            Self::Sdk(action) => {
                action.validate()?;
                ("sdk", serde_json::to_string(action)?)
            }
            Self::Status(id) => ("status", id.clone()),
            Self::Reconcile(id) => ("reconcile", id.clone()),
            Self::Navigate(goal) => {
                goal.validate()?;
                ("navigate", serde_json::to_string(goal)?)
            }
        };
        let choice = Choice {
            tool: tool.into(),
            input,
            path: String::new(),
            summary: "Lua behavior step".into(),
            notes: String::new(),
            experiment: false,
        };
        crate::model::validate_choice(&choice)?;
        Ok(choice)
    }
}

fn step(source: &str, context: &Value) -> Result<Step> {
    ensure(
        source.len() <= SOURCE_LIMIT,
        "Lua source exceeds32768 bytes",
    )?;
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
        LuaOptions::default(),
    )?;
    lua.set_memory_limit(16 * 1024 * 1024)?;
    // No host I/O, modules, dynamic loaders, or ways to swallow a budget error.
    for name in [
        "dofile",
        "loadfile",
        "load",
        "require",
        "collectgarbage",
        "pcall",
        "xpcall",
        "print",
    ] {
        lua.globals().set(name, mlua::Value::Nil)?;
    }
    let ticks = Arc::new(AtomicU32::new(0));
    let started = Instant::now();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1000),
        move |_, _| {
            if ticks.fetch_add(1, Ordering::Relaxed) >= 99
                || started.elapsed() > Duration::from_millis(100)
            {
                return Err(mlua::Error::RuntimeError(
                    "Lua instruction/time budget exceeded".into(),
                ));
            }
            Ok(VmState::Continue)
        },
    )?;
    // The module is evaluated in an isolated VM and has no gameplay functions.
    // A failed candidate cannot dispatch an action or mutate saved state.
    let module: mlua::Table = lua
        .load(source)
        .set_name("behavior.lua")
        .set_mode(mlua::chunk::ChunkMode::Text)
        .eval()?;
    ensure(
        module.get::<u32>("version")? == 1,
        "Lua module version1 required",
    )?;
    let function: mlua::Function = module.get("step")?;
    let answer: mlua::Value = function.call(lua.to_value(context)?)?;
    let answer: Step = lua.from_value(answer)?;
    ensure(
        serde_json::to_vec(&answer.state)?.len() <= STATE_LIMIT,
        "Lua state exceeds16384 bytes",
    )?;
    ensure(
        answer.done == answer.command.is_none(),
        "Return either one command or done=true",
    )?;
    if let Some(command) = &answer.command {
        command.choice()?;
    }
    Ok(answer)
}

fn source(root: &Path, input: &str) -> Result<String> {
    let relative = Path::new(input);
    ensure(
        !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            && relative.extension().is_some_and(|e| e == "lua"),
        "Use a relative workspace .lua path",
    )?;
    let root = root.canonicalize()?;
    let path = root.join(relative);
    ensure(
        path.canonicalize()?.starts_with(&root) && fs::symlink_metadata(&path)?.is_file(),
        "Lua source must be a regular workspace file",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(&path)?
        .take(SOURCE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure(bytes.len() <= SOURCE_LIMIT, "Lua source exceeds32768 bytes")?;
    Ok(String::from_utf8(bytes)?)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    state: Value,
    result: Value,
    in_flight: bool,
    #[serde(default)]
    pending_action: Option<String>,
}
impl Default for Checkpoint {
    fn default() -> Self {
        Self {
            state: json!({}),
            result: Value::Null,
            in_flight: false,
            pending_action: None,
        }
    }
}

/// Run a bounded behavior slice without replacing the Brain, body or SDK session.
pub async fn execute(brain: &mut Brain, path: &str) -> Result<Value> {
    // Validate the lexical path even if the file is currently unavailable.
    let relative = Path::new(path);
    ensure(
        !path.is_empty()
            && !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            && relative.extension().is_some_and(|e| e == "lua"),
        "Use a relative workspace .lua path",
    )?;
    let revision = brain.journal.get("goal_revision")?.unwrap_or_default();
    let key = digest(
        json!([
            brain.cfg.workspace.canonicalize()?,
            path,
            brain.server.instance_id,
            brain.observed.player["uuid"],
            revision
        ])
        .to_string()
        .as_bytes(),
    );
    let checkpoint_key = format!("lua-checkpoint-{key}");
    let active_key = format!("lua-source-{key}");
    let mut checkpoint: Checkpoint = brain
        .journal
        .get(&checkpoint_key)?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default();
    ensure(
        !checkpoint.in_flight,
        "Lua step was interrupted or failed; inspect/reconcile existing action IDs and use a reviewed successor path; no automatic replay",
    )?;
    let mut active = brain.journal.get(&active_key)?;
    let initial_inventory = brain.observed.player["inventory"].clone();
    let started = Instant::now();
    let mut reloads = 0;
    let mut rejected = None;
    let mut previous_rejection = String::new();
    let mut feedback = Vec::new();
    for index in 0..STEP_LIMIT {
        ensure(
            brain.journal.get("goal_revision")?.unwrap_or_default() == revision,
            "Lua goal changed; saved state retained",
        )?;
        if started.elapsed() >= Duration::from_secs(brain.cfg.tool_timeout_secs) {
            break;
        }
        // Pending gameplay is inspected, never resubmitted. No script version
        // may advance until the original receipt reaches a settled outcome.
        let pending = checkpoint
            .pending_action
            .clone()
            .or(brain
                .journal
                .get("pending_action")?
                .filter(|id| !id.is_empty()))
            .or(brain
                .body
                .as_ref()
                .map(|body| body.shared.pending_program())
                .transpose()?
                .flatten())
            .unwrap_or_default();
        if !pending.is_empty() {
            loop {
                let remaining = Duration::from_secs(brain.cfg.tool_timeout_secs)
                    .saturating_sub(started.elapsed());
                let receipt = match tokio::time::timeout(remaining, brain.inspect(&pending)).await {
                    Ok(result) => result?,
                    Err(_) => {
                        return Ok(
                            json!({"language":"lua","success":true,"done":false,"waiting_for_action":true,"pending_action":pending,"resume":"Run the same script to inspect this original ID; no replay"}),
                        );
                    }
                };
                if receipt.outcome.terminal() {
                    checkpoint.result["receipt"] = serde_json::to_value(receipt)?;
                    checkpoint.pending_action = None;
                    break;
                }
                if receipt.outcome == minecraft_sdk::Outcome::Uncertain {
                    return Ok(
                        json!({"language":"lua","success":false,"done":false,"waiting_for_action":true,"pending_action":pending,"outcome":receipt.outcome,"resume":"Explicitly inspect/reconcile this original ID before resuming; no replay"}),
                    );
                }
                // Finite controls stay inside this behavior slice. Another model
                // turn is not needed for every movement/status poll.
                tokio::time::sleep(Duration::from_millis(100).min(remaining)).await;
            }
        }
        brain.refresh().await?;
        ensure(
            brain.journal.get("goal_revision")?.unwrap_or_default() == revision,
            "Lua goal changed; saved state retained",
        )?;
        let context = json!({"state":checkpoint.state,"last_result":checkpoint.result,"observation":brain.observed,"goal":brain.goal,"goal_revision":revision});
        let candidate = source(&brain.cfg.workspace, path);
        let next = match candidate {
            Ok(candidate) if active.as_ref() != Some(&candidate) => {
                match step(&candidate, &context) {
                    Ok(answer) => {
                        brain.journal.set(&active_key, &candidate)?;
                        brain.journal.event("lua_reloaded", json!({"path":path,"sha256":digest(candidate.as_bytes()),"boundary":"settled action","brain_pid":std::process::id(),"run":brain.run_id,"instance":brain.server.instance_id,"uuid":brain.observed.player["uuid"],"entity_id":brain.observed.player["entity_id"],"goal_revision":revision,"session_replaced":false}))?;
                        active = Some(candidate);
                        reloads += 1;
                        answer
                    }
                    Err(error) => {
                        let message = error.to_string();
                        rejected = Some(message.clone());
                        if previous_rejection != message {
                            brain.journal.event("lua_reload_rejected", json!({"path":path,"error":message,"last_good_retained":active.is_some()}))?;
                            previous_rejection = message;
                        }
                        step(active.as_deref().ok_or(error)?, &context)?
                    }
                }
            }
            Ok(_) => step(active.as_deref().ok_or("Lua source unavailable")?, &context)?,
            Err(error) => {
                rejected = Some(error.to_string());
                step(active.as_deref().ok_or(error)?, &context)?
            }
        };
        ensure(
            brain.journal.get("goal_revision")?.unwrap_or_default() == revision,
            "Lua goal changed during decision; no operation dispatched",
        )?;
        if !next.done && started.elapsed() >= Duration::from_secs(brain.cfg.tool_timeout_secs) {
            break;
        }
        checkpoint.state = next.state;
        if next.done {
            brain
                .journal
                .set(&checkpoint_key, &serde_json::to_string(&checkpoint)?)?;
            return Ok(
                json!({"language":"lua","success":true,"exit_code":0,"done":true,"steps":index,"reloads":reloads,"rejected_update":rejected,"source_sha256":active.as_ref().map(|s|digest(s.as_bytes())),"state":checkpoint.state,"last_result":checkpoint.result,"inventory_changed":initial_inventory!=brain.observed.player["inventory"],"inventory_delta":crate::feedback::inventory_delta(&initial_inventory,&brain.observed.player["inventory"]),"local_action_feedback":{"scope":"resident Lua SDK dispatches","from_sequence":0,"through_sequence":feedback.len(),"omitted_records":0,"requests":feedback},"authority":"Client execution; not proof of gameplay completion"}),
            );
        }
        let command = next.command.ok_or("Lua command required")?.choice()?;
        checkpoint.in_flight = true;
        // Commit state before executing anything. An interrupted/failed step is
        // fenced across process restarts, including the uncertain-dispatch gap.
        brain
            .journal
            .set(&checkpoint_key, &serde_json::to_string(&checkpoint)?)?;
        let result = tokio::time::timeout(
            Duration::from_secs(brain.cfg.tool_timeout_secs).saturating_sub(started.elapsed()),
            Box::pin(crate::tools::execute(brain, &command)),
        )
        .await
        .map_err(|_| "Lua action slice timed out; inspect pending IDs, never replay")??;
        if result["request"].is_object() {
            feedback.push(json!({"request":result["request"],"response":crate::feedback::receipt_summary(&serde_json::from_value(result["receipt"].clone())?)}));
        }
        checkpoint.result = result;
        checkpoint.pending_action = checkpoint.result["receipt"]
            .as_object()
            .and_then(|_| {
                serde_json::from_value::<minecraft_sdk::Receipt>(
                    checkpoint.result["receipt"].clone(),
                )
                .ok()
            })
            .filter(|receipt| !receipt.outcome.terminal())
            .map(|receipt| receipt.action_id)
            .or_else(|| {
                checkpoint.result["navigation"]["report"]["pending_action"]
                    .as_str()
                    .map(str::to_owned)
            });
        // Results may be large. Retain a bounded view; full SDK receipts remain
        // independently journalled by the resident Rust executor.
        if serde_json::to_vec(&checkpoint.result)?.len() > 32768 {
            checkpoint.result = json!({"truncated":true,"note":"Use a focused observation/query; original action IDs remain in the Brain journal"});
        }
        checkpoint.in_flight = false;
        brain
            .journal
            .set(&checkpoint_key, &serde_json::to_string(&checkpoint)?)?;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(
        json!({"language":"lua","success":true,"done":false,"slice_exhausted":true,"reloads":reloads,"rejected_update":rejected,"state":checkpoint.state,"last_result":checkpoint.result,"inventory_changed":initial_inventory!=brain.observed.player["inventory"],"inventory_delta":crate::feedback::inventory_delta(&initial_inventory,&brain.observed.player["inventory"]),"local_action_feedback":{"scope":"resident Lua SDK dispatches","from_sequence":0,"through_sequence":feedback.len(),"omitted_records":0,"requests":feedback},"resume":"Run the same Lua path to continue saved state; completed steps are not replayed"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acquisition_resume_lua_navigation_shares_closed_brain_validation() {
        let goal = r#"{kind='position',dimension='fixture:world',target={1,1,1},distance=0.5}"#;
        let source = |input: &str| {
            format!(
                "return {{version=1,step=function() return {{state={{}},command={{tool='navigate',input={input}}}}} end}}"
            )
        };
        let bare = step(&source(goal), &json!({}))
            .unwrap()
            .command
            .unwrap()
            .choice()
            .unwrap();
        let value: Value = serde_json::from_str(&bare.input).unwrap();
        assert_eq!(value["kind"], "position");
        assert!(value.get("until_inventory").is_none());
        let acquisition =
            format!("{{goal={goal},until_inventory={{item='fixture:resource',minimum=1}}}}");
        let selected = step(&source(&acquisition), &json!({}))
            .unwrap()
            .command
            .unwrap()
            .choice()
            .unwrap();
        assert_eq!(selected.tool, "navigate");
        let value: Value = serde_json::from_str(&selected.input).unwrap();
        assert_eq!(value["until_inventory"]["minimum"], 1);
        for input in [
            format!("{{goal={goal},until_inventory={{item='fixture:resource',minimum=0}}}}"),
            format!(
                "{{goal={goal},until_inventory={{item='fixture:resource',minimum=1,actor='foreign'}}}}"
            ),
            format!(
                "{{goal={goal},until_inventory={{item='fixture:resource',minimum=1}},console='forbidden'}}"
            ),
        ] {
            assert!(
                step(&source(&input), &json!({})).is_err(),
                "invalid acquisition cannot acquire tool authority"
            );
        }
    }
    #[test]
    fn scripts_are_bounded_pure_decisions_with_typed_sdk_validation() {
        let valid = "return {version=1, step=function(ctx) return {state={n=(ctx.state.n or 0)+1},command={tool='sdk',input={kind='look',yaw=0,pitch=0}}} end}";
        let answer = step(valid, &json!({"state":{"n":4}})).unwrap();
        assert_eq!(answer.state["n"], 5);
        for invalid in [
            "while true do end",
            "return {version=1,step=function() while true do end end}",
            "return {version=1,step=function() return {state={},command={tool='terminal',input='bad'}} end}",
            "return {version=1,step=function() return {state={},command={tool='sdk',input={kind='select_slot',slot=90}}} end}",
            "return {version=1,step=function() return {state={},done=true,command={tool='observe'}} end}",
            "return {version=1,step=function() return {state={huge=string.rep('x',20000000)},done=true} end}",
        ] {
            assert!(step(invalid, &json!({"state":{}})).is_err(), "{invalid}");
        }
        step("assert(os==nil and io==nil and package==nil and debug==nil and load==nil and pcall==nil and coroutine==nil); return {version=1,step=function() return {state={},done=true} end}", &json!({})).unwrap();
    }

    fn program(slot: u8) -> String {
        format!(
            "return {{version=1,step=function(ctx) local n=ctx.state.n or 0; if n>=2 then return {{state=ctx.state,done=true}} end; return {{state={{n=n+1}},command={{tool='sdk',input={{kind='select_slot',slot={slot}}}}}}} end}}"
        )
    }
    struct Server {
        path: std::path::PathBuf,
        next_source: String,
        requests: std::sync::Mutex<Vec<Value>>,
        statuses: AtomicU32,
        fail: bool,
        hold: std::sync::atomic::AtomicBool,
    }
    async fn observation() -> axum::Json<Value> {
        axum::Json(
            json!({"revision":"fixture","player":{"uuid":"actor","inventory":[]},"world":{},"menu":{}}),
        )
    }
    async fn act(
        axum::extract::State(server): axum::extract::State<Arc<Server>>,
        axum::Json(request): axum::Json<Value>,
    ) -> (axum::http::StatusCode, axum::Json<Value>) {
        let mut requests = server.requests.lock().unwrap();
        requests.push(request.clone());
        if requests.len() == 1 {
            fs::write(&server.path, &server.next_source).unwrap();
        }
        if server.fail {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"code":"internal","message":"fixture transport failure"})),
            );
        }
        (
            axum::http::StatusCode::OK,
            axum::Json(
                json!({"action_id":request["action_id"],"outcome":if requests.len()==1{"running"}else{"observed"},"adapter_version":"fixture","evidence":{},"error":null}),
            ),
        )
    }
    async fn status(
        axum::extract::State(server): axum::extract::State<Arc<Server>>,
        axum::extract::Path(id): axum::extract::Path<String>,
    ) -> axum::Json<Value> {
        server.statuses.fetch_add(1, Ordering::Relaxed);
        axum::Json(
            json!({"action_id":id,"outcome":if server.hold.load(Ordering::Relaxed){"running"}else{"observed"},"adapter_version":"fixture","evidence":{},"error":null}),
        )
    }
    async fn fixture(
        next_source: String,
        fail: bool,
    ) -> (
        tempfile::TempDir,
        Brain,
        Arc<Server>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (root, brain) =
            crate::admission_tests::fixture(&format!("http://{}/", listener.local_addr().unwrap()));
        let path = root.path().join("behavior.lua");
        fs::write(&path, program(1)).unwrap();
        let server = Arc::new(Server {
            path,
            next_source,
            requests: Default::default(),
            statuses: AtomicU32::new(0),
            fail,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let app = axum::Router::new()
            .route("/v1/bots/own/observations", axum::routing::get(observation))
            .route("/v1/bots/own/actions", axum::routing::post(act))
            .route("/v1/bots/own/actions/{id}", axum::routing::get(status))
            .with_state(server.clone());
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (root, brain, server, handle)
    }
    #[tokio::test]
    async fn hot_reload_preserves_state_pid_session_and_exactly_once_actions() {
        let (_root, mut brain, server, handle) = fixture(program(2), false).await;
        let run_id = brain.run_id.clone();
        let result = execute(&mut brain, "behavior.lua").await.unwrap();
        assert_eq!(result["done"], true);
        assert_eq!(result["reloads"], 2);
        assert_eq!(result["state"]["n"], 2);
        let requests = server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["action"]["slot"], 1);
        assert_eq!(requests[1]["action"]["slot"], 2);
        assert_ne!(requests[0]["action_id"], requests[1]["action_id"]);
        assert!(server.statuses.load(Ordering::Relaxed) >= 1);
        assert_eq!(brain.run_id, run_id);
        assert_eq!(
            brain.journal.get("pending_action").unwrap().as_deref(),
            Some("")
        );
        // A new slice reads durable state, rather than starting the script over.
        assert_eq!(
            execute(&mut brain, "behavior.lua").await.unwrap()["steps"],
            0
        );
        assert_eq!(server.requests.lock().unwrap().len(), 2);
        // This is the same client and character scope; no connect/disconnect
        // routes exist on the fixture server and no model endpoint is available.
        brain.client.observe().await.unwrap();
        handle.abort();
    }
    #[tokio::test]
    async fn rejected_updates_keep_last_good_and_lua_source_versions_restore() {
        let (root, mut brain, server, handle) = fixture("not valid Lua!".into(), false).await;
        let result = execute(&mut brain, "behavior.lua").await.unwrap();
        assert_eq!(result["done"], true);
        assert_eq!(result["reloads"], 1);
        assert!(result["rejected_update"].is_string());
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r["action"]["slot"] == 1)
        );
        let workspace = root.path().join("source");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("behavior.lua"), program(3)).unwrap();
        let version = crate::versions::snapshot(&workspace, root.path())
            .unwrap()
            .unwrap();
        fs::write(workspace.join("behavior.lua"), "bad source").unwrap();
        crate::versions::restore(root.path(), &workspace, &version).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("behavior.lua")).unwrap(),
            program(3)
        );
        handle.abort();
    }
    #[tokio::test]
    async fn failed_or_interrupted_dispatch_is_durable_and_never_replayed() {
        let (_root, mut brain, server, handle) = fixture(program(2), true).await;
        assert!(execute(&mut brain, "behavior.lua").await.is_err());
        assert!(
            execute(&mut brain, "behavior.lua")
                .await
                .unwrap_err()
                .to_string()
                .contains("no automatic replay")
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        assert!(
            !brain
                .journal
                .get("pending_action")
                .unwrap()
                .unwrap()
                .is_empty()
        );
        handle.abort();
    }

    #[tokio::test]
    async fn running_controls_block_activation_until_original_id_settles() {
        let (_root, mut brain, server, handle) = fixture(program(2), false).await;
        brain
            .server
            .capabilities
            .push("minecraft:finite_locomotion".into());
        fs::write(&server.path, "return {version=1,step=function(ctx) return {state={n=1},command={tool='sdk',input={kind='start_move',target={x=1,y=64,z=1},jump=false,max_ticks=5}}} end}").unwrap();
        server.hold.store(true, Ordering::Relaxed);
        brain.cfg.tool_timeout_secs = 1;
        let first = execute(&mut brain, "behavior.lua").await.unwrap();
        assert_eq!(first["waiting_for_action"], true);
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        let original = first["pending_action"].clone();
        assert!(original.is_string());
        server.hold.store(false, Ordering::Relaxed);
        brain.cfg.tool_timeout_secs = 5;
        let second = execute(&mut brain, "behavior.lua").await.unwrap();
        assert_eq!(second["done"], true);
        assert_eq!(second["reloads"], 1);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["action_id"], original);
        assert_eq!(requests[1]["action"]["slot"], 2);
        handle.abort();
    }
}
