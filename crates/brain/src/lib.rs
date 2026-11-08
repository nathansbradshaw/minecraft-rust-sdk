//! Container-local coding-agent harness. Trusted gameplay authority stays in SDK
//! gateway/server components. Model completion claims never certify game effects.
mod admission;
#[cfg(test)]
mod admission_tests;
pub mod body;
mod completion;
pub mod containers;
pub mod conversation;
mod diagnostics;
mod discovery;
pub mod feedback;
pub mod memory;
pub mod model;
pub mod navigation;
pub mod output;
mod ownership;
pub mod progress;
mod routing;
pub mod tools;
pub mod versions;
pub mod views;
pub mod worker;
mod workspace;
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
fn default_investigation_turns() -> u32 {
    4
}
fn default_terminal_output_budget() -> u32 {
    32
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_investigation_turns")]
    pub investigation_turns: u32,
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
    /// Optional success checks for this configured goal, not other player goals.
    #[serde(default)]
    pub completion_inventory: std::collections::BTreeMap<String, u64>,
    /// Client behavior preferences retained across replaceable gameplay goals.
    /// Empty by default; this does not change server authority or SDK policy.
    #[serde(default)]
    pub instructions: String,
    #[serde(default = "default_turns")]
    pub max_turns: u32,
    #[serde(default = "default_timeout")]
    pub tool_timeout_secs: u64,
    #[serde(default = "default_interval")]
    pub model_interval_secs: u64,
    #[serde(default)]
    pub exit_on_complete: bool,
    /// Optional replaceable decision executable; the resident engine owns play.
    #[serde(default)]
    pub decision_worker: Option<PathBuf>,
}
fn conversation_default() -> bool {
    true
}
fn model_context_contract(instructions: &str) -> String {
    digest(json!({"format":"sdk-shaped-observation-v1/focused-observation-v1/retrievable-memory-v1/container-inspection-memory-v1/bounded-history-context-v2/bounded-investigation-routing-v2","model_request_contract":model::REQUEST_CONTRACT,"sdk_reference":include_str!("sdk.md"),"command_schema":model::schema(),"system_prompt":include_str!("system.md"),"deployment_instructions":instructions}).to_string().as_bytes())
}
pub struct Journal {
    db: Connection,
}
impl Journal {
    /// Read-only diagnostics never migrate, chmod or write the supplied journal.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(3))?;
        Ok(Self { db })
    }
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        db.busy_timeout(Duration::from_secs(3))?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA max_page_count=16384;CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL);CREATE TABLE IF NOT EXISTS memory(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,text TEXT NOT NULL);CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE IF NOT EXISTS investigations(subject TEXT PRIMARY KEY,remaining INTEGER NOT NULL);")?;
        memory::initialize(&db)?;
        diagnostics::initialize(&db)?;
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
    fn context_contract(&self, contract: &str) -> Result<bool> {
        if self.get("context_contract")?.as_deref() == Some(contract) {
            return Ok(false);
        }
        self.set("inspection_streak", "0")?;
        self.set("investigation_remaining", "0")?;
        self.set("needs_investigation", "false")?;
        self.set("context_contract", contract)?;
        self.event("context_contract_changed",json!({"contract_sha256":contract,"inspection_investigation_reopened":true,"goal_and_model_allowance_changed":false}))?;
        Ok(true)
    }
    /// One durable revision per accepted task, including repeated goal text.
    pub fn select_goal(
        &self,
        goal: &str,
        kind: &str,
        data: Value,
        player_request: Option<&str>,
    ) -> Result<i64> {
        self.select_goal_checked(goal, kind, data, player_request, None)?
            .ok_or_else(|| "Unconditional goal selection failed".into())
    }
    fn select_goal_checked(
        &self,
        goal: &str,
        kind: &str,
        data: Value,
        player_request: Option<&str>,
        expected_revision: Option<&str>,
    ) -> Result<Option<i64>> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some(expected) = expected_revision {
            let current: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key='goal_revision'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if current.as_deref().unwrap_or_default() != expected {
                return Ok(None);
            }
            let current_goal: Option<String> = tx
                .query_row("SELECT value FROM meta WHERE key='goal'", [], |r| r.get(0))
                .optional()?;
            let completed: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key='completed_revision'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            // A repeated description of active work is continuation. A new
            // request for the same completed objective still creates a task.
            if current_goal.as_deref() == Some(goal) && completed.as_deref() != Some(expected) {
                return Ok(None);
            }
        }
        tx.execute(
            "INSERT INTO events(at,kind,data) VALUES(?1,?2,?3)",
            params![now(), kind, data.to_string()],
        )?;
        let id = tx.last_insert_rowid();
        for (key, value) in [
            ("goal", goal),
            ("goal_revision", &id.to_string()),
            ("completed_goal", ""),
            ("completed_revision", ""),
            ("inspection_streak", "0"),
            ("investigation_remaining", "0"),
            ("needs_investigation", "false"),
            ("develop_next", "false"),
            ("development_turns_remaining", "0"),
        ] {
            tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        }
        if let Some(request) = player_request {
            tx.execute("INSERT INTO meta(key,value) VALUES('player_request',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[request])?;
        }
        tx.commit()?;
        println!("{}", json!({"event":kind,"sequence":id,"data":data}));
        Ok(Some(id))
    }
    pub fn conversation_goal(
        &self,
        reply: &model::Conversation,
        expected_revision: &str,
        messages: &Value,
    ) -> Result<bool> {
        if reply.goal_action == model::GoalAction::Continue || reply.goal.trim().is_empty() {
            return Ok(false);
        }
        Ok(self
            .select_goal_checked(
                &reply.goal,
                "player_goal_selected",
                json!({"goal":reply.goal,"reason":reply.reason,"messages":messages}),
                Some(&messages.to_string()),
                Some(expected_revision),
            )?
            .is_some())
    }
    fn complete_goal(&self, goal: &str, revision: &str) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let get = |key: &str| -> rusqlite::Result<Option<String>> {
            tx.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                .optional()
        };
        if get("goal")?
            .as_deref()
            .is_some_and(|current| current != goal)
            || get("goal_revision")?.unwrap_or_default() != revision
        {
            return Ok(false);
        }
        for (key, value) in [("completed_goal", goal), ("completed_revision", revision)] {
            tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        }
        tx.commit()?;
        Ok(true)
    }
    fn goal_completed(&self, goal: &str) -> Result<bool> {
        Ok(self.get("completed_goal")?.as_deref() == Some(goal)
            && self.get("completed_revision")?.unwrap_or_default()
                == self.get("goal_revision")?.unwrap_or_default())
    }
    fn note_intention(&self, id: &str, independent: bool) -> Result<()> {
        if !independent || self.get("pending_action")?.unwrap_or_default().is_empty() {
            self.set("pending_action", id)?;
        }
        Ok(())
    }
    fn resolve_pending(&self, receipt: &minecraft_sdk::Receipt) -> Result<()> {
        if receipt.outcome.terminal()
            && self.get("pending_action")?.as_deref() == Some(&receipt.action_id)
        {
            self.set("pending_action", "")?;
        }
        Ok(())
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
        // The latest requested read is the tool's answer, not redundant receipt
        // history. Its records have already been paged/bounded at the tool edge.
        // Removing world here used to leave valid page counts with no records.
        let requested_page = (value["tool"] == "observe"
            && matches!(
                value["result"]["view"].as_str(),
                Some("blocks" | "entities" | "menu" | "inventory")
            )
            && value["result"]["page"].is_object())
            || (value["tool"] == "query" && value["result"]["view"] == "capability_query");
        if !requested_page {
            compact_history(&mut value);
        }
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
        if !["sdk", "write", "terminal", "run", "navigate"].contains(&choice.tool.as_str())
            || tool_failed(result)
        {
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
    fn experiment(
        &self,
        choice: &model::Choice,
        result: &Value,
        source: &str,
        goal_revision: &str,
    ) -> Result<bool> {
        if !["terminal", "run"].contains(&choice.tool.as_str())
            || (choice.tool != "run"
                && !choice.experiment
                && result["local_action_feedback"]["requests"]
                    .as_array()
                    .is_none_or(Vec::is_empty))
        {
            return Ok(false);
        }
        let signature = progress::experiment_signature(source, result);
        let novel = self.get("experiment_signature")?.as_deref() != Some(signature.as_str());
        let mut saved = json!({"command":choice.input,"summary":choice.summary,"source_sha256":source,"goal_revision":goal_revision,"signature":signature,"result":result,"authority":"container diagnostics, not benchmark/native certification"});
        compact_history(&mut saved);
        bound_value(&mut saved, 8192);
        self.set("last_experiment", &saved.to_string())?;
        self.set("experiment_signature", &signature)?;
        Ok(novel)
    }
    fn investigation_budget(
        &self,
        subject: &str,
        budget: u32,
        expected_revision: &str,
    ) -> Result<Option<(progress::Investigation, u32)>> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let current: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key='goal_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if current.as_deref().unwrap_or_default() != expected_revision {
            return Ok(None);
        }
        let known: Option<u32> = tx
            .query_row(
                "SELECT remaining FROM investigations WHERE subject=?1",
                [subject],
                |r| r.get(0),
            )
            .optional()?;
        let remaining = known.unwrap_or(budget).min(budget);
        tx.execute("INSERT INTO investigations(subject,remaining) VALUES(?1,?2) ON CONFLICT(subject) DO UPDATE SET remaining=excluded.remaining",params![subject,remaining])?;
        for (key, value) in [
            ("investigation_subject", subject.to_owned()),
            ("investigation_remaining", remaining.to_string()),
        ] {
            tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        }
        // Initialize the persisted finite session, not just its legacy boolean.
        // A continued/reopened process must never replenish an existing subject.
        if known.is_none() && remaining > 0 {
            for (key, value) in [
                ("develop_next", "true".to_owned()),
                (
                    "development_turns_remaining",
                    remaining.min(routing::DEVELOPMENT_TURNS).to_string(),
                ),
            ] {
                tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
            }
        }
        tx.commit()?;
        Ok(Some((
            progress::investigation(
                subject,
                if known.is_some() { subject } else { "" },
                remaining,
            ),
            remaining,
        )))
    }
    fn diagnosis_subject(&self, source: &str, revision: &str, environment: &str) -> Result<String> {
        self.admit_diagnostic_feedback(source, revision)?;
        let mut subject = json!({"source":source,"goal_revision":revision,"environment":environment,"experiment":self.get("experiment_signature")?,"context_contract":self.get("context_contract")?,"discovery_revision":self.get("discovery_revision")?,"decision_worker_revision":self.get("decision_worker_revision")?});
        // Preserve existing subject identities when there is no new feedback.
        if let Some(notice) = self.get("diagnostic_feedback_revision")? {
            subject["diagnostic_feedback_revision"] = json!(notice);
        }
        Ok(digest(subject.to_string().as_bytes()))
    }
    fn investigation_spent(&self, subject: &str, finished: bool) -> Result<()> {
        self.db.execute("UPDATE investigations SET remaining=CASE WHEN ?2 THEN 0 ELSE max(remaining-1,0) END WHERE subject=?1",params![subject,finished])?;
        let remaining: Option<u32> = self
            .db
            .query_row(
                "SELECT remaining FROM investigations WHERE subject=?1",
                [subject],
                |r| r.get(0),
            )
            .optional()?;
        self.set(
            "investigation_remaining",
            &remaining.unwrap_or_default().to_string(),
        )
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
    _actor_lock: fs::File,
}
impl Brain {
    pub async fn open(cfg: Config) -> Result<Self> {
        completion::validate_config(&cfg)?;
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
            cfg.instructions.len() <= 2048,
            "Deployment instructions exceed context budget",
        )?;
        ensure(
            (1..=64).contains(&cfg.max_inspections_without_execution),
            "Invalid inspection budget",
        )?;
        ensure(
            (1..=8).contains(&cfg.investigation_turns),
            "Invalid investigation budget",
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
        journal.context_contract(&digest(json!({"model":model_context_contract(&cfg.instructions),"configured_completion_goal":cfg.goal,"configured_completion_inventory":cfg.completion_inventory}).to_string().as_bytes()))?;
        let imported = journal.import_program_history(&cfg.state)?;
        if imported > 0 {
            journal.event(
                "historical_programs_indexed",
                json!({"files":imported,"status":"retained candidates only; no success granted"}),
            )?;
        }
        let mut random = [0; 16];
        use std::io::Read;
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let run_id = digest(&random)[..24].to_owned();
        let goal = journal.get("goal")?.unwrap_or_else(|| cfg.goal.clone());
        let mut profile: Profile = serde_json::from_slice(&private_read(&cfg.profile, 16384)?)?;
        let actor_lock = ownership::acquire(
            &profile,
            &std::env::temp_dir().join("minecraft-sdk-brain-actors"),
        )?;
        let mut client = Client::with_profile(&profile)?.without_local_arbiter();
        let info = client.verify_instance(&profile.instance_id).await?;
        let imported = journal
            .import_container_history(&profile.instance_id, &profile.credential.player_uuid)?;
        if imported > 0 {
            journal.event("container_history_indexed",json!({"inspections":imported,"native_actions_replayed":false,"authority":"historical local SDK receipts only"}))?;
        }
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
        if let Err(error) = completion::bind(&journal, &cfg, &goal, &observed) {
            let _ = client.disconnect().await;
            return Err(error);
        }
        let body = if cfg.body.enabled {
            if ![
                "minecraft:finite_locomotion",
                "minecraft:survival_facts",
                "minecraft:ranged_observation",
                "minecraft:pending_actions",
                "minecraft:session_material_preservation",
            ]
            .iter()
            .all(|c| info.capabilities.iter().any(|v| v == c))
            {
                let _ = client.disconnect().await;
                return Err("Default Survival brain requires finite controls, survival/ranged facts, pending-action inspection and session material preservation".into());
            }
            let finite_waypoint_axis = info.limits["finite_move_waypoint_blocks"]
                .as_f64()
                .filter(|v| (0.9..=4.2).contains(v))
                .unwrap_or(3.5);
            match body::Handle::start_with_waypoint_range(
                profile.clone(),
                cfg.body.clone(),
                &cfg.state,
                observed.clone(),
                finite_waypoint_axis,
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
            _actor_lock: actor_lock,
        })
    }
    pub async fn refresh(&mut self) -> Result<()> {
        if self.cfg.decision_worker.is_some() {
            let revision = worker::revision(&self.cfg)?;
            if self.journal.get("decision_worker_revision")?.as_deref() != Some(&revision) {
                self.journal.set("decision_worker_revision", &revision)?;
            }
        }
        self.observed = if let Some(h) = &self.body {
            h.shared.wait_fresh().await?
        } else {
            self.client.observe().await?
        };
        self.journal
            .remember_observation(&self.server.instance_id, &self.observed)?;
        if let Some(goal) = self.journal.get("goal")? {
            self.goal = goal;
        }
        Ok(())
    }
    pub async fn act(&mut self, action: minecraft_sdk::Action) -> Result<Value> {
        // A local schema/range failure never reached gateway admission. Record
        // durable pending identity only after this same SDK validator accepts it.
        action.validate()?;
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
        let mut previous = self.journal.get("pending_action")?.unwrap_or_default();
        let independent = matches!(
            action,
            minecraft_sdk::Action::Chat { .. } | minecraft_sdk::Action::Look { .. }
        );
        let cancelling = matches!(&action,minecraft_sdk::Action::CancelControl{action_id}|minecraft_sdk::Action::CancelUseItem{action_id} if action_id==&previous);
        if !previous.is_empty() && !independent && !cancelling {
            if let Err(error) = self.inspect(&previous).await {
                let recovered = match error.downcast_ref::<minecraft_sdk::ApiError>() {
                    Some(error) => admission::reconcile_invalid(
                        &self.journal,
                        &previous,
                        &self.server.instance_id,
                        self.observed.player["uuid"].as_str().unwrap_or_default(),
                        error,
                    )?,
                    None => None,
                };
                if recovered.is_none() {
                    return Err(error);
                }
            }
            previous = self.journal.get("pending_action")?.unwrap_or_default();
        }
        ensure(
            previous.is_empty() || independent || cancelling,
            "Inspect/reconcile the recorded pending action before submitting another mutation",
        )?;
        let sequence = self.journal.event(
            "action_intent",
            json!({"action":action,"before":projection(&self.observed)}),
        )?;
        let id = format!("brain-{}-{sequence}", self.run_id);
        self.journal.note_intention(&id, independent)?;
        let movement = matches!(
            action,
            minecraft_sdk::Action::StartMove { .. } | minecraft_sdk::Action::CancelControl { .. }
        );
        let mut receipt = self
            .client
            .act(&ActionRequest {
                action_id: id.clone(),
                action: action.clone(),
            })
            .await?;
        drop(_guard);
        if !movement && receipt.outcome == Outcome::Running {
            receipt = self
                .client
                .wait_for_action(&receipt.action_id, Duration::from_secs(10))
                .await?;
        }
        self.journal.resolve_pending(&receipt)?;
        if let Some(snapshot) = containers::inspection(
            &ActionRequest {
                action_id: id,
                action,
            },
            &receipt,
        ) {
            self.journal.remember_container(
                &self.server.instance_id,
                self.observed.player["uuid"].as_str().unwrap_or_default(),
                &snapshot,
            )?;
        }
        self.journal
            .event("action_receipt", serde_json::to_value(&receipt)?)?;
        self.observed = self.client.observe().await?;
        Ok(json!({"receipt":receipt,"after":projection(&self.observed)}))
    }
    pub async fn inspect(&mut self, id: &str) -> Result<minecraft_sdk::Receipt> {
        let receipt = self.client.action_status(id).await?;
        ensure(receipt.action_id == id, "Status receipt action ID mismatch")?;
        self.journal.resolve_pending(&receipt)?;
        self.journal.remember_container_receipt(
            &self.server.instance_id,
            self.observed.player["uuid"].as_str().unwrap_or_default(),
            &receipt,
        )?;
        self.journal
            .event("action_status", serde_json::to_value(&receipt)?)?;
        Ok(receipt)
    }
    pub async fn settle(&mut self, id: &str) -> Result<Value> {
        let receipt = match self.client.action_status(id).await {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Some(proof) = admission::reconcile_invalid(
                    &self.journal,
                    id,
                    &self.server.instance_id,
                    self.observed.player["uuid"].as_str().unwrap_or_default(),
                    &error,
                )? {
                    return Ok(proof);
                }
                return Err(error.into());
            }
        };
        let receipt = if receipt.outcome.terminal() || receipt.outcome == Outcome::Running {
            receipt
        } else {
            self.client.reconcile(id).await?
        };
        ensure(
            receipt.action_id == id,
            "Reconciliation receipt action ID mismatch",
        )?;
        self.journal.resolve_pending(&receipt)?;
        self.journal
            .event("explicit_reconciliation", serde_json::to_value(&receipt)?)?;
        Ok(serde_json::to_value(receipt)?)
    }
    pub async fn run(&mut self) -> Result<()> {
        let model = model::Model::new(&self.cfg.model_endpoint, &self.cfg.model_key)?;
        match worker::initialize(&self.cfg) {
            Ok(Some(selected)) => {
                self.journal
                    .event("decision_worker_initialized", selected)?;
            }
            Ok(None) => {}
            Err(error) => {
                self.journal.event("decision_worker_initialization_error",json!({"error":error.to_string(),"body_owner":"resident engine","next_action":"activate a valid worker; no unchanged retry"}))?;
            }
        }
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
            let mut investigation = Value::Null;
            let mut investigation_left = 0;
            let mut investigation_subject = String::new();
            let mut investigation_revision = String::new();
            if (inspected_goal == self.goal
                && inspected >= self.cfg.max_inspections_without_execution)
                || self.journal.get("needs_investigation")?.as_deref() == Some("true")
                || self
                    .journal
                    .get("investigation_remaining")?
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or_default()
                    > 0
            {
                loop {
                    let source = progress::workspace_fingerprint_excluding_state(
                        &self.cfg.workspace,
                        &self.cfg.state,
                    )?;
                    let revision = self.journal.get("goal_revision")?.unwrap_or_default();
                    let environment = progress::environment_signature(&self.observed);
                    let subject =
                        self.journal
                            .diagnosis_subject(&source, &revision, &environment)?;
                    let Some((state, remaining)) = self.journal.investigation_budget(
                        &subject,
                        self.cfg.investigation_turns,
                        &revision,
                    )?
                    else {
                        self.refresh().await?;
                        inspected = 0;
                        break;
                    };
                    match state {
                        progress::Investigation::Start => {
                            investigation_left = remaining;
                            self.journal.event("investigation_started",json!({"subject":subject,"turn_budget":investigation_left,"goal":self.goal,"source_sha256":source,"body_and_chat":"remain active","allowance_changed":false}))?;
                        }
                        progress::Investigation::Continue => investigation_left = remaining,
                        progress::Investigation::Exhausted => {
                            self.journal.event("investigation_exhausted",json!({"subject":subject,"goal":self.goal,"last_experiment":self.journal.get("last_experiment")?,"next_action":"Concrete defect: inspect the recorded diagnosis and missing boundary. No unchanged model polling; changed source, meaningful native facts or another task can reopen this subject.","body_and_chat":"remain active"}))?;
                            while self.journal.get("goal_revision")?.unwrap_or_default() == revision
                                && progress::workspace_fingerprint_excluding_state(
                                    &self.cfg.workspace,
                                    &self.cfg.state,
                                )? == source
                                && progress::environment_signature(&self.observed) == environment
                                && self.journal.diagnosis_subject(
                                    &source,
                                    &revision,
                                    &environment,
                                )? == subject
                            {
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                self.refresh().await?;
                            }
                            if self.journal.get("goal_revision")?.unwrap_or_default() != revision {
                                inspected = 0;
                                break;
                            }
                            continue;
                        }
                    }
                    investigation_subject = subject.clone();
                    investigation_revision = revision.clone();
                    investigation = json!({"mode":"investigation","remaining_turns":investigation_left,"subject":subject,"task":"Identify the earliest failing boundary from last_experiment and current facts. Gather a discriminating observation, repair code or run a changed bounded experiment. Do not reconstruct unchanged output repeatedly. State a concrete missing interface if client code cannot resolve it."});
                    break;
                }
            }
            self.refresh().await?;
            if !investigation_revision.is_empty()
                && self.journal.get("goal_revision")?.unwrap_or_default() != investigation_revision
            {
                investigation = Value::Null;
                investigation_left = 0;
                investigation_subject.clear();
                inspected = 0;
            }
            while self.cfg.await_chat_goal
                && ((self.goal == self.cfg.goal && self.journal.get("goal_revision")?.is_none())
                    || self.journal.goal_completed(&self.goal)?)
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
                self.refresh().await?;
            }
            let preferred = self.journal.take_development_turn()?;
            let develop = loop {
                if let Some(model) = self.journal.available_model(preferred, now())? {
                    break model;
                }
                // Both allocations are exhausted. Keep the physical and chat
                // lanes responsive without resubmitting denied requests.
                tokio::time::sleep(Duration::from_millis(250)).await;
                self.refresh().await?;
            };
            let pending = self.journal.get("pending_action")?.unwrap_or_default();
            let activity: Value = self
                .journal
                .get("activity")?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(json!([]));
            let mut context = json!({"server":self.server,"goal":self.goal,"player_request":self.journal.get("player_request")?,"turn":turn,"workspace":self.cfg.workspace,"sdk_reference":include_str!("sdk.md"),"working_notes":self.journal.get("working_notes")?,"inspection_calls_without_execution":self.journal.get("inspection_streak")?,"development_request":self.journal.get("development_request")?,"observation":projection(&self.observed),"observation_view":{"format":"sdk-shaped-compact-v1","complete":false,"blocks":"nearest observed non-air subset; omitted cells are unknown","entities":"hostiles and loot prioritized; omitted entities are unknown","inventory":"occupied slots only, original slot numbers","menus":"Use observe with view menu for paged current slot contents; view entities with match_id filters loot without another program."},"body":self.body.as_ref().map(|h|h.shared.view()),"pending_action":pending,"last_tool_result":self.journal.last_result()?,"recent":self.journal.recent()?,"completed_tool_activity":activity,"memory":self.journal.recall(&self.goal)?});
            context["investigation"] = investigation;
            context["deployment_instructions"] = json!(self.cfg.instructions);
            context["project_manifest"] = workspace::manifest_hint(&self.cfg.workspace);
            context["completion_requirements"] = completion::view(&self.cfg, &self.goal);
            context["decision_worker"] = json!({"enabled":self.cfg.decision_worker.is_some(),"protocol":1,"activate":"replace_worker with a compiled Rust executable path; current decision/tool settles first; body and native connection stay resident"});
            context["inspected_containers"] = self
                .journal
                .recalled_containers(&self.server.instance_id, &self.observed)?;
            context["model_availability"] = self.journal.model_availability(now())?;
            context["last_experiment"] = self
                .journal
                .get("last_experiment")?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(Value::Null);
            fit_context(&mut context)?;
            self.journal
                .set("last_model_context", &serde_json::to_string(&context)?)?;
            self.journal.event("model_role", json!({"model":if develop {"gpt-6.1-sol"} else {"gpt-6-luna"},"preferred_development":preferred,"quota_fallback":preferred != develop,"development_turns_remaining":self.journal.get("development_turns_remaining")?,"provider_allowances_changed":false}))?;
            let requested_goal = self.goal.clone();
            let requested_revision = self.journal.get("goal_revision")?.unwrap_or_default();
            let worker_revision = worker::revision(&self.cfg)?;
            let worker_cfg = self.cfg.clone();
            let worker_journal = if self.cfg.decision_worker.is_some() {
                Some(Journal::open(&self.cfg.state.join("brain.sqlite"))?)
            } else {
                None
            };
            let lanes = self.body.as_ref().map(|h| h.shared.model_lanes.clone());
            let future = async {
                let _permit = match lanes {
                    Some(lanes) => Some(lanes.acquire(develop).await?),
                    None => None,
                };
                if let Some(journal) = &worker_journal {
                    worker::choose(&worker_cfg, journal, &context, develop, preferred).await
                } else {
                    model.choose_for(&context, develop, preferred).await
                }
            };
            tokio::pin!(future);
            let answer = loop {
                tokio::select! {reply=&mut future=>break reply,_=tokio::time::sleep(Duration::from_secs(1))=>{match self.refresh().await{Ok(())=>{},Err(e)=>{self.journal.event("observation_error",json!({"error":e.to_string()}))?;}}}}
            };
            next = tokio::time::Instant::now() + Duration::from_secs(self.cfg.model_interval_secs);
            self.refresh().await?;
            // Allocation applies to the character even if chat replaced the
            // goal while this inference was pending.
            let daily_unavailable = match &answer {
                Err(error) => self
                    .journal
                    .remember_daily_reset(develop, error.as_ref(), now())?,
                Ok(_) => false,
            };
            if worker::revision(&self.cfg)? != worker_revision {
                self.journal.event("decision_worker_replaced",json!({"old_revision":worker_revision,"discarded_pending_output":true,"body_and_native_connection":"resident and unchanged"}))?;
                continue;
            }
            if self.goal != requested_goal
                || self.journal.get("goal_revision")?.unwrap_or_default() != requested_revision
            {
                self.journal.event("superseded_reasoning",json!({"requested_goal":requested_goal,"current_goal":self.goal,"discarded_result":true}))?;
                continue;
            }
            let (choice, usage) = match answer {
                Ok(v) => v,
                Err(e) => {
                    let retry_at = model::retry_at(e.as_ref());
                    self.journal
                        .event("model_error", json!({"error":e.to_string(),"turn":turn,"retry_at_utc":retry_at,"unchanged_failure_polling":false}))?;
                    if daily_unavailable {
                        self.journal.event("model_allocation_unavailable",json!({"development":develop,"retry_at_utc":retry_at,"other_model_work_allowed":true,"allowances_changed":false}))?;
                        continue;
                    }
                    // Preserve responsive gameplay/chat while the operator or a
                    // new player goal supplies a changed condition. Repeating a
                    // rejected inference request is not investigation.
                    while self.goal == requested_goal
                        && self.journal.get("goal_revision")?.unwrap_or_default()
                            == requested_revision
                        && worker::revision(&self.cfg)? == worker_revision
                    {
                        if retry_at.is_some_and(|at| now() >= at) {
                            break;
                        }
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
            if choice.tool == "develop"
                || (!develop
                    && (choice.tool == "write" || (choice.tool == "terminal" && choice.experiment)))
            {
                self.journal.request_development()?;
            } else if choice.tool == "complete" {
                self.journal.set("develop_next", "false")?;
            }
            let completing = choice.tool == "complete";
            let source_before = progress::workspace_fingerprint_excluding_state(
                &self.cfg.workspace,
                &self.cfg.state,
            )?;
            let program_version = if choice.tool == "run"
                || (choice.tool == "terminal" && choice.experiment)
            {
                match versions::snapshot(&self.cfg.workspace, &self.cfg.state) {
                    Ok(version) => version,
                    Err(error) => {
                        self.journal.event("skill_snapshot_error",json!({"error":error.to_string(),"execution":"still allowed; no source version claimed"}))?;
                        None
                    }
                }
            } else {
                None
            };
            let result = tools::execute(self, &choice).await;
            if let (Some(version), Ok(value)) = (&program_version, &result)
                && let Err(error) = self.journal.remember_program(
                    &self.cfg.state,
                    version,
                    &requested_goal,
                    &choice,
                    value,
                )
            {
                self.journal.event("skill_memory_error",json!({"error":error.to_string(),"version":version,"execution":"result retained independently"}))?;
            }
            let completed = completing
                && result.is_ok()
                && self
                    .journal
                    .complete_goal(&requested_goal, &requested_revision)?;
            if completing && result.is_ok() && !completed {
                self.journal.event("superseded_completion",json!({"goal":requested_goal,"goal_revision":requested_revision,"completed_new_work":false}))?;
            }
            let source_after = progress::workspace_fingerprint_excluding_state(
                &self.cfg.workspace,
                &self.cfg.state,
            )?;
            let goal_changed = self.goal != requested_goal
                || self.journal.get("goal_revision")?.unwrap_or_default() != requested_revision;
            let mut executed = result
                .as_ref()
                .is_ok_and(|r| progress::executed(&choice.tool, r, &source_before, &source_after));
            if let Ok(value) = &result {
                if self.journal.search_discovery(
                    &choice,
                    value,
                    &requested_revision,
                    &self.server.instance_id,
                    self.observed.player["dimension"]
                        .as_str()
                        .unwrap_or_default(),
                )? {
                    self.journal.event("search_discovery",json!({"goal_revision":requested_revision,"discovery_revision":self.journal.get("discovery_revision")?,"scope":"New accepted SDK search facts can reopen diagnosis; not gameplay progress or success","allowances_changed":false}))?;
                }
                executed |=
                    self.journal
                        .experiment(&choice, value, &source_after, &requested_revision)?;
            } else if choice.tool == "run" || (choice.tool == "terminal" && choice.experiment) {
                self.journal.experiment(&choice,&json!({"error":result.as_ref().err().map(ToString::to_string),"command_execution_uncertain":"Inspect existing action IDs before retrying"}),&source_after,&requested_revision)?;
            }
            if executed || goal_changed {
                if !investigation_subject.is_empty() {
                    self.journal
                        .investigation_spent(&investigation_subject, true)?;
                }
                self.journal.set("investigation_remaining", "0")?;
                self.journal.set("needs_investigation", "false")?;
            } else if investigation_left > 0 {
                self.journal
                    .investigation_spent(&investigation_subject, false)?;
            }
            let inspected =
                progress::next_inspection_count(inspected, inspected_goal == self.goal, executed);
            self.journal
                .set("inspection_streak", &inspected.to_string())?;
            self.journal.set("inspection_goal", &self.goal)?;
            match result {
                Ok(value) => {
                    self.journal.activity(&choice, &value)?;
                    if tool_failed(&value) && !goal_changed {
                        self.journal.set("needs_investigation", "true")?;
                        if !develop {
                            self.journal.request_development()?;
                        }
                        self.journal.set("development_request", "Investigate the last tool failure using its error and current observation. Do not repeat an unchanged rejected action; repair its precondition or the program first.")?;
                    }
                    self.journal
                        .event("tool_result", json!({"tool":choice.tool,"goal_revision":requested_revision,"result":value}))?;
                }
                Err(e) => {
                    if !goal_changed {
                        self.journal.set("needs_investigation", "true")?;
                    }
                    self.journal.event(
                        "tool_result",
                        json!({"tool":choice.tool,"goal_revision":requested_revision,"error":e.to_string()}),
                    )?;
                    if !develop {
                        self.journal.request_development()?;
                    }
                }
            }
            if completed && self.cfg.exit_on_complete {
                break;
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
    let blocks:Vec<_>=blocks.into_iter().filter(|b|b["block"]!="minecraft:air").take(80).map(|b|json!({"position":b["position"],"block":b["block"],"replaceable":b["replaceable"],"destroy_progress":b["destroy_progress"],"may_interact":b["may_interact"]})).collect();
    let inventory: Vec<_> = player["inventory"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["count"].as_u64().is_some_and(|c| c > 0))
        .map(|r| json!({"slot":r["slot"],"item":r["item"],"count":r["count"],"food":r["food"]}))
        .collect();
    let mut entities: Vec<_> = o.world["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let priority = |e: &Value| {
        if e["hostile"] == true {
            0
        } else if e["type"] == "minecraft:item" {
            1
        } else if e["projectile"] == true
            && e["velocity"]
                .as_array()
                .is_some_and(|v| v.iter().any(|x| x.as_f64().is_some_and(|x| x.abs() > 0.1)))
        {
            2
        } else if e["player"] == true || e["type"] == "minecraft:player" {
            3
        } else if e["projectile"] == true {
            5
        } else {
            4
        }
    };
    entities.sort_by(|a, b| {
        priority(a)
            .cmp(&priority(b))
            .then_with(|| distance(a).total_cmp(&distance(b)))
    });
    let entities: Vec<_> = entities.into_iter().take(24).map(views::entity).collect();
    json!({"revision":o.revision,"player":{"uuid":player["uuid"],"entity_id":player["entity_id"],"dimension":player["dimension"],"position":player["position"],"yaw":player["yaw"],"pitch":player["pitch"],"on_ground":player["on_ground"],"health":player["health"],"food":player["food"],"inventory":inventory,"selected":player["selected"]},"world":{"blocks":blocks,"entities":entities,"chat":o.world["chat"].as_array().map(|c|c.iter().skip(c.len().saturating_sub(24)).collect::<Vec<_>>()),"game_time":o.world["game_time"],"day_time":o.world["day_time"]},"menu":{"menu_id":o.menu["menu_id"],"state_id":o.menu["state_id"],"type":o.menu["type"],"carried":o.menu["carried"],"summary":containers::menu_summary(&o.menu)}})
}
/// Compact large strings/arrays without losing the structure of errors or player
/// facts. Array tails contain newer journal feedback and are retained first.
fn bound_value(value: &mut Value, budget: usize) {
    for _ in 0..20 {
        if value.to_string().len() <= budget {
            return;
        }
        let before = value.to_string().len();
        shrink_value(value);
        if value.to_string().len() == before {
            return;
        }
    }
}
fn fit_context(context: &mut Value) -> Result<()> {
    // Focused views have already been paged under a byte ceiling. Truncating
    // their records again would invalidate returned counts and slot bindings.
    let focused = context["last_tool_result"]["result"]["view"].is_string();
    let tracked: Vec<_> = [
        "server",
        "memory",
        "inspected_containers",
        "recent",
        "observation",
        "development_request",
        "completed_tool_activity",
        "last_tool_result",
        "last_experiment",
        "working_notes",
        "player_request",
    ]
    .into_iter()
    .map(|field| {
        (
            field,
            context[field].to_string().len(),
            context[field].as_array().map(Vec::len),
        )
    })
    .collect();
    for (field, budget) in [
        ("server", 2500),
        ("memory", 5000),
        ("inspected_containers", 5000),
        ("recent", 3000),
        ("observation", 6000),
        ("development_request", 1000),
        ("completed_tool_activity", 1500),
        ("last_tool_result", 9000),
        ("last_experiment", 4096),
        ("working_notes", 2000),
        ("player_request", 2000),
    ] {
        if !(focused && field == "last_tool_result") {
            bound_value(&mut context[field], budget);
        }
    }
    // Preserve the experiment/error channel under pressure. Older history,
    // redundant world detail and long source previews have complete file paths.
    for (field, budget) in [
        ("recent", 1000),
        ("completed_tool_activity", 512),
        ("observation", 3000),
        ("memory", 2500),
        ("inspected_containers", 2000),
        ("last_tool_result", if focused { 7000 } else { 5000 }),
        ("server", 1500),
        ("working_notes", 1000),
    ] {
        if serde_json::to_vec(context)?.len() <= 32768 {
            break;
        }
        if !(focused && field == "last_tool_result") {
            bound_value(&mut context[field], budget);
        }
    }
    // Recursive shrinking has a floor: short scalar fields, complete inventory
    // bindings and the final two records cannot shrink further. Two copies of
    // a normal experiment can therefore exceed their nominal history budgets.
    // Mark compaction honestly, then elide redundant history before discarding
    // any current observation, latest tool feedback or experiment/error channel.
    let mut projection = serde_json::Map::new();
    for (field, bytes, records) in &tracked {
        if context[field].to_string().len() != *bytes {
            projection.insert((*field).into(), json!({"complete":false,"view":"compacted","original_bytes":bytes,"original_records":records}));
        }
    }
    if !projection.is_empty() {
        context["context_projection"] = json!(projection);
    }
    for field in [
        "completed_tool_activity",
        "recent",
        "memory",
        "inspected_containers",
    ] {
        if serde_json::to_vec(context)?.len() <= 32768 {
            break;
        }
        if context[field].is_null() || context[field].as_array().is_some_and(Vec::is_empty) {
            continue;
        }
        let (_, bytes, records) = tracked
            .iter()
            .find(|(name, _, _)| *name == field)
            .ok_or("Context budget field missing")?;
        context[field] = json!({"omitted_for_context_budget":true,"original_bytes":bytes,"original_records":records,"retained_in":"persistent client journal and original tool output files"});
        projection.insert(field.into(), json!({"complete":false,"view":"elided_redundant_history","original_bytes":bytes,"original_records":records}));
        context["context_projection"] = json!(projection);
    }
    ensure(
        serde_json::to_vec(context)?.len() <= 32768,
        "Context budget exceeded",
    )
}

/// Offline replay of saved journal feedback over a previously accepted frame.
/// The previous observation is retained; this does not observe or reconnect a
/// character, call inference, replay actions or certify a gameplay outcome.
pub fn replay_context(journal: &Journal, context: &mut Value) -> Result<Value> {
    let goal = context["goal"]
        .as_str()
        .ok_or("Context goal missing")?
        .to_owned();
    context["last_tool_result"] = journal.last_result()?;
    context["recent"] = journal.recent()?;
    context["memory"] = journal.recall(&goal)?;
    for (field, key) in [
        ("completed_tool_activity", "activity"),
        ("last_experiment", "last_experiment"),
    ] {
        context[field] = journal
            .get(key)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
    }
    context["working_notes"] = json!(journal.get("working_notes")?);
    let sizes = |v: &Value| -> Value {
        v.as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), json!(v.to_string().len())))
                    .collect::<serde_json::Map<_, _>>()
            })
            .map(Value::Object)
            .unwrap_or(Value::Null)
    };
    let before_bytes = serde_json::to_vec(context)?.len();
    let before_fields = sizes(context);
    let error = fit_context(context).err().map(|e| e.to_string());
    Ok(
        json!({"scope":"offline saved-observation/journal replay; no SDK or model access","budget_bytes":32768,"fit":error.is_none(),"error":error,"before_bytes":before_bytes,"after_bytes":serde_json::to_vec(context)?.len(),"before_fields":before_fields,"after_fields":sizes(context)}),
    )
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
                } else if matches!(key.as_str(), "blocks" | "entities" | "inspected_containers")
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
        || v["local_action_feedback"]
            .get("error")
            .is_some_and(|e| !e.is_null())
        || v.get("error").is_some_and(|e| !e.is_null())
        || matches!(
            v["receipt"]["outcome"].as_str(),
            Some("rejected" | "uncertain" | "failed" | "expired")
        )
        || matches!(
            v["outcome"].as_str(),
            Some("rejected" | "uncertain" | "failed" | "expired")
        )
        || v["local_action_feedback"]["requests"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| {
                row["response"].get("error").is_some_and(|e| !e.is_null())
                    || matches!(
                        row["response"]["outcome"].as_str(),
                        Some("rejected" | "uncertain" | "failed" | "expired")
                    )
            })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_turn_read_failure_gets_one_persistent_feedback_opportunity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let j = Journal::open(&path).unwrap();
        let revision = j
            .select_goal("obtain wood", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        j.set("needs_investigation", "true").unwrap();
        let subject = j
            .diagnosis_subject("source", &revision, "same-world")
            .unwrap();
        j.investigation_budget(&subject, 1, &revision).unwrap();
        // This is the actual failure ordering: the last allowed diagnostic
        // returns new feedback after spending its final investigation turn.
        j.investigation_spent(&subject, false).unwrap();
        j.event("tool_result", json!({"tool":"query","error":"Capability response exceeds the model view budget; use an SDK program"})).unwrap();
        let repaired = j
            .diagnosis_subject("source", &revision, "same-world")
            .unwrap();
        assert_ne!(
            repaired, subject,
            "New feedback must reach a decision before the loop stalls"
        );
        assert_eq!(
            j.investigation_budget(&repaired, 1, &revision).unwrap(),
            Some((progress::Investigation::Start, 1))
        );
        assert!(
            j.last_result().unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("model view budget")
        );
        j.investigation_spent(&repaired, false).unwrap();
        // Repeating that error, changing the read tool, or reopening the
        // process cannot create another opportunity for the same experiment.
        j.event("tool_result", json!({"tool":"query","error":"Capability response exceeds the model view budget; use an SDK program"})).unwrap();
        assert_eq!(
            j.diagnosis_subject("source", &revision, "same-world")
                .unwrap(),
            repaired
        );
        j.event(
            "tool_result",
            json!({"tool":"read","error":"Another diagnostic failed"}),
        )
        .unwrap();
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert_eq!(
            j.diagnosis_subject("source", &revision, "same-world")
                .unwrap(),
            repaired
        );
        assert_eq!(
            j.investigation_budget(&repaired, 8, &revision).unwrap(),
            Some((progress::Investigation::Exhausted, 0))
        );
    }
    #[test]
    fn investigation_gets_a_finite_sol_session_without_restart_renewal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let j = Journal::open(&path).unwrap();
        let revision = j
            .select_goal("make progress", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        j.set("development_turns_remaining", "0").unwrap();
        assert_eq!(
            j.investigation_budget("stalled", 2, &revision).unwrap(),
            Some((progress::Investigation::Start, 2))
        );
        assert!(j.take_development_turn().unwrap());
        j.investigation_spent("stalled", false).unwrap();
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert_eq!(
            j.investigation_budget("stalled", 2, &revision).unwrap(),
            Some((progress::Investigation::Continue, 1))
        );
        assert!(j.take_development_turn().unwrap());
        assert!(!j.take_development_turn().unwrap());
        j.investigation_spent("stalled", false).unwrap();
        assert_eq!(
            j.investigation_budget("stalled", 8, &revision).unwrap(),
            Some((progress::Investigation::Exhausted, 0))
        );
        assert!(!j.take_development_turn().unwrap());
        // A genuinely new subject receives at most the normal four turns.
        j.investigation_budget("changed-facts", 8, &revision)
            .unwrap();
        j.set("sol_daily_reset", "200").unwrap();
        assert_eq!(j.available_model(true, 100).unwrap(), Some(false));
        for _ in 0..routing::DEVELOPMENT_TURNS {
            assert!(j.take_development_turn().unwrap());
        }
        assert!(!j.take_development_turn().unwrap());
        assert_eq!(j.get("sol_daily_reset").unwrap().as_deref(), Some("200"));
    }
    #[test]
    fn history_floors_do_not_stop_reasoning_or_corrupt_latest_evidence() {
        // Short fields reach shrink_value's floor even though their enclosing
        // record exceeds its nominal budget. No private journal data is used.
        let history: serde_json::Map<String, Value> = (0..50)
            .map(|n| {
                (
                    format!("field_{n}"),
                    json!("short retained fact ".repeat(20)),
                )
            })
            .collect();
        let page = json!({"view":"menu","complete":true,"menu_id":7,"state_id":11,"returned":1,"slots":[{"slot":8,"item":"custom:resource","count":3}]});
        let experiment = json!({"exit_code":101,"stderr":"compile error: missing method","inventory_delta":[{"item":"custom:resource","change":1,"current_count":3}],"local_action_feedback":{"requests":[{"request":{"action_id":"durable-failed-action"},"response":{"outcome":"rejected"}}]},"output_files":{"stderr":{"path":"retained-errors.txt"}}});
        let player = json!({"position":[1.25,64.0,3.5],"health":17,"inventory":[{"slot":8,"item":"custom:resource","count":3}]});
        let mut context = json!({"sdk_reference":include_str!("sdk.md"),"goal":"craft a tool","completed_tool_activity":[history.clone(),history.clone()],"recent":[history.clone(),history],"last_tool_result":{"result":page},"last_experiment":experiment,"observation":{"player":player}});
        assert!(context.to_string().len() > 32768);
        fit_context(&mut context).unwrap();
        assert!(context.to_string().len() <= 32768);
        assert_eq!(context["last_tool_result"]["result"], page);
        assert_eq!(context["last_experiment"], experiment);
        assert_eq!(context["observation"]["player"], player);
        assert_eq!(
            context["completed_tool_activity"]["omitted_for_context_budget"],
            true
        );
        assert_eq!(
            context["context_projection"]["completed_tool_activity"]["complete"],
            false
        );
        assert_eq!(context["goal"], "craft a tool");
    }
    #[test]
    fn diagnostic_journal_refuses_creation_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        assert!(Journal::open_read_only(&path).is_err());
        assert!(!path.exists());
        let journal = Journal::open(&path).unwrap();
        journal.event("fixture", json!({"id":"preserved"})).unwrap();
        drop(journal);
        let before = fs::read(&path).unwrap();
        let read_only = Journal::open_read_only(&path).unwrap();
        assert!(read_only.event("mutation", json!({})).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    #[test]
    fn context_pressure_preserves_complete_standing_instructions() {
        assert_ne!(
            model_context_contract("preserve builds"),
            model_context_contract("")
        );
        let instructions = "Retain this deployment preference across all player tasks. ".repeat(30);
        let mut context = json!({"goal":"new player task","deployment_instructions":instructions,"server":"s".repeat(3000),"observation":{"world":{"blocks":(0..100).map(|n|json!({"position":[n,64,0],"block":"minecraft:stone"})).collect::<Vec<_>>()},"player":{"position":[1.,64.,2.]}},"recent":["x".repeat(16000)],"sdk_reference":include_str!("sdk.md")});
        fit_context(&mut context).unwrap();
        assert_eq!(context["deployment_instructions"], instructions);
        assert_eq!(context["goal"], "new player task");
        assert!(context.to_string().len() <= 32768);
    }
    #[test]
    fn inspections_cannot_replace_the_last_experiment_or_reset_a_repeated_failure() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        let mut choice = model::Choice {
            tool: "terminal".into(),
            input: "cargo run --bin candidate".into(),
            path: String::new(),
            summary: "run candidate".into(),
            notes: String::new(),
            experiment: true,
        };
        let failure = json!({"success":true,"exit_code":0,"local_action_feedback":{"requests":[{"request":{"action_id":"old-id","action":{"kind":"look","yaw":1,"pitch":0}},"response":{"outcome":"rejected","error":{"code":"permission_denied","message":"fixture denied"}}}]},"output_files":{"stderr":{"path":"retained.log"}}});
        assert!(tool_failed(&failure));
        assert!(
            j.experiment(&choice, &failure, "source-one", "goal-one")
                .unwrap()
        );
        let saved = j.get("last_experiment").unwrap();
        choice.experiment = false;
        choice.input = "cat main.rs".into();
        assert!(
            !j.experiment(
                &choice,
                &json!({"success":true,"exit_code":0}),
                "source-one",
                "goal-one"
            )
            .unwrap()
        );
        assert_eq!(saved, j.get("last_experiment").unwrap());
        choice.experiment = true;
        let mut repeated = failure.clone();
        repeated["local_action_feedback"]["requests"][0]["request"]["action_id"] =
            json!("different-id");
        assert!(
            !j.experiment(&choice, &repeated, "source-one", "goal-one")
                .unwrap()
        );
        assert!(
            j.experiment(&choice, &repeated, "repaired-source", "goal-one")
                .unwrap()
        );
    }
    #[test]
    fn exhausted_investigation_survives_restart_reverting_source_and_budget_increase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite");
        let revision;
        {
            let j = Journal::open(&path).unwrap();
            revision = j
                .select_goal("collect material", "fixture", json!({}), None)
                .unwrap()
                .to_string();
            assert_eq!(
                j.investigation_budget("subject-a", 2, &revision).unwrap(),
                Some((progress::Investigation::Start, 2))
            );
            j.investigation_spent("subject-a", false).unwrap();
            j.investigation_spent("subject-a", false).unwrap();
            assert_eq!(
                j.investigation_budget("subject-b", 2, &revision).unwrap(),
                Some((progress::Investigation::Start, 2))
            );
        }
        let j = Journal::open(&path).unwrap();
        assert_eq!(
            j.investigation_budget("subject-a", 8, &revision).unwrap(),
            Some((progress::Investigation::Exhausted, 0))
        );
        let other = Journal::open(&path).unwrap();
        other
            .select_goal("find food", "fixture", json!({}), None)
            .unwrap();
        assert!(
            j.investigation_budget("stale-request", 4, &revision)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            j.get("needs_investigation").unwrap().as_deref(),
            Some("false")
        );
        assert_eq!(
            j.get("investigation_remaining").unwrap().as_deref(),
            Some("0")
        );
    }
    #[test]
    fn large_context_preserves_experiment_errors_and_sdk_coordinates() {
        let mut context = json!({"server":{"description":"x".repeat(30000)},"sdk_reference":"API docs".repeat(500),"goal":"goal","recent":vec![json!({"text":"x".repeat(10000)});12],"last_tool_result":{"result":{"content":"source".repeat(5000)}},"last_experiment":{"result":{"exit_code":101,"stderr":"compiler failure marker","local_action_feedback":{"requests":[]},"output_files":{"stderr":{"path":"complete-errors.txt"}}}},"observation":{"player":{"position":[12.25,64.0,-9.5],"inventory":[{"slot":7,"item":"custom:item","count":2}]},"world":{"blocks":[{"position":[12,64,-9],"block":"custom:resource"}]}}});
        fit_context(&mut context).unwrap();
        assert!(context.to_string().len() <= 32768);
        assert_eq!(context["last_experiment"]["result"]["exit_code"], 101);
        assert_eq!(
            context["last_experiment"]["result"]["stderr"],
            "compiler failure marker"
        );
        assert_eq!(
            context["observation"]["world"]["blocks"][0]["position"],
            json!([12, 64, -9])
        );
    }
    fn conversation(action: model::GoalAction, goal: &str) -> model::Conversation {
        model::Conversation {
            message: String::new(),
            goal_action: action,
            goal: goal.into(),
            reason: "fixture".into(),
        }
    }
    #[test]
    fn conversation_continuation_preserves_active_work_and_completed_state() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&t.path().join("brain.sqlite")).unwrap();
        let revision = j
            .select_goal(
                "collect wood",
                "fixture",
                json!({}),
                Some("original player request"),
            )
            .unwrap()
            .to_string();
        j.set("develop_next", "true").unwrap();
        j.set("inspection_streak", "7").unwrap();
        j.set("development_request", "inspect saved compiler error")
            .unwrap();
        let messages = json!([{"kind":"system","message":"peer joined the game"}]);
        // Defensive handling also covers a model filling goal while saying continue.
        for action in [model::GoalAction::Continue, model::GoalAction::Replace] {
            assert!(
                !j.conversation_goal(&conversation(action, "collect wood"), &revision, &messages)
                    .unwrap()
            );
            assert_eq!(
                j.get("goal_revision").unwrap().as_deref(),
                Some(revision.as_str())
            );
            assert_eq!(j.get("inspection_streak").unwrap().as_deref(), Some("7"));
            assert_eq!(j.get("develop_next").unwrap().as_deref(), Some("true"));
            assert_eq!(
                j.get("development_request").unwrap().as_deref(),
                Some("inspect saved compiler error")
            );
            assert_eq!(
                j.get("player_request").unwrap().as_deref(),
                Some("original player request")
            );
        }
        assert!(j.complete_goal("collect wood", &revision).unwrap());
        assert!(
            !j.conversation_goal(
                &conversation(model::GoalAction::Continue, "collect wood"),
                &revision,
                &messages
            )
            .unwrap()
        );
        assert!(j.goal_completed("collect wood").unwrap());
        let request = json!([{"kind":"disguised","message":"please collect another log"}]);
        assert!(
            j.conversation_goal(
                &conversation(model::GoalAction::Replace, "collect wood"),
                &revision,
                &request
            )
            .unwrap()
        );
        assert!(!j.goal_completed("collect wood").unwrap());
        assert_ne!(
            j.get("goal_revision").unwrap().as_deref(),
            Some(revision.as_str())
        );
        assert_eq!(j.get("player_request").unwrap(), Some(request.to_string()));
    }
    #[test]
    fn delayed_conversation_cannot_replace_newer_work_from_another_connection() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("brain.sqlite");
        let conversation_db = Journal::open(&path).unwrap();
        let old = conversation_db
            .select_goal("collect wood", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        let planner = Journal::open(&path).unwrap();
        let newer = planner
            .select_goal("find food", "fixture", json!({}), Some("newer request"))
            .unwrap()
            .to_string();
        assert!(
            !conversation_db
                .conversation_goal(
                    &conversation(model::GoalAction::Replace, "build shelter"),
                    &old,
                    &json!([])
                )
                .unwrap()
        );
        assert_eq!(planner.get("goal").unwrap().as_deref(), Some("find food"));
        assert_eq!(
            planner.get("goal_revision").unwrap().as_deref(),
            Some(newer.as_str())
        );
        assert_eq!(
            planner.get("player_request").unwrap().as_deref(),
            Some("newer request")
        );
    }
    #[test]
    fn conversation_requires_an_explicit_valid_goal_action() {
        let mut reply = json!({"message":"","goal":"collect wood","reason":"fixture"});
        assert!(serde_json::from_value::<model::Conversation>(reply.clone()).is_err());
        reply["goal_action"] = json!("continue");
        assert_eq!(
            serde_json::from_value::<model::Conversation>(reply.clone())
                .unwrap()
                .goal_action,
            model::GoalAction::Continue
        );
        reply["goal_action"] = json!("execute_host_command");
        assert!(serde_json::from_value::<model::Conversation>(reply).is_err());
    }
    #[test]
    fn completed_task_survives_restart_but_repeated_or_superseding_requests_are_new_work() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("brain.sqlite");
        let first;
        {
            let j = Journal::open(&path).unwrap();
            first = j
                .select_goal(
                    "collect wood",
                    "player_goal_selected",
                    json!({}),
                    Some("first chat"),
                )
                .unwrap()
                .to_string();
            assert!(j.complete_goal("collect wood", &first).unwrap());
        }
        let j = Journal::open(&path).unwrap();
        assert!(j.goal_completed("collect wood").unwrap());
        let repeated = j
            .select_goal(
                "collect wood",
                "player_goal_selected",
                json!({}),
                Some("second chat"),
            )
            .unwrap()
            .to_string();
        assert_ne!(first, repeated);
        assert!(!j.goal_completed("collect wood").unwrap());
        assert!(!j.complete_goal("collect wood", &first).unwrap());
        assert_eq!(
            j.get("player_request").unwrap().as_deref(),
            Some("second chat")
        );
        assert!(j.complete_goal("collect wood", &repeated).unwrap());
        j.select_goal(
            "craft tools",
            "player_goal_selected",
            json!({}),
            Some("third chat"),
        )
        .unwrap();
        assert!(!j.complete_goal("collect wood", &repeated).unwrap());
        assert!(!j.goal_completed("craft tools").unwrap());
        assert_eq!(j.get("goal").unwrap().as_deref(), Some("craft tools"));
    }
    #[test]
    fn pending_identity_survives_independent_failure_and_only_matching_resolution_clears_it() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&t.path().join("brain.sqlite")).unwrap();
        j.note_intention("movement", false).unwrap();
        j.note_intention("independent-chat", true).unwrap();
        assert_eq!(
            j.get("pending_action").unwrap().as_deref(),
            Some("movement")
        );
        let mut receipt = minecraft_sdk::Receipt {
            action_id: "independent-chat".into(),
            outcome: Outcome::Observed,
            adapter_version: "fixture".into(),
            evidence: json!({}),
            error: None,
        };
        j.resolve_pending(&receipt).unwrap();
        assert_eq!(
            j.get("pending_action").unwrap().as_deref(),
            Some("movement")
        );
        receipt.action_id = "movement".into();
        for outcome in [
            Outcome::Prepared,
            Outcome::Dispatched,
            Outcome::Running,
            Outcome::Uncertain,
        ] {
            receipt.outcome = outcome;
            j.resolve_pending(&receipt).unwrap();
            assert_eq!(
                j.get("pending_action").unwrap().as_deref(),
                Some("movement")
            );
        }
        receipt.outcome = Outcome::Observed;
        j.resolve_pending(&receipt).unwrap();
        assert_eq!(j.get("pending_action").unwrap().as_deref(), Some(""));
        j.note_intention("next-mutation", false).unwrap();
        j.resolve_pending(&receipt).unwrap();
        assert_eq!(
            j.get("pending_action").unwrap().as_deref(),
            Some("next-mutation")
        );
    }
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
            .map(|n| json!({"position":[n,64,0],"block":"fixture:block","description":"x".repeat(100)}))
            .collect();
        let entities: Vec<_> = (0..64)
            .map(|n| json!({"entity_id":n,"position":[n,64,1],"hostile":n==0}))
            .collect();
        let inventory: Vec<_> = (0..6)
            .map(|slot| json!({"slot":slot,"item":"fixture:item","count":slot+1}))
            .collect();
        let mut context = json!({"player":{"position":[12.25,64.0,-9.5],"velocity":[0.1,0.0,0.2],"health":17.0,"inventory":inventory},"world":{"blocks":blocks,"entities":entities},"collision":[0.,0.,0.,1.,1.,1.]});
        bound_value(&mut context, 2000);
        assert!(context.to_string().len() <= 2000);
        assert_eq!(context["player"]["position"], json!([12.25, 64.0, -9.5]));
        assert_eq!(context["player"]["velocity"], json!([0.1, 0.0, 0.2]));
        assert_eq!(context["collision"], json!([0., 0., 0., 1., 1., 1.]));
        assert_eq!(context["world"]["blocks"][0]["position"], json!([0, 64, 0]));
        assert_eq!(context["world"]["entities"][0]["entity_id"], 0);
        assert_eq!(context["world"]["entities"][0]["hostile"], true);
        assert_eq!(context["player"]["inventory"].as_array().unwrap().len(), 6);
        assert_eq!(context["player"]["inventory"][0]["count"], 1);
    }
    #[test]
    fn compact_observation_uses_actual_sdk_paths_and_preserves_runtime_bindings() {
        let full = Observation {
            revision: "runtime-revision".into(),
            player: json!({"position":[12.25,64.0,-9.5],"inventory":[{"slot":7,"item":"custom:material","count":2},{"slot":8,"item":"minecraft:air","count":0}]}),
            world: json!({"blocks":[{"position":[12,64,-9],"block":"custom:resource","may_interact":true},{"position":[13,64,-9],"block":"minecraft:air"}],"entities":[{"entity_id":812,"position":[13.5,64.0,-8.5],"type":"custom:creature"}],"chat":(0..30).map(|n|json!({"sequence":n})).collect::<Vec<_>>()}),
            menu: json!({"menu_id":11,"state_id":19,"type":"custom:menu","carried":{"item":"custom:material","count":1}}),
        };
        let compact = projection(&full);
        let parsed: Observation = serde_json::from_value(compact.clone()).unwrap();
        assert_eq!(
            parsed.world["blocks"][0]["position"],
            full.world["blocks"][0]["position"]
        );
        assert_eq!(parsed.world["blocks"][0]["block"], "custom:resource");
        assert!(parsed.world["blocks"][0].get("p").is_none());
        assert!(compact.get("blocks").is_none());
        assert_eq!(parsed.world["entities"][0]["entity_id"], 812);
        assert_eq!(parsed.player["position"], full.player["position"]);
        assert_eq!(parsed.player["inventory"][0]["slot"], 7);
        assert_eq!(parsed.menu["menu_id"], 11);
        assert_eq!(parsed.menu["state_id"], 19);
        assert_eq!(parsed.world["chat"][0]["sequence"], 6);
        assert_eq!(parsed.world["chat"][23]["sequence"], 29);
    }
    #[test]
    fn only_a_changed_context_contract_reopens_stalled_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        j.select_goal(
            "collect material",
            "fixture",
            json!({}),
            Some("original request"),
        )
        .unwrap();
        j.set("develop_next", "true").unwrap();
        j.set("pending_action", "never-replay").unwrap();
        assert!(j.context_contract("contract-one").unwrap());
        j.set("inspection_streak", "8").unwrap();
        assert!(!j.context_contract("contract-one").unwrap());
        assert_eq!(j.get("inspection_streak").unwrap().as_deref(), Some("8"));
        assert!(j.context_contract("contract-two").unwrap());
        assert_eq!(j.get("inspection_streak").unwrap().as_deref(), Some("0"));
        assert_eq!(j.get("goal").unwrap().as_deref(), Some("collect material"));
        assert_eq!(
            j.get("player_request").unwrap().as_deref(),
            Some("original request")
        );
        assert_eq!(
            j.get("pending_action").unwrap().as_deref(),
            Some("never-replay")
        );
        assert_eq!(j.get("develop_next").unwrap().as_deref(), Some("true"));
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
    fn latest_focused_pages_survive_journal_and_model_context() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&t.path().join("brain.sqlite")).unwrap();
        let mut observation = Observation {
            revision: "native-page".into(),
            player: json!({"uuid":"actor","dimension":"custom:world","position":[3,64,9],"inventory":[]}),
            world: json!({"blocks":[{"position":[4,64,9],"block":"custom:resource","state":"sample"}],"entities":[{"entity_id":73,"type":"minecraft:item","position":[4.5,64,9.5],"item":{"item":"custom:resource","count":2}}]}),
            menu: json!({"menu_id":7,"state_id":8,"slots":[{"slot":0,"inventory_slot":0,"player_inventory":false,"item":"custom:result","count":1}]}),
        };
        observation.world["blocks"] = json!((0..64).map(|n|json!({"position":[n,64,9],"block":format!("custom:{}{n}","resource".repeat(24)),"state":"properties".repeat(20),"may_interact":true,"collision":[[0,0,0,1,1,1]]})).collect::<Vec<_>>());
        observation.world["entities"] = json!((0..64).map(|n|json!({"entity_id":n,"type":"minecraft:item","position":[n,64,9],"item":{"item":format!("custom:{}{n}","resource".repeat(24)),"count":2}})).collect::<Vec<_>>());
        for view in ["blocks", "entities", "menu", "inventory"] {
            let page =
                views::focused(&observation, &json!({"view":view,"limit":32}).to_string()).unwrap();
            j.event("tool_result", json!({"tool":"observe","result":page}))
                .unwrap();
            let mut context = json!({"last_tool_result":j.last_result().unwrap(),"recent":vec!["old history ".repeat(1000);20],"observation":projection(&observation),"sdk_reference":include_str!("sdk.md"),"deployment_instructions":"rules ".repeat(300)});
            for field in [
                "server",
                "memory",
                "inspected_containers",
                "completed_tool_activity",
                "last_experiment",
                "working_notes",
                "player_request",
                "development_request",
            ] {
                context[field] = json!("historical context ".repeat(500));
            }
            fit_context(&mut context).unwrap();
            assert_eq!(
                context["last_tool_result"]["result"], page,
                "The requested {view} records and pagination must reach the model intact"
            );
        }
        let query = json!({"view":"capability_query","result":{"world":{"blocks":[{"position":[4,64,9],"block":"custom:resource"}]}}});
        j.event("tool_result", json!({"tool":"query","result":query}))
            .unwrap();
        assert_eq!(
            j.last_result().unwrap()["result"],
            query,
            "Requested capability data is also retained"
        );
        j.event("tool_result",json!({"tool":"sdk","result":{"receipt":{"evidence":{"before":{"world":{"blocks":["large"]}},"after":{"world":{"blocks":["large"]}}}},"after":{"world":{"blocks":["large"]}}}})).unwrap();
        assert!(
            !j.last_result().unwrap().to_string().contains("large"),
            "Full receipt history remains compacted"
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
