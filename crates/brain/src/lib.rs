//! Container-local coding-agent harness. Trusted gameplay authority stays in SDK
//! gateway/server components. Model completion claims never certify game effects.
pub mod body;
pub mod conversation;
pub mod model;
pub mod output;
pub mod progress;
pub mod tools;
use minecraft_sdk::{ActionRequest, Client, ErrorCode, Observation, Outcome, Profile};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn ensure(ok: bool, s: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(s.to_owned().into()) }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn private_read(path: &Path, bound: u64) -> Result<Vec<u8>> {
    let m = fs::symlink_metadata(path)?;
    ensure(
        m.is_file()
            && !m.file_type().is_symlink()
            && m.permissions().mode() & 0o077 == 0
            && m.len() <= bound,
        "Private bounded regular file required",
    )?;
    Ok(fs::read(path)?)
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("next");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    use std::io::Write;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&temp, path)?;
    fs::File::open(path.parent().ok_or("Parent required")?)?.sync_all()?;
    Ok(())
}
fn default_turns() -> u32 {
    64
}
fn default_timeout() -> u64 {
    240
}
fn default_interval() -> u64 {
    31
}
fn default_inspections() -> u32 {
    8
}
fn default_terminal_output_budget() -> u32 {
    32
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_terminal_output_budget")]
    pub terminal_output_budget_mib: u32,
    #[serde(default = "default_inspections")]
    pub max_inspections_without_execution: u32,
    #[serde(default = "conversation_default")]
    pub conversation: bool,
    #[serde(default)]
    pub await_chat_goal: bool,
    #[serde(default)]
    pub body: body::Config,
    pub profile: PathBuf,
    pub model_key: PathBuf,
    pub model_endpoint: String,
    pub workspace: PathBuf,
    #[serde(default)]
    pub cargo_target: Option<PathBuf>,
    pub state: PathBuf,
    pub goal: String,
    #[serde(default = "default_turns")]
    pub max_turns: u32,
    #[serde(default = "default_timeout")]
    pub tool_timeout_secs: u64,
    #[serde(default = "default_interval")]
    pub model_interval_secs: u64,
    #[serde(default)]
    pub exit_on_complete: bool,
}
fn conversation_default() -> bool {
    true
}
pub struct Journal {
    db: Connection,
}
impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        db.busy_timeout(Duration::from_secs(3))?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA max_page_count=16384;CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL);CREATE TABLE IF NOT EXISTS memory(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,text TEXT NOT NULL);CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
        Ok(Self { db })
    }
    pub fn event(&self, kind: &str, data: Value) -> Result<i64> {
        self.db.execute(
            "INSERT INTO events(at,kind,data) VALUES(?1,?2,?3)",
            params![now(), kind, data.to_string()],
        )?;
        let id = self.db.last_insert_rowid();
        println!("{}", json!({"event":kind,"sequence":id,"data":data}));
        Ok(id)
    }
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        Ok(())
    }
    pub fn remember(&self, kind: &str, text: &str) -> Result<()> {
        ensure(
            ["fact", "report", "hypothesis", "episode", "skill"].contains(&kind),
            "Memory kind required",
        )?;
        ensure(text.len() <= 16384, "Memory exceeds budget")?;
        self.db.execute(
            "INSERT INTO memory(at,kind,text) VALUES(?1,?2,?3)",
            params![now(), kind, text],
        )?;
        Ok(())
    }
    pub fn recall(&self, query: &str) -> Result<Value> {
        let mut q=self.db.prepare("SELECT id,at,kind,text FROM memory WHERE instr(lower(text),lower(?1))>0 ORDER BY id DESC LIMIT 12")?;
        let mut rows = q.query([query])?;
        let mut values = Vec::new();
        while let Some(r) = rows.next()? {
            values.push(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?}));
        }
        Ok(json!(values))
    }
    pub fn history(&self, query: &str) -> Result<Value> {
        // Include the receipt immediately following a matching action intent,
        // so a lookup for "look" can recover its real durable action ID.
        let mut q = self.db.prepare("SELECT id,at,kind,data FROM events WHERE instr(lower(data),lower(?1))>0 OR id IN (SELECT id+1 FROM events WHERE kind='action_intent' AND instr(lower(data),lower(?1))>0) ORDER BY id DESC LIMIT 12")?;
        let mut rows = q.query([query])?;
        let mut records = Vec::new();
        while let Some(r) = rows.next()? {
            let mut data: Value = serde_json::from_str(&r.get::<_, String>(3)?)?;
            compact_history(&mut data);
            bound_value(&mut data, 1500);
            records.push(json!({"sequence":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"event":r.get::<_,String>(2)?,"data":data}));
        }
        Ok(json!(records))
    }
    fn last_result(&self) -> Result<Value> {
        let data: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM events WHERE kind='tool_result' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let mut value: Value = data
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or(Value::Null);
        compact_history(&mut value);
        Ok(value)
    }
    fn recent(&self) -> Result<Value> {
        let mut q=self.db.prepare("SELECT kind,data FROM events WHERE kind IN ('choice','tool_result','model_error','model_completion_claim') ORDER BY id DESC LIMIT 6")?;
        let mut rows = q.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let mut data: Value = serde_json::from_str(&r.get::<_, String>(1)?)?;
            if let Some(result) = data.get_mut("result") {
                // Fresh state is supplied separately. Repeating complete observations
                // here previously evicted the very errors the model needed to repair.
                if let Some(object) = result.as_object_mut() {
                    object.remove("after");
                    object.remove("native_observation");
                    object.remove("blocks");
                    object.remove("entities");
                    object.remove("chat");
                    if let Some(receipt) = object.get_mut("receipt") {
                        let evidence = &receipt["evidence"];
                        let compact = json!({"native_effect_verified":evidence["native_effect_verified"],"dispatch_completed":evidence["dispatch_completed"]});
                        receipt["evidence"] = compact;
                    }
                }
                bound_value(result, 4096);
            }
            out.push(json!({"kind":r.get::<_,String>(0)?,"data":data}));
        }
        out.reverse();
        Ok(json!(out))
    }
    fn activity(&self, choice: &model::Choice, result: &Value) -> Result<()> {
        if !["sdk", "write", "terminal"].contains(&choice.tool.as_str()) || tool_failed(result) {
            return Ok(());
        }
        let mut records: Vec<Value> = self
            .get("activity")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut excerpt = result.clone();
        if choice.tool == "sdk" {
            excerpt = json!({"action_id":result["receipt"]["action_id"],"outcome":result["receipt"]["outcome"],"player":result["after"]["player"]});
        }
        bound_value(&mut excerpt, 1024);
        records.push(json!({"tool":choice.tool,"input":if choice.tool=="write" { String::from("source edit; path/hash in result") } else {choice.input.clone()},"result":excerpt}));
        if records.len() > 12 {
            records.drain(..records.len() - 12);
        }
        self.set("activity", &serde_json::to_string(&records)?)
    }
}
fn compact_history(value: &mut Value) {
    match value {
        Value::Object(o) => {
            for key in [
                "before",
                "after",
                "world",
                "blocks",
                "entities",
                "native_observation",
            ] {
                o.remove(key);
            }
            for v in o.values_mut() {
                compact_history(v);
            }
        }
        Value::Array(a) => {
            for v in a {
                compact_history(v);
            }
        }
        _ => {}
    }
}
pub struct Brain {
    pub server: minecraft_sdk::ServerInfo,
    pub body: Option<body::Handle>,
    pub cfg: Config,
    pub client: Client,
    pub journal: Journal,
    pub run_id: String,
    pub observed: Observation,
    pub goal: String,
    _lock: fs::File,
}
impl Brain {
    pub async fn open(cfg: Config) -> Result<Self> {
        ensure(
            Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists(),
            "Run the brain inside its isolated container",
        )?;
        ensure(
            unsafe { libc::geteuid() } != 0,
            "Non-root container required",
        )?;
        ensure(
            (1..=4096).contains(&cfg.max_turns)
                && (1..=600).contains(&cfg.tool_timeout_secs)
                && cfg.model_interval_secs <= 3600
                && !cfg.goal.trim().is_empty()
                && cfg.goal.len() <= 2048,
            "Invalid run budget/goal",
        )?;
        ensure(
            (1..=64).contains(&cfg.max_inspections_without_execution),
            "Invalid inspection budget",
        )?;
        ensure(
            (8..=256).contains(&cfg.terminal_output_budget_mib),
            "Invalid terminal output storage budget",
        )?;
        ensure(
            !cfg.await_chat_goal || (cfg.conversation && cfg.body.enabled),
            "Waiting for chat requires conversation and the responsive body",
        )?;
        fs::create_dir_all(&cfg.workspace)?;
        fs::create_dir_all(&cfg.state)?;
        fs::set_permissions(&cfg.state, fs::Permissions::from_mode(0o700))?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(cfg.state.join("controller.lock"))?;
        use std::os::fd::AsRawFd;
        ensure(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Another brain owns this state directory",
        )?;
        let journal = Journal::open(&cfg.state.join("brain.sqlite"))?;
        let mut random = [0; 16];
        use std::io::Read;
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let run_id = digest(&random)[..24].to_owned();
        let goal = journal.get("goal")?.unwrap_or_else(|| cfg.goal.clone());
        let mut profile: Profile = serde_json::from_slice(&private_read(&cfg.profile, 16384)?)?;
        let mut client = Client::with_profile(&profile)?.without_local_arbiter();
        let info = client.verify_instance(&profile.instance_id).await?;
        if let Err(e) = client.observe().await {
            if e.code == ErrorCode::SessionRequired {
                profile.session = None;
            } else {
                return Err(e.into());
            }
        }
        if profile.session.is_none() {
            let request = digest(
                format!(
                    "brain-connect:{}:{}",
                    profile.credential.bot_id,
                    cfg.state.display()
                )
                .as_bytes(),
            );
            profile.session = Some(client.connect(&request).await?);
            client = Client::with_profile(&profile)?.without_local_arbiter();
            if let Err(e) = private_write(&cfg.profile, &serde_json::to_vec(&profile)?) {
                let _ = client.disconnect().await;
                return Err(e);
            }
        }
        let observed = match client.observe().await {
            Ok(o) => o,
            Err(e) => {
                let _ = client.disconnect().await;
                return Err(e.into());
            }
        };
        let body = if cfg.body.enabled {
            if ![
                "minecraft:finite_locomotion",
                "minecraft:survival_facts",
                "minecraft:ranged_observation",
                "minecraft:pending_actions",
            ]
            .iter()
            .all(|c| info.capabilities.iter().any(|v| v == c))
            {
                let _ = client.disconnect().await;
                return Err("Default Survival brain requires finite controls, survival/ranged facts and pending-action inspection".into());
            }
            match body::Handle::start(
                profile.clone(),
                cfg.body.clone(),
                &cfg.state,
                observed.clone(),
            ) {
                Ok(h) => Some(h),
                Err(e) => {
                    let _ = client.disconnect().await;
                    return Err(e);
                }
            }
        } else {
            None
        };
        if let Err(e)=journal.event("brain_started",json!({"run":run_id,"uuid":profile.credential.player_uuid,"instance":profile.instance_id,"goal":goal,"execution":"container-local","initial":projection(&observed)})) {
            if let Some(body)=body {let _=body.stop().await;}
            let _=client.disconnect().await;return Err(e);
        }
        Ok(Self {
            server: info,
            body,
            cfg,
            client,
            journal,
            run_id,
            observed,
            goal,
            _lock: lock,
        })
    }
    pub async fn refresh(&mut self) -> Result<()> {
        self.observed = if let Some(h) = &self.body {
            h.shared.observe()?
        } else {
            self.client.observe().await?
        };
        if let Some(goal) = self.journal.get("goal")? {
            self.goal = goal;
        }
        Ok(())
    }
    pub async fn act(&mut self, action: minecraft_sdk::Action) -> Result<Value> {
        let safety = self.body.as_ref().map(|h| h.shared.clone());
        let _guard = if let Some(s) = &safety {
            ensure(
                body::supported(&action),
                "Use finite start_move controls so defensive movement remains preemptible",
            )?;
            let guard = s.mutation.clone().lock_owned().await;
            ensure(
                !s.urgent()
                    || matches!(
                        action,
                        minecraft_sdk::Action::Chat { .. } | minecraft_sdk::Action::Look { .. }
                    ),
                "Defensive control is active; observation, conversation and coding remain available",
            )?;
            Some(guard)
        } else {
            None
        };
        let previous = self.journal.get("pending_action")?.unwrap_or_default();
        let independent = matches!(
            action,
            minecraft_sdk::Action::Chat { .. } | minecraft_sdk::Action::Look { .. }
        );
        let cancelling = matches!(&action,minecraft_sdk::Action::CancelControl{action_id} if action_id==&previous);
        ensure(
            previous.is_empty() || independent || cancelling,
            "Inspect/reconcile the recorded pending action before submitting another mutation",
        )?;
        let sequence = self.journal.event(
            "action_intent",
            json!({"action":action,"before":projection(&self.observed)}),
        )?;
        let id = format!("brain-{}-{sequence}", self.run_id);
        self.journal.set("pending_action", &id)?;
        let receipt = self
            .client
            .act(&ActionRequest {
                action_id: id,
                action,
            })
            .await?;
        drop(_guard);
        if independent && !previous.is_empty() {
            self.journal.set("pending_action", &previous)?;
        } else if receipt.outcome.terminal() {
            self.journal.set("pending_action", "")?;
        }
        self.journal
            .event("action_receipt", serde_json::to_value(&receipt)?)?;
        self.observed = self.client.observe().await?;
        Ok(json!({"receipt":receipt,"after":projection(&self.observed)}))
    }
    pub async fn settle(&mut self, id: &str) -> Result<Value> {
        let receipt = self.client.action_status(id).await?;
        let receipt = if receipt.outcome.terminal() || receipt.outcome == Outcome::Running {
            receipt
        } else {
            self.client.reconcile(id).await?
        };
        if receipt.outcome.terminal() && self.journal.get("pending_action")?.as_deref() == Some(id)
        {
            self.journal.set("pending_action", "")?;
        }
        self.journal
            .event("explicit_reconciliation", serde_json::to_value(&receipt)?)?;
        Ok(serde_json::to_value(receipt)?)
    }
    pub async fn run(&mut self) -> Result<()> {
        let model = model::Model::new(&self.cfg.model_endpoint, &self.cfg.model_key)?;
        let mut next = tokio::time::Instant::now();
        for turn in 0..self.cfg.max_turns {
            while tokio::time::Instant::now() < next {
                tokio::time::sleep(Duration::from_secs(1)).await;
                self.refresh().await?;
            }
            self.refresh().await?;
            let mut inspected = self
                .journal
                .get("inspection_streak")?
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or_default();
            let inspected_goal = self.journal.get("inspection_goal")?.unwrap_or_default();
            if inspected_goal == self.goal
                && inspected >= self.cfg.max_inspections_without_execution
            {
                let source = progress::workspace_fingerprint(&self.cfg.workspace)?;
                self.journal.event("execution_stalled",json!({"inspection_calls":inspected,"goal":self.goal,"source_sha256":source,"thinking_lane":"paused pending changed source or goal","body_and_chat":"remain active","next_action":"Investigate the recorded context/tool boundary; no unchanged model polling"}))?;
                while self.goal == inspected_goal
                    && progress::workspace_fingerprint(&self.cfg.workspace)? == source
                {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    self.refresh().await?;
                }
                self.journal.set("inspection_streak", "0")?;
                inspected = 0;
            }
            while self.cfg.await_chat_goal
                && (self.goal == self.cfg.goal
                    || self.journal.get("completed_goal")?.as_deref() == Some(&self.goal))
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
                self.refresh().await?;
            }
            let pending = self.journal.get("pending_action")?.unwrap_or_default();
            let activity: Value = self
                .journal
                .get("activity")?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(json!([]));
            let mut context = json!({"server":self.server,"goal":self.goal,"player_request":self.journal.get("player_request")?,"turn":turn,"workspace":self.cfg.workspace,"sdk_reference":include_str!("sdk.md"),"working_notes":self.journal.get("working_notes")?,"inspection_calls_without_execution":self.journal.get("inspection_streak")?,"development_request":self.journal.get("development_request")?,"observation":projection(&self.observed),"body":self.body.as_ref().map(|h|h.shared.view()),"pending_action":pending,"last_tool_result":self.journal.last_result()?,"recent":self.journal.recent()?,"completed_tool_activity":activity,"memory":self.journal.recall(&self.goal)?});
            // Preserve the latest error and player state under pressure; never
            // silently delete all feedback to make a world snapshot fit.
            bound_value(&mut context["memory"], 1000);
            bound_value(&mut context["recent"], 3000);
            bound_value(&mut context["observation"], 6000);
            bound_value(&mut context["development_request"], 1000);
            bound_value(&mut context["completed_tool_activity"], 1500);
            bound_value(&mut context["last_tool_result"], 9000);
            bound_value(&mut context["working_notes"], 2000);
            bound_value(&mut context["player_request"], 2000);
            ensure(
                serde_json::to_vec(&context)?.len() <= 32768,
                "Context budget exceeded",
            )?;
            self.journal
                .set("last_model_context", &serde_json::to_string(&context)?)?;
            let develop = self.journal.get("develop_next")?.as_deref() == Some("true");
            let requested_goal = self.goal.clone();
            let lanes = self.body.as_ref().map(|h| h.shared.model_lanes.clone());
            let future = async {
                let _permit = match lanes {
                    Some(lanes) => Some(lanes.acquire(develop).await?),
                    None => None,
                };
                model.choose(&context, develop).await
            };
            tokio::pin!(future);
            let answer = loop {
                tokio::select! {reply=&mut future=>break reply,_=tokio::time::sleep(Duration::from_secs(1))=>{match self.refresh().await{Ok(())=>{},Err(e)=>{self.journal.event("observation_error",json!({"error":e.to_string()}))?;}}}}
            };
            next = tokio::time::Instant::now() + Duration::from_secs(self.cfg.model_interval_secs);
            self.refresh().await?;
            if self.goal != requested_goal {
                self.journal.event("superseded_reasoning",json!({"requested_goal":requested_goal,"current_goal":self.goal,"discarded_result":true}))?;
                continue;
            }
            let (choice, usage) = match answer {
                Ok(v) => v,
                Err(e) => {
                    self.journal
                        .event("model_error", json!({"error":e.to_string(),"turn":turn}))?;
                    // Preserve responsive gameplay/chat while the operator or a
                    // new player goal supplies a changed condition. Repeating a
                    // rejected inference request is not investigation.
                    while self.goal == requested_goal {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        self.refresh().await?;
                    }
                    continue;
                }
            };
            self.journal
                .event("choice", json!({"choice":choice,"usage":usage,"turn":turn}))?;
            if !choice.notes.is_empty() {
                self.journal.set("working_notes", &choice.notes)?;
            }
            if ["develop", "write", "terminal"].contains(&choice.tool.as_str()) {
                self.journal.set("develop_next", "true")?;
            } else if choice.tool == "complete" {
                self.journal.set("develop_next", "false")?;
            }
            let completed = choice.tool == "complete";
            let source_before = progress::workspace_fingerprint_excluding_state(
                &self.cfg.workspace,
                &self.cfg.state,
            )?;
            let result = tools::execute(self, &choice).await;
            let source_after = progress::workspace_fingerprint_excluding_state(
                &self.cfg.workspace,
                &self.cfg.state,
            )?;
            let executed = result
                .as_ref()
                .is_ok_and(|r| progress::executed(&choice.tool, r, &source_before, &source_after));
            let inspected =
                progress::next_inspection_count(inspected, inspected_goal == self.goal, executed);
            self.journal
                .set("inspection_streak", &inspected.to_string())?;
            self.journal.set("inspection_goal", &self.goal)?;
            match result {
                Ok(value) => {
                    self.journal.activity(&choice, &value)?;
                    if tool_failed(&value) {
                        self.journal.set("develop_next", "true")?;
                        self.journal.set("development_request", "Investigate the last tool failure using its error and current observation. Do not repeat an unchanged rejected action; repair its precondition or the program first.")?;
                    }
                    self.journal
                        .event("tool_result", json!({"tool":choice.tool,"result":value}))?;
                }
                Err(e) => {
                    self.journal.event(
                        "tool_result",
                        json!({"tool":choice.tool,"error":e.to_string()}),
                    )?;
                    self.journal.set("develop_next", "true")?;
                }
            }
            if completed && self.cfg.exit_on_complete {
                break;
            }
            if completed {
                self.journal.set("completed_goal", &self.goal)?;
            }
        }
        self.journal.event("brain_run_budget_finished",json!({"run":self.run_id,"goal":self.goal,"completion_authority":"external_native_evidence"}))?;
        Ok(())
    }
}
pub fn projection(o: &Observation) -> Value {
    let player = &o.player;
    let p = player["position"].as_array();
    let mut blocks: Vec<_> = o.world["blocks"].as_array().into_iter().flatten().collect();
    let distance = |b: &Value| -> f64 {
        (0..3)
            .map(|i| {
                let a = b["position"][i].as_f64().unwrap_or(1e9);
                let p = p
                    .and_then(|p| p.get(i))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.);
                (a - p).powi(2)
            })
            .sum()
    };
    blocks.sort_by(|a, b| distance(a).total_cmp(&distance(b)));
    let blocks:Vec<_>=blocks.into_iter().filter(|b|b["block"]!="minecraft:air").take(80).map(|b|json!({"p":b["position"],"block":b["block"],"replaceable":b["replaceable"],"destroy_progress":b["destroy_progress"],"may_interact":b["may_interact"]})).collect();
    let inventory: Vec<_> = player["inventory"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["count"].as_u64().is_some_and(|c| c > 0))
        .map(|r| json!({"slot":r["slot"],"item":r["item"],"count":r["count"],"food":r["food"]}))
        .collect();
    let entities: Vec<_> = o.world["entities"].as_array().into_iter().flatten().take(24).map(|e| json!({"uuid":e["uuid"],"entity_id":e["entity_id"],"type":e["type"],"position":e["position"],"velocity":e["velocity"],"visible":e["visible"],"hostile":e["hostile"],"ranged_attack":e["ranged_attack"],"main_hand":e["main_hand"],"off_hand":e["off_hand"],"projectile":e["projectile"],"creeper":e["creeper"],"health":e["health"],"item":e["item"],"name":e["name"]})).collect();
    json!({"revision":o.revision,"player":{"uuid":player["uuid"],"entity_id":player["entity_id"],"dimension":player["dimension"],"position":player["position"],"yaw":player["yaw"],"pitch":player["pitch"],"on_ground":player["on_ground"],"health":player["health"],"food":player["food"],"inventory":inventory,"selected":player["selected"]},"blocks":blocks,"entities":entities,"chat":o.world["chat"].as_array().map(|c|c.iter().rev().take(24).collect::<Vec<_>>()),"game_time":o.world["game_time"],"day_time":o.world["day_time"],"menu":{"menu_id":o.menu["menu_id"],"state_id":o.menu["state_id"],"type":o.menu["type"],"carried":o.menu["carried"]}})
}
/// Compact large strings/arrays without losing the structure of errors or player
/// facts. Array tails contain newer journal feedback and are retained first.
fn bound_value(value: &mut Value, budget: usize) {
    for _ in 0..20 {
        if value.to_string().len() <= budget {
            return;
        }
        shrink_value(value);
    }
}
fn shrink_value(value: &mut Value) {
    match value {
        Value::String(s) if s.len() > 512 => {
            let mut end = s.len() / 2;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push_str(" [truncated]");
        }
        Value::Array(a) if a.len() <= 6 && a.iter().all(Value::is_number) => {}
        Value::Array(a) => {
            if a.len() > 2 {
                a.drain(..a.len() / 2);
            }
            for v in a {
                shrink_value(v);
            }
        }
        Value::Object(o) => {
            for (key, v) in o.iter_mut() {
                if key == "inventory"
                    && let Some(a) = v.as_array_mut()
                {
                    for row in a {
                        shrink_value(row);
                    }
                } else if matches!(key.as_str(), "blocks" | "entities")
                    && let Some(a) = v.as_array_mut()
                {
                    // These snapshots prioritize nearest blocks and visible
                    // threats first, unlike chronological journal lists.
                    if a.len() > 2 {
                        a.truncate(a.len().div_ceil(2));
                    }
                    for row in a {
                        shrink_value(row);
                    }
                } else {
                    shrink_value(v);
                }
            }
        }
        _ => {}
    }
}
fn tool_failed(v: &Value) -> bool {
    v["success"] == false
        || v.get("error").is_some_and(|e| !e.is_null())
        || matches!(
            v["receipt"]["outcome"].as_str(),
            Some("rejected" | "uncertain" | "failed" | "expired")
        )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labelled_memory_and_pending_actions_survive_restart() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("brain.sqlite");
        {
            let j = Journal::open(&path).unwrap();
            j.remember("report", "A player says there is wood north")
                .unwrap();
            j.set("pending_action", "never-replay-this").unwrap();
        }
        let j = Journal::open(&path).unwrap();
        assert_eq!(
            j.get("pending_action").unwrap().as_deref(),
            Some("never-replay-this")
        );
        assert_eq!(j.recall("wood").unwrap()[0]["kind"], "report");
        assert!(j.remember("verified_success_from_model", "fake").is_err());
    }
    #[test]
    fn compact_context_keeps_complete_coordinates_and_nearby_threats() {
        let blocks: Vec<_> = (0..128)
            .map(|n| json!({"p":[n,64,0],"block":"fixture:block","description":"x".repeat(100)}))
            .collect();
        let entities: Vec<_> = (0..64)
            .map(|n| json!({"entity_id":n,"position":[n,64,1],"hostile":n==0}))
            .collect();
        let inventory: Vec<_> = (0..6)
            .map(|slot| json!({"slot":slot,"item":"fixture:item","count":slot+1}))
            .collect();
        let mut context = json!({"player":{"position":[12.25,64.0,-9.5],"velocity":[0.1,0.0,0.2],"health":17.0,"inventory":inventory},"blocks":blocks,"entities":entities,"collision":[0.,0.,0.,1.,1.,1.]});
        bound_value(&mut context, 2000);
        assert!(context.to_string().len() <= 2000);
        assert_eq!(context["player"]["position"], json!([12.25, 64.0, -9.5]));
        assert_eq!(context["player"]["velocity"], json!([0.1, 0.0, 0.2]));
        assert_eq!(context["collision"], json!([0., 0., 0., 1., 1., 1.]));
        assert_eq!(context["blocks"][0]["p"], json!([0, 64, 0]));
        assert_eq!(context["entities"][0]["entity_id"], 0);
        assert_eq!(context["entities"][0]["hostile"], true);
        assert_eq!(context["player"]["inventory"].as_array().unwrap().len(), 6);
        assert_eq!(context["player"]["inventory"][0]["count"], 1);
    }
    #[test]
    fn large_observations_cannot_erase_rejection_feedback() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&t.path().join("brain.sqlite")).unwrap();
        let result = json!({"receipt":{"outcome":"rejected","error":{"message":"Character is dead; explicitly respawn before acting"}},"after":{"entities":vec!["x".repeat(3000);128]}});
        assert!(tool_failed(&result));
        j.event("tool_result", json!({"tool":"sdk","result":result}))
            .unwrap();
        let recent = j.recent().unwrap();
        assert!(recent.to_string().len() < 4096);
        assert!(recent.to_string().contains("explicitly respawn"));
        assert!(!tool_failed(&json!({"receipt":{"outcome":"observed"}})));
        assert!(tool_failed(&json!({"success":false,"exit_code":101})));
    }
    #[test]
    fn history_recovers_older_native_receipts_by_action_kind() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&t.path().join("brain.sqlite")).unwrap();
        j.event(
            "action_intent",
            json!({"action":{"kind":"look","yaw":15},"before":{"world":"large"}}),
        )
        .unwrap();
        j.event(
            "action_receipt",
            json!({"action_id":"durable-native-id","outcome":"observed"}),
        )
        .unwrap();
        for n in 0..20 {
            j.event("choice", json!({"summary":format!("later inspection {n}")}))
                .unwrap();
        }
        assert!(
            !j.recent()
                .unwrap()
                .to_string()
                .contains("durable-native-id")
        );
        assert!(
            j.history("look")
                .unwrap()
                .to_string()
                .contains("durable-native-id")
        );
    }
    #[test]
    fn latest_source_output_is_preserved_independently_of_recent_summaries() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&t.path().join("brain.sqlite")).unwrap();
        let text = format!("{}\nfn last_line_is_needed() {{}}", "x".repeat(7500));
        j.event(
            "tool_result",
            json!({"tool":"terminal","result":{"stdout":text,"exit_code":0,"success":true}}),
        )
        .unwrap();
        assert!(
            j.last_result()
                .unwrap()
                .to_string()
                .contains("last_line_is_needed")
        );
    }
}
