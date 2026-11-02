use minecraft_sdk_core::*;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::path::Path;

fn internal(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, "Journal operation failed")
}
fn conflict(message: &str) -> ApiError {
    ApiError::new(ErrorCode::Conflict, message)
}
pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(internal)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn key_hash(instance: &str, bot: &str, key: &str) -> String {
    let digest = Sha256::digest(format!("minecraft-sdk-key-v1\0{instance}\0{bot}\0{key}"));
    format!("{digest:x}")
}
#[derive(Debug, Clone)]
pub struct Actor {
    pub bot_id: String,
    pub uuid: String,
    pub name: String,
}
pub struct Store {
    db: Connection,
    pub instance_id: String,
    _lock: std::fs::File,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
            if let Some(parent) = path.parent() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(internal)?;
                let meta = std::fs::symlink_metadata(parent).map_err(internal)?;
                if !meta.is_dir()
                    || meta.permissions().mode() & 0o077 != 0
                    || meta.uid() != unsafe { libc::geteuid() }
                {
                    return Err(ApiError::new(
                        ErrorCode::InvalidRequest,
                        "Journal directory must be private and owned by the gateway user",
                    ));
                }
            }
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .map_err(internal)?;
            let meta = file.metadata().map_err(internal)?;
            if !meta.is_file()
                || meta.permissions().mode() & 0o077 != 0
                || meta.uid() != unsafe { libc::geteuid() }
            {
                return Err(ApiError::new(
                    ErrorCode::InvalidRequest,
                    "Journal must be a private regular file owned by the gateway user",
                ));
            }
        }
        use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path.with_extension("lock"))
            .map_err(internal)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Another gateway owns this journal",
            ));
        }
        let db = Connection::open(path).map_err(internal)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(internal)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS bots(id TEXT PRIMARY KEY, label TEXT NOT NULL COLLATE NOCASE UNIQUE,
              uuid TEXT NOT NULL UNIQUE,name TEXT NOT NULL COLLATE NOCASE UNIQUE,key_hash TEXT NOT NULL,revoked INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS reserved(name TEXT PRIMARY KEY COLLATE NOCASE);
            CREATE TABLE IF NOT EXISTS sessions(bot TEXT PRIMARY KEY REFERENCES bots(id),id TEXT NOT NULL,request_id TEXT NOT NULL,adapter TEXT NOT NULL,state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS actions(bot TEXT NOT NULL REFERENCES bots(id),id TEXT NOT NULL,intent TEXT NOT NULL,receipt TEXT NOT NULL,unresolved INTEGER NOT NULL,PRIMARY KEY(bot,id));
            CREATE TABLE IF NOT EXISTS legacy_bindings(bot TEXT PRIMARY KEY,path TEXT NOT NULL,identity TEXT NOT NULL,rows INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS legacy_overrides(bot TEXT NOT NULL,id TEXT NOT NULL,receipt TEXT NOT NULL,PRIMARY KEY(bot,id));
            INSERT OR IGNORE INTO legacy_overrides SELECT bot,id,receipt FROM actions WHERE intent LIKE 'legacy:%' AND json_extract(receipt,'$.outcome')='reconciled';
            DELETE FROM actions WHERE intent LIKE 'legacy:%' AND bot IN(SELECT bot FROM legacy_bindings);").map_err(internal)?;
        let existing: Option<String> = db
            .query_row("SELECT value FROM metadata WHERE key='instance'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(internal)?;
        let instance_id = match existing {
            Some(s) => s,
            None => {
                let id = random_id()?;
                db.execute("INSERT INTO metadata VALUES('instance',?1)", [&id])
                    .map_err(internal)?;
                id
            }
        };
        let mut store = Self {
            db,
            instance_id,
            _lock: lock,
        };
        // Previous process could have dispatched any prepared action. Never retry it.
        let pending: Vec<(String, String, String)> = {
            let mut q = store
                .db
                .prepare("SELECT bot,id,receipt FROM actions WHERE unresolved<>0")
                .map_err(internal)?;
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(internal)?
                .collect::<std::result::Result<_, _>>()
                .map_err(internal)?
        };
        for (bot, id, json) in pending {
            let mut receipt: Receipt = serde_json::from_str(&json).map_err(internal)?;
            if receipt.outcome == Outcome::Uncertain {
                continue;
            }
            receipt.outcome = Outcome::Uncertain;
            receipt.error = Some(ApiError::new(
                ErrorCode::UncertainOutcome,
                "Gateway restarted; reconcile without replay",
            ));
            store.save_receipt(&bot, &id, &receipt)?;
        }
        store
            .db
            .execute("DELETE FROM sessions", [])
            .map_err(internal)?;
        Ok(store)
    }
    pub fn reserve(&self, name: &str) -> Result<()> {
        self.db
            .execute("INSERT OR IGNORE INTO reserved VALUES(?1)", [name])
            .map_err(internal)?;
        Ok(())
    }
    /// A deployment storage limit, independent of registry/player counts. FULL
    /// rejects an intent before dispatch, or leaves its existing intent uncertain.
    pub fn set_budget_mib(&self, mib: u32) -> Result<()> {
        if !(1..=65536).contains(&mib) {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Journal budget must be 1–65536 MiB",
            ));
        }
        let size: i64 = self
            .db
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .map_err(internal)?;
        let maximum = i64::from(mib) * 1024 * 1024 / size;
        let actual: i64 = self
            .db
            .query_row(&format!("PRAGMA max_page_count={maximum}"), [], |r| {
                r.get(0)
            })
            .map_err(internal)?;
        if actual > maximum {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Existing journal exceeds the configured storage budget",
            ));
        }
        self.db
            .execute_batch("PRAGMA wal_autocheckpoint=256; PRAGMA journal_size_limit=4194304;")
            .map_err(internal)?;
        Ok(())
    }
    pub fn register(&mut self, registration: &Registration) -> Result<Credential> {
        let label = &registration.name;
        if !valid_id(label) || label.len() > 32 {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Bot name must be 1–32 ASCII letters, digits, hyphens or underscores",
            ));
        }
        if self
            .db
            .query_row("SELECT 1 FROM reserved WHERE name=?1", [label], |_| Ok(()))
            .optional()
            .map_err(internal)?
            .is_some()
        {
            return Err(conflict("Reserved bot identity"));
        }
        let id = random_id()?;
        let key = random_id()?;
        let name = format!(".S_{}", &id[..12]);
        let mut u = random_id()?;
        u.replace_range(12..13, "4");
        u.replace_range(16..17, "8");
        let uuid = format!(
            "{}-{}-{}-{}-{}",
            &u[..8],
            &u[8..12],
            &u[12..16],
            &u[16..20],
            &u[20..32]
        );
        let hash = key_hash(&self.instance_id, &id, &key);
        self.db.execute("INSERT INTO bots(id,label,uuid,name,key_hash) VALUES(?1,?2,?3,?4,?5)",params![id,label,uuid,name,hash]).map_err(|e| {
            if matches!(e,rusqlite::Error::SqliteFailure(ref error,_) if error.code == rusqlite::ErrorCode::ConstraintViolation) { conflict("Bot name already registered") } else { internal(e) }
        })?;
        Ok(Credential {
            bot_id: id,
            player_uuid: uuid,
            key,
        })
    }
    pub fn authenticate(&self, bot: &str, key: &str) -> Result<Actor> {
        if !valid_id(bot) || key.len() != 64 {
            return Err(ApiError::new(
                ErrorCode::Unauthorized,
                "Invalid bot credential",
            ));
        }
        let actor = self
            .db
            .query_row(
                "SELECT uuid,name FROM bots WHERE id=?1 AND key_hash=?2 AND revoked=0",
                params![bot, key_hash(&self.instance_id, bot, key)],
                |r| {
                    Ok(Actor {
                        bot_id: bot.into(),
                        uuid: r.get(0)?,
                        name: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(internal)?;
        actor.ok_or_else(|| ApiError::new(ErrorCode::Unauthorized, "Invalid bot credential"))
    }
    pub fn rotate(&mut self, actor: &Actor) -> Result<Credential> {
        if self.has_session(&actor.bot_id)? {
            return Err(conflict("Disconnect before rotating the key"));
        }
        let key = random_id()?;
        self.db
            .execute(
                "UPDATE bots SET key_hash=?2 WHERE id=?1",
                params![
                    actor.bot_id,
                    key_hash(&self.instance_id, &actor.bot_id, &key)
                ],
            )
            .map_err(internal)?;
        Ok(Credential {
            bot_id: actor.bot_id.clone(),
            player_uuid: actor.uuid.clone(),
            key,
        })
    }
    pub fn revoke(&mut self, bot: &str) -> Result<()> {
        self.db
            .execute("UPDATE bots SET revoked=1 WHERE id=?1", [bot])
            .map_err(internal)?;
        // Native disconnect must be performed through the running gateway, not a second DB opener.
        Ok(())
    }
    pub fn operator_actor(&self, bot: &str) -> Result<Actor> {
        self.db
            .query_row("SELECT uuid,name FROM bots WHERE id=?1", [bot], |r| {
                Ok(Actor {
                    bot_id: bot.into(),
                    uuid: r.get(0)?,
                    name: r.get(1)?,
                })
            })
            .optional()
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "Unknown bot"))
    }
    pub fn operator_summary(&self) -> Result<serde_json::Value> {
        let mut query=self.db.prepare("SELECT b.id,b.label,b.uuid,b.name,coalesce(l.rows,0),EXISTS(SELECT 1 FROM sessions s WHERE s.bot=b.id) FROM bots b LEFT JOIN legacy_bindings l ON l.bot=b.id ORDER BY b.label").map_err(internal)?;
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, bool>(5)?,
                ))
            })
            .map_err(internal)?;
        let mut result = Vec::new();
        for row in rows {
            let (id, label, uuid, name, count, connected) = row.map_err(internal)?;
            let pending = if let Some(legacy) = self.legacy(&id)? {
                legacy
                    .query_row(
                        "SELECT id FROM actions WHERE result IS NULL LIMIT 1",
                        [],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(internal)?
            } else {
                None
            };
            result.push(serde_json::json!({"bot_id":id,"label":label,"player_uuid":uuid,"player_name":name,"legacy_actions":count,"connected":connected,"legacy_pending":pending}));
        }
        Ok(serde_json::json!(result))
    }
    pub fn import_legacy(
        &mut self,
        label: &str,
        uuid: &str,
        name: &str,
        journal: &Path,
    ) -> Result<Credential> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if !valid_id(label)
            || label.len() > 32
            || uuid.len() != 36
            || !uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
            || ![8, 13, 18, 23].iter().all(|i| uuid.as_bytes()[*i] == b'-')
            || !name.starts_with(".AI_")
            || name.len() > 16
            || !name[4..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Invalid legacy character binding",
            ));
        }
        let meta = std::fs::symlink_metadata(journal).map_err(internal)?;
        if !meta.is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { libc::geteuid() }
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Legacy journal must be private and owned by the gateway user",
            ));
        }
        let legacy =
            Connection::open_with_flags(journal, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(internal)?;
        let binding: String = legacy
            .query_row("SELECT value FROM meta WHERE key='identity'", [], |r| {
                r.get(0)
            })
            .map_err(internal)?;
        if !binding.ends_with(&format!(":{uuid}")) {
            return Err(conflict(
                "Legacy journal identity does not match the requested UUID",
            ));
        }
        let id = random_id()?;
        let key = random_id()?;
        let hash = key_hash(&self.instance_id, &id, &key);
        let tx = self.db.transaction().map_err(internal)?;
        tx.execute(
            "INSERT INTO bots(id,label,uuid,name,key_hash) VALUES(?1,?2,?3,?4,?5)",
            params![id, label, uuid, name, hash],
        )
        .map_err(internal)?;
        tx.execute("CREATE TABLE IF NOT EXISTS legacy_bindings(bot TEXT PRIMARY KEY,path TEXT NOT NULL,identity TEXT NOT NULL,rows INTEGER NOT NULL)",[]).map_err(internal)?;
        // Keep authoritative histories in place. An indexed lookup fences old
        // IDs and pending work without copying gigabytes of historical receipts.
        let count: i64 = legacy
            .query_row("SELECT count(*) FROM actions", [], |r| r.get(0))
            .map_err(internal)?;
        tx.execute(
            "INSERT INTO legacy_bindings VALUES(?1,?2,?3,?4)",
            params![
                id,
                journal.to_str().ok_or_else(|| ApiError::new(
                    ErrorCode::InvalidRequest,
                    "Legacy journal path must be UTF-8"
                ))?,
                binding,
                count
            ],
        )
        .map_err(internal)?;
        tx.execute("INSERT OR IGNORE INTO reserved VALUES(?1)", [label])
            .map_err(internal)?;
        tx.commit().map_err(internal)?;
        Ok(Credential {
            bot_id: id,
            player_uuid: uuid.into(),
            key,
        })
    }
    pub fn recover(&mut self, bot: &str) -> Result<Credential> {
        let actor = self.operator_actor(bot)?;
        if self.has_session(bot)? {
            return Err(conflict("Disconnect the character before key recovery"));
        }
        let credential = self.rotate(&actor)?;
        self.db
            .execute("UPDATE bots SET revoked=0 WHERE id=?1", [bot])
            .map_err(internal)?;
        Ok(credential)
    }
    pub fn has_session(&self, bot: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT 1 FROM sessions WHERE bot=?1", [bot], |_| Ok(()))
            .optional()
            .map_err(internal)?
            .is_some())
    }
    pub fn connect(
        &mut self,
        actor: &Actor,
        request: &SessionRequest,
        adapter: &str,
    ) -> Result<(Session, bool)> {
        if !valid_id(&request.request_id) {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Invalid connection request ID",
            ));
        }
        let old: Option<(String, String, String, String)> = self
            .db
            .query_row(
                "SELECT id,request_id,adapter,state FROM sessions WHERE bot=?1",
                [&actor.bot_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(internal)?;
        if let Some((id, request_id, adapter, state)) = old {
            if request_id == request.request_id && state == "connected" {
                return Ok((
                    Session {
                        session_id: id,
                        adapter_version: adapter,
                    },
                    false,
                ));
            }
            return Err(conflict(
                "Character already has a controller or pending connection",
            ));
        }
        let id = random_id()?;
        self.db
            .execute(
                "INSERT INTO sessions VALUES(?1,?2,?3,?4,'connecting')",
                params![actor.bot_id, id, request.request_id, adapter],
            )
            .map_err(internal)?;
        Ok((
            Session {
                session_id: id,
                adapter_version: adapter.into(),
            },
            true,
        ))
    }
    pub fn connected(&self, bot: &str) -> Result<()> {
        self.db
            .execute("UPDATE sessions SET state='connected' WHERE bot=?1", [bot])
            .map_err(internal)?;
        Ok(())
    }
    pub fn disconnect(&self, bot: &str) -> Result<()> {
        self.interrupt_running(bot)?;
        self.db
            .execute("DELETE FROM sessions WHERE bot=?1", [bot])
            .map_err(internal)?;
        Ok(())
    }
    pub fn running(&self, bot: &str) -> Result<Vec<String>> {
        let mut q = self
            .db
            .prepare("SELECT id FROM actions WHERE bot=?1 AND unresolved=2")
            .map_err(internal)?;
        q.query_map([bot], |r| r.get(0))
            .map_err(internal)?
            .collect::<std::result::Result<_, _>>()
            .map_err(internal)
    }
    fn interrupt_running(&self, bot: &str) -> Result<()> {
        for id in self.running(bot)? {
            if let Some(mut r) = self.receipt(bot, &id)? {
                r.outcome = Outcome::Uncertain;
                r.error = Some(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Controller connection ended; inspect/reconcile without replay",
                ));
                self.db
                    .execute(
                        "UPDATE actions SET receipt=?3,unresolved=1 WHERE bot=?1 AND id=?2",
                        params![bot, id, serde_json::to_string(&r).map_err(internal)?],
                    )
                    .map_err(internal)?;
            }
        }
        Ok(())
    }
    pub fn session(&self, bot: &str, session: &str) -> Result<String> {
        self.db
            .query_row(
                "SELECT adapter FROM sessions WHERE bot=?1 AND id=?2 AND state='connected'",
                params![bot, session],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::SessionRequired,
                    "A connected controller session is required",
                )
            })
    }
    pub fn receipt(&self, bot: &str, id: &str) -> Result<Option<Receipt>> {
        let json: Option<String> = self
            .db
            .query_row(
                "SELECT receipt FROM actions WHERE bot=?1 AND id=?2",
                params![bot, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        if let Some(json) = json {
            return serde_json::from_str(&json).map(Some).map_err(internal);
        }
        let override_json: Option<String> = self
            .db
            .query_row(
                "SELECT receipt FROM legacy_overrides WHERE bot=?1 AND id=?2",
                params![bot, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        if let Some(json) = override_json {
            return serde_json::from_str(&json).map(Some).map_err(internal);
        }
        self.legacy_receipt(bot, id)
    }
    fn legacy(&self, bot: &str) -> Result<Option<Connection>> {
        let binding: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT path,identity FROM legacy_bindings WHERE bot=?1",
                [bot],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((path, expected)) = binding else {
            return Ok(None);
        };
        let legacy = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(internal)?;
        let current: String = legacy
            .query_row("SELECT value FROM meta WHERE key='identity'", [], |r| {
                r.get(0)
            })
            .map_err(internal)?;
        if current != expected {
            return Err(conflict("Authoritative legacy journal identity changed"));
        }
        Ok(Some(legacy))
    }
    fn legacy_receipt(&self, bot: &str, id: &str) -> Result<Option<Receipt>> {
        let Some(legacy) = self.legacy(bot)? else {
            return Ok(None);
        };
        let row: Option<(String, bool)> = legacy
            .query_row(
                "SELECT digest,result IS NULL FROM actions WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        Ok(row.map(|(digest,pending)|Receipt{action_id:id.into(),outcome:if pending{Outcome::Uncertain}else{Outcome::Imported},adapter_version:"legacy-import-v1".into(),evidence:serde_json::json!({"imported":true,"original_intent_digest":digest,"historical_effect_verified":false}),error:if pending{Some(ApiError::new(ErrorCode::UncertainOutcome,"Imported unfinished action; reconcile without replay"))}else{None}}))
    }
    pub fn prepare(
        &mut self,
        bot: &str,
        request: &ActionRequest,
        adapter: &str,
    ) -> Result<(Receipt, bool)> {
        if !valid_id(&request.action_id) {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Invalid action ID",
            ));
        }
        request.action.validate()?;
        if self.legacy_receipt(bot, &request.action_id)?.is_some() {
            return Err(conflict("Historical action IDs remain permanently fenced"));
        }
        if let Some(legacy) = self.legacy(bot)? {
            let pending: Option<String> = legacy
                .query_row(
                    "SELECT id FROM actions WHERE result IS NULL LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            if let Some(id) = pending
                && self
                    .receipt(bot, &id)?
                    .is_none_or(|r| r.outcome != Outcome::Reconciled)
            {
                return Err(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Reconcile the imported unfinished action before new mutations",
                ));
            }
        }
        let intent = serde_json::to_string(&request.action).map_err(internal)?;
        let tx = self.db.transaction().map_err(internal)?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT intent,receipt FROM actions WHERE bot=?1 AND id=?2",
                params![bot, request.action_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        if let Some((original, json)) = old {
            if original != intent {
                return Err(conflict(
                    "Action ID is permanently bound to its original intent",
                ));
            }
            return Ok((serde_json::from_str(&json).map_err(internal)?, false));
        }
        let unresolved: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM actions WHERE bot=?1 AND unresolved=1)",
                [bot],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if unresolved {
            return Err(ApiError::new(
                ErrorCode::UncertainOutcome,
                "Reconcile the previous action before submitting another mutation",
            ));
        }
        let receipt = Receipt {
            action_id: request.action_id.clone(),
            outcome: Outcome::Prepared,
            adapter_version: adapter.into(),
            evidence: serde_json::Value::Null,
            error: None,
        };
        tx.execute(
            "INSERT INTO actions VALUES(?1,?2,?3,?4,1)",
            params![
                bot,
                request.action_id,
                intent,
                serde_json::to_string(&receipt).map_err(internal)?
            ],
        )
        .map_err(internal)?;
        tx.commit().map_err(internal)?;
        Ok((receipt, true))
    }
    pub fn save_receipt(&mut self, bot: &str, id: &str, receipt: &Receipt) -> Result<()> {
        if receipt.action_id != id {
            return Err(internal("Adapter action identity mismatch"));
        }
        if self.legacy_receipt(bot, id)?.is_some() {
            self.db.execute("INSERT INTO legacy_overrides VALUES(?1,?2,?3) ON CONFLICT(bot,id) DO UPDATE SET receipt=excluded.receipt",params![bot,id,serde_json::to_string(receipt).map_err(internal)?]).map_err(internal)?;
            return Ok(());
        }
        let unresolved = if receipt.outcome == Outcome::Running {
            2
        } else if matches!(
            receipt.outcome,
            Outcome::Prepared | Outcome::Dispatched | Outcome::Uncertain
        ) {
            1
        } else {
            0
        };
        self.db
            .execute(
                "UPDATE actions SET receipt=?3,unresolved=?4 WHERE bot=?1 AND id=?2",
                params![
                    bot,
                    id,
                    serde_json::to_string(receipt).map_err(internal)?,
                    unresolved
                ],
            )
            .map_err(internal)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn running_controls_allow_cancellation_without_replay_and_become_uncertain_after_disconnect() {
        let (_dir, mut s, _key, actor) = setup();
        let request = ActionRequest {
            action_id: "motion-1".into(),
            action: Action::StartMove {
                target: Position {
                    x: 1.,
                    y: 1.,
                    z: 1.,
                },
                jump: false,
                max_ticks: 40,
            },
        };
        let (mut receipt, new) = s.prepare(&actor.bot_id, &request, "fixture").unwrap();
        assert!(new);
        receipt.outcome = Outcome::Running;
        s.save_receipt(&actor.bot_id, &request.action_id, &receipt)
            .unwrap();
        let (same, new) = s.prepare(&actor.bot_id, &request, "fixture").unwrap();
        assert!(!new);
        assert_eq!(same.outcome, Outcome::Running);
        let mut changed = request.clone();
        changed.action = Action::Jump;
        assert_eq!(
            s.prepare(&actor.bot_id, &changed, "fixture")
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        let cancel = ActionRequest {
            action_id: "cancel-1".into(),
            action: Action::CancelControl {
                action_id: request.action_id.clone(),
            },
        };
        let (mut cancellation, new) = s.prepare(&actor.bot_id, &cancel, "fixture").unwrap();
        assert!(new);
        cancellation.outcome = Outcome::Running;
        s.save_receipt(&actor.bot_id, &cancel.action_id, &cancellation)
            .unwrap();
        assert_eq!(s.running(&actor.bot_id).unwrap().len(), 2);
        s.disconnect(&actor.bot_id).unwrap();
        assert!(s.running(&actor.bot_id).unwrap().is_empty());
        for id in [&request.action_id, &cancel.action_id] {
            assert_eq!(
                s.receipt(&actor.bot_id, id).unwrap().unwrap().outcome,
                Outcome::Uncertain
            );
        }
        let other = ActionRequest {
            action_id: "later".into(),
            action: Action::Jump,
        };
        assert_eq!(
            s.prepare(&actor.bot_id, &other, "fixture")
                .unwrap_err()
                .code,
            ErrorCode::UncertainOutcome
        );
    }
    #[test]
    fn restart_preserves_running_intents_as_uncertain_instead_of_replaying_them() {
        let (dir, mut s, _key, actor) = setup();
        let request = ActionRequest {
            action_id: "running-at-crash".into(),
            action: Action::StartMove {
                target: Position {
                    x: 1.,
                    y: 1.,
                    z: 1.,
                },
                jump: false,
                max_ticks: 40,
            },
        };
        let (mut receipt, _) = s.prepare(&actor.bot_id, &request, "fixture").unwrap();
        receipt.outcome = Outcome::Running;
        s.save_receipt(&actor.bot_id, &request.action_id, &receipt)
            .unwrap();
        drop(s);
        let mut reopened = Store::open(&dir.path().join("journal.sqlite")).unwrap();
        let (old, new) = reopened
            .prepare(&actor.bot_id, &request, "fixture")
            .unwrap();
        assert!(!new);
        assert_eq!(old.outcome, Outcome::Uncertain);
    }
    fn setup() -> (tempfile::TempDir, Store, Credential, Actor) {
        let dir = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut s = Store::open(&dir.path().join("journal.sqlite")).unwrap();
        let key = s
            .register(&Registration {
                name: "builder".into(),
            })
            .unwrap();
        let actor = s.authenticate(&key.bot_id, &key.key).unwrap();
        (dir, s, key, actor)
    }
    #[test]
    fn storage_budget_preserves_intent_when_receipt_cannot_fit() {
        let (_dir, mut store, _key, actor) = setup();
        store.set_budget_mib(1).unwrap();
        let request = ActionRequest {
            action_id: "bounded".into(),
            action: Action::Respawn,
        };
        let (mut receipt, new) = store.prepare(&actor.bot_id, &request, "fixture").unwrap();
        assert!(new);
        receipt.outcome = Outcome::Observed;
        receipt.evidence = serde_json::json!("x".repeat(2 * 1024 * 1024));
        assert!(
            store
                .save_receipt(&actor.bot_id, &request.action_id, &receipt)
                .is_err()
        );
        assert_eq!(
            store
                .receipt(&actor.bot_id, &request.action_id)
                .unwrap()
                .unwrap()
                .outcome,
            Outcome::Prepared
        );
        assert!(!store.prepare(&actor.bot_id, &request, "fixture").unwrap().1);
        let maximum: i64 = store
            .db
            .query_row("PRAGMA max_page_count", [], |r| r.get(0))
            .unwrap();
        let count: i64 = store
            .db
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap();
        assert!(count <= maximum);
    }
    #[test]
    fn identity_keys_reservation_and_rotation() {
        let (_d, mut s, key, actor) = setup();
        assert!(s.authenticate("other", &key.key).is_err());
        s.reserve("Woods").unwrap();
        assert!(
            s.register(&Registration {
                name: "woods".into()
            })
            .is_err()
        );
        let next = s.rotate(&actor).unwrap();
        assert!(s.authenticate(&key.bot_id, &key.key).is_err());
        assert_eq!(
            s.authenticate(&next.bot_id, &next.key).unwrap().uuid,
            actor.uuid
        );
        s.revoke(&next.bot_id).unwrap();
        assert!(s.authenticate(&next.bot_id, &next.key).is_err());
    }
    #[test]
    fn duplicate_and_interrupted_actions_never_replay() {
        let (d, mut s, key, actor) = setup();
        let request = ActionRequest {
            action_id: "first".into(),
            action: Action::Respawn,
        };
        assert!(s.prepare(&actor.bot_id, &request, "adapter-a").unwrap().1);
        assert!(!s.prepare(&actor.bot_id, &request, "adapter-a").unwrap().1);
        assert!(
            s.prepare(
                &actor.bot_id,
                &ActionRequest {
                    action_id: "first".into(),
                    action: Action::Jump
                },
                "a"
            )
            .is_err()
        );
        assert!(
            s.prepare(
                &actor.bot_id,
                &ActionRequest {
                    action_id: "next".into(),
                    action: Action::Jump
                },
                "a"
            )
            .is_err()
        );
        drop(s);
        let mut s = Store::open(&d.path().join("journal.sqlite")).unwrap();
        let actor = s.authenticate(&key.bot_id, &key.key).unwrap();
        let (receipt, new) = s.prepare(&actor.bot_id, &request, "adapter-b").unwrap();
        assert!(!new);
        assert_eq!(receipt.outcome, Outcome::Uncertain);
        assert_eq!(receipt.adapter_version, "adapter-a");
    }
    #[test]
    fn single_controller_and_state_survive_reconnect() {
        let (_d, mut s, _key, actor) = setup();
        let r = SessionRequest {
            request_id: "controller-a".into(),
        };
        let (session, _) = s.connect(&actor, &r, "pinned-v1").unwrap();
        s.connected(&actor.bot_id).unwrap();
        assert_eq!(
            s.connect(&actor, &r, "new-v2").unwrap().0.adapter_version,
            "pinned-v1"
        );
        assert!(
            s.connect(
                &actor,
                &SessionRequest {
                    request_id: "b".into()
                },
                "v2"
            )
            .is_err()
        );
        assert!(s.session(&actor.bot_id, "forged").is_err());
        s.disconnect(&actor.bot_id).unwrap();
        assert!(s.session(&actor.bot_id, &session.session_id).is_err());
    }
    #[test]
    fn legacy_import_preserves_identity_pending_state_and_replay_fences() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, mut store, _key, _actor) = setup();
        let path = dir.path().join("legacy.sqlite");
        let legacy = Connection::open(&path).unwrap();
        legacy.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);CREATE TABLE actions(id TEXT PRIMARY KEY,digest TEXT,request TEXT,result TEXT);INSERT INTO meta VALUES('identity','25565:11111111-1111-3111-8111-111111111111');INSERT INTO actions VALUES('old-done','digest-done','private-request','old-result');INSERT INTO actions VALUES('old-pending','digest-pending','private-request',NULL);").unwrap();
        drop(legacy);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let credential = store
            .import_legacy(
                "legacy-bot",
                "11111111-1111-3111-8111-111111111111",
                ".AI_Legacy",
                &path,
            )
            .unwrap();
        assert_eq!(
            store
                .authenticate(&credential.bot_id, &credential.key)
                .unwrap()
                .name,
            ".AI_Legacy"
        );
        assert_eq!(
            store
                .receipt(&credential.bot_id, "old-done")
                .unwrap()
                .unwrap()
                .outcome,
            Outcome::Imported
        );
        assert_eq!(
            store
                .receipt(&credential.bot_id, "old-pending")
                .unwrap()
                .unwrap()
                .outcome,
            Outcome::Uncertain
        );
        assert!(
            store
                .prepare(
                    &credential.bot_id,
                    &ActionRequest {
                        action_id: "old-done".into(),
                        action: Action::Respawn
                    },
                    "new-adapter"
                )
                .is_err()
        );
        assert!(
            store
                .prepare(
                    &credential.bot_id,
                    &ActionRequest {
                        action_id: "new".into(),
                        action: Action::Respawn
                    },
                    "new-adapter"
                )
                .is_err()
        );
        assert_eq!(bytes, std::fs::read(path).unwrap());
    }
}
