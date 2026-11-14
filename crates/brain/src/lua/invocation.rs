//! Caller arguments select reusable client work; they never authorize replay.
use super::*;
use rusqlite::{OptionalExtension, params};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    path: String,
    invocation_id: String,
    args: Value,
}
#[derive(Deserialize, Serialize)]
struct Active {
    key: String,
    path: String,
}
pub(super) struct Invocation {
    pub path: String,
    named: Option<Named>,
}
impl Invocation {
    pub fn parse(input: &str) -> Result<Option<Self>> {
        let named = if input.ends_with(".lua") {
            // A legacy filename may itself start with a brace. Named request
            // objects end with their JSON delimiter, not a Lua extension.
            None
        } else if input.trim_start().starts_with('{') {
            ensure(input.len() <= 10000, "Bounded Lua invocation required")?;
            let named: Named = serde_json::from_str(input)?;
            ensure(
                minecraft_sdk::valid_id(&named.invocation_id) && named.invocation_id.len() <= 96,
                "Opaque Lua invocation_id up to96bytes required",
            )?;
            ensure(
                serde_json::to_vec(&named.args)?.len() <= 8192,
                "Lua args exceed8192 bytes",
            )?;
            Some(named)
        } else {
            return Ok(None);
        };
        let path = named.as_ref().map_or(input, |n| n.path.as_str());
        ensure(
            !path.is_empty()
                && (named.is_none() || path.len() <= 512)
                && Path::new(path)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
                && Path::new(path).extension().is_some_and(|e| e == "lua"),
            "Use a relative workspace .lua path",
        )?;
        Ok(Some(Self {
            path: path.to_owned(),
            named,
        }))
    }
    pub fn args(&self) -> Option<&Value> {
        self.named.as_ref().map(|n| &n.args)
    }

    /// Keep the old plain-path key exactly; named IDs bind path/args separately.
    pub fn bind(&self, brain: &Brain, revision: &str, legacy_key: &str) -> Result<String> {
        let scope = digest(
            json!([
                "lua-invocation-v1",
                brain.cfg.workspace.canonicalize()?,
                brain.server.instance_id,
                brain.observed.player["uuid"],
                revision
            ])
            .to_string()
            .as_bytes(),
        );
        let active_key = format!("lua-active-invocation-{scope}");
        let key = self.named.as_ref().map_or_else(
            || legacy_key.to_owned(),
            |n| digest(json!([scope, n.invocation_id]).to_string().as_bytes()),
        );
        let tx = brain.journal.db.unchecked_transaction()?;
        let get = |name: &str| -> rusqlite::Result<Option<String>> {
            tx.query_row("SELECT value FROM meta WHERE key=?1", [name], |r| r.get(0))
                .optional()
        };
        let settled = |key: &str, previous_path: &str| -> Result<()> {
            if let Some(value) = get(&format!("lua-checkpoint-{key}"))? {
                let checkpoint: Checkpoint = serde_json::from_str(&value)?;
                if checkpoint.in_flight
                    || checkpoint
                        .pending_action
                        .as_ref()
                        .is_some_and(|id| !id.is_empty())
                {
                    ensure(
                        self.path != previous_path && program_pending(brain)?.is_none(),
                        "Original Lua invocation is interrupted or pending; resume/inspect its original IDs, no automatic replay or new invocation escape",
                    )?;
                    if checkpoint.mutation_possible
                        || checkpoint
                            .pending_action
                            .as_ref()
                            .is_some_and(|id| !id.is_empty())
                    {
                        let id = checkpoint.pending_action.as_deref().filter(|id|!id.is_empty()).ok_or("Interrupted Lua step lacks a settled original ID; another invocation cannot escape it")?;
                        let receipt: Option<String> = tx.query_row("SELECT data FROM events WHERE kind IN ('action_status','explicit_reconciliation') AND json_extract(data,'$.action_id')=?1 ORDER BY id DESC LIMIT 1",[id],|row|row.get(0)).optional()?;
                        let receipt: minecraft_sdk::Receipt = serde_json::from_str(&receipt.ok_or(
                        "Explicitly inspect/reconcile the original Lua action before a successor",
                    )?)?;
                        ensure(
                            receipt.action_id == id
                                && receipt.outcome.terminal()
                                && receipt.evidence["input_released"] == true,
                            "Original Lua action has not conclusively settled and released input",
                        )?;
                    }
                    // The ordinary executor still validates the pure candidate
                    // before any tool. Do not permit an alias/missing source.
                    source(&brain.cfg.workspace, &self.path)?;
                }
            }
            Ok(())
        };
        if let Some(previous) = get(&active_key)? {
            let previous: Active = serde_json::from_str(&previous)?;
            if previous.key != key {
                settled(&previous.key, &previous.path)?;
            }
        }
        if let Some(named) = &self.named {
            // Adding an ID must not evade an old failed plain-path checkpoint.
            settled(legacy_key, &self.path)?;
            let binding_key = format!("lua-invocation-binding-{key}");
            let binding = json!({"path":self.path,"args":named.args}).to_string();
            if let Some(previous) = get(&binding_key)? {
                ensure(
                    previous == binding,
                    "Lua invocation_id is already bound to different path/args",
                )?;
            } else {
                tx.execute(
                    "INSERT INTO meta(key,value) VALUES(?1,?2)",
                    params![binding_key, binding],
                )?;
            }
        }
        // Both forms participate in the handoff fence. Plain paths keep their
        // original checkpoint identity/state and gain no arguments.
        let active = serde_json::to_string(&Active {
            key: key.clone(),
            path: self.path.clone(),
        })?;
        tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![active_key,active])?;
        tx.commit()?;
        Ok(key)
    }
}

pub(crate) fn path(input: &str) -> Result<Option<String>> {
    Ok(Invocation::parse(input)?.map(|i| i.path))
}
