//! Operator-side native receipt capture. Never run against bot-controlled journals.
use minecraft_sdk_core::{Action, ActionRequest, Outcome, Receipt, valid_id};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn ensure(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn private_file(path: &Path, limit: u64) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure(
        m.is_file()
            && !m.file_type().is_symlink()
            && m.mode() & 0o077 == 0
            && m.uid() == unsafe { libc::geteuid() }
            && m.len() <= limit,
        "Private, bounded, operator-owned regular file required",
    )
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedAction {
    pub request: ActionRequest,
    pub outcome: Outcome,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub boundary: String,
    pub message: String,
    pub artifact_sha256: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceUsage {
    pub measurement: String,
    pub container_memory_bytes: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub pids: Option<u32>,
    pub notes: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub run_id: String,
    pub goal: String,
    pub instance_id: String,
    pub player_uuid: String,
    pub source_revision: String,
    pub controller_sha256: String,
    pub program_source_sha256: String,
    pub program_binary_sha256: String,
    pub trace_sha256: String,
    pub model_requests: BTreeMap<String, u32>,
    pub resources: ResourceUsage,
    pub assists: Vec<String>,
    pub interventions: Vec<String>,
    pub failures: Vec<Failure>,
    pub actions: Vec<ExpectedAction>,
}
impl Spec {
    pub fn validate(&self) -> Result<()> {
        ensure(
            valid_id(&self.run_id) && !self.goal.trim().is_empty() && self.goal.len() <= 2048,
            "Run identity and bounded goal required",
        )?;
        ensure(
            hex(&self.instance_id, 64)
                && self.player_uuid.len() == 36
                && self
                    .player_uuid
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b'-'),
            "Instance/character identity required",
        )?;
        ensure(
            hex(&self.source_revision, 40),
            "Full source commit required (operator-attested)",
        )?;
        for hash in [
            &self.controller_sha256,
            &self.program_source_sha256,
            &self.program_binary_sha256,
            &self.trace_sha256,
        ] {
            ensure(hex(hash, 64), "Artifact SHA-256 bindings required")?;
        }
        ensure(
            self.model_requests
                .keys()
                .all(|m| ["gpt-6-luna", "gpt-6.1-sol"].contains(&m.as_str())),
            "Unknown model usage category",
        )?;
        ensure(
            ["end-of-run", "peak", "sample", "unmeasured"]
                .contains(&self.resources.measurement.as_str())
                && self
                    .resources
                    .cpu_percent
                    .is_none_or(|n| n.is_finite() && n >= 0.)
                && self.resources.notes.len() <= 4096,
            "Labelled resource measurement required",
        )?;
        ensure(
            self.assists.len() <= 64
                && self.interventions.len() <= 64
                && self
                    .assists
                    .iter()
                    .chain(&self.interventions)
                    .all(|s| s.len() <= 2048),
            "Bounded assists/interventions required",
        )?;
        ensure(
            self.failures.len() <= 64
                && self.failures.iter().all(|f| {
                    !f.boundary.is_empty()
                        && f.boundary.len() <= 256
                        && f.message.len() <= 2048
                        && hex(&f.artifact_sha256, 64)
                }),
            "Bounded failures and artifact bindings required",
        )?;
        ensure(
            !self.actions.is_empty() && self.actions.len() <= 32,
            "One to 32 native actions required",
        )?;
        let mut ids = std::collections::BTreeSet::new();
        for expected in &self.actions {
            ensure(
                valid_id(&expected.request.action_id) && ids.insert(&expected.request.action_id),
                "Unique valid action IDs required",
            )?;
            expected.request.action.validate()?;
        }
        Ok(())
    }
}
/// Does not open the gateway Store, acquire its writer lock, migrate it, or mutate
/// any session. A deferred read transaction gives one consistent journal snapshot.
pub fn capture(journal: &Path, spec: &Spec) -> Result<Value> {
    spec.validate()?;
    private_file(journal, 64 * 1024 * 1024 * 1024)?;
    let mut db = Connection::open_with_flags(journal, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(3))?;
    db.execute_batch("PRAGMA query_only=ON;")?;
    let tx = db.transaction()?;
    let instance: String =
        tx.query_row("SELECT value FROM metadata WHERE key='instance'", [], |r| {
            r.get(0)
        })?;
    ensure(
        instance == spec.instance_id,
        "Wrong authoritative server instance",
    )?;
    let bot: String = tx.query_row(
        "SELECT id FROM bots WHERE uuid=?1",
        [&spec.player_uuid],
        |r| r.get(0),
    )?;
    let mut native = Vec::new();
    let mut observed = 0;
    for expected in &spec.actions {
        let (intent, bytes):(String,String) = tx.query_row("SELECT intent,receipt FROM actions WHERE bot=?1 AND id=?2 AND length(intent)<=16384 AND length(receipt)<=1048576", params![bot,expected.request.action_id], |r|Ok((r.get(0)?,r.get(1)?)))?;
        let action: Action = serde_json::from_str(&intent)?;
        ensure(
            serde_json::to_value(&action)? == serde_json::to_value(&expected.request.action)?,
            "Native action intent differs from claimed intent",
        )?;
        let receipt: Receipt = serde_json::from_str(&bytes)?;
        ensure(
            receipt.action_id == expected.request.action_id && receipt.outcome == expected.outcome,
            "Native action ID/outcome differs from expectation",
        )?;
        if receipt.outcome == Outcome::Observed {
            ensure(
                receipt.error.is_none() && receipt.evidence["native_effect_verified"] == true,
                "Observed receipt lacks native effect evidence",
            )?;
            observed += 1;
        }
        native.push(json!({"action":action,"receipt":receipt,"journal_row_sha256":sha256(bytes.as_bytes())}));
    }
    tx.commit()?;
    Ok(
        json!({"format":"minecraft-sdk-native-run-evidence-v1","captured_at":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"spec":spec,"native_actions":native,"verified_observed_actions":observed,"authority":"Native action rows were independently read from the operator-owned gateway journal. Code/source/model/resource bindings and interventions are operator attestations. This report does not certify program execution, goal completion, autonomous competence or comparative performance."}),
    )
}
