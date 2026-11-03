//! Narrow recovery for pre-fix local validation failures; never a game receipt.
use crate::*;

pub(super) fn reconcile_invalid(
    journal: &Journal,
    id: &str,
    instance: &str,
    actor: &str,
    gateway_error: &minecraft_sdk::ApiError,
) -> Result<Option<Value>> {
    if gateway_error.code != ErrorCode::NotFound
        || journal.get("pending_action")?.as_deref() != Some(id)
    {
        return Ok(None);
    }
    let Some((run, sequence)) = id.strip_prefix("brain-").and_then(|s| s.rsplit_once('-')) else {
        return Ok(None);
    };
    if run.len() != 24 || !run.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let Ok(sequence) = sequence.parse::<i64>() else {
        return Ok(None);
    };
    let intent: Option<String> = journal
        .db
        .query_row(
            "SELECT data FROM events WHERE id=?1 AND kind='action_intent'",
            [sequence],
            |r| r.get(0),
        )
        .optional()?;
    let Some(intent) = intent else {
        return Ok(None);
    };
    let intent: Value = serde_json::from_str(&intent)?;
    let Ok(action) = serde_json::from_value::<minecraft_sdk::Action>(intent["action"].clone())
    else {
        return Ok(None);
    };
    let Err(validation) = action.validate() else {
        return Ok(None);
    };
    let started: Option<String> = journal
        .db
        .query_row(
            "SELECT data FROM events WHERE id<?1 AND kind='brain_started' ORDER BY id DESC LIMIT 1",
            [sequence],
            |r| r.get(0),
        )
        .optional()?;
    let Some(started) = started else {
        return Ok(None);
    };
    let started: Value = serde_json::from_str(&started)?;
    if started["run"] != run || started["instance"] != instance || started["uuid"] != actor {
        return Ok(None);
    }
    let proof = json!({"action_id":id,"outcome":"reconciled","scope":"local client-rejected intention; not an authoritative game receipt","gateway_status":"not_found","local_validation_error":validation.code,"recorded_intent_sha256":digest(intent.to_string().as_bytes()),"historical_effect_verified":false,"native_effect_verified":false,"replayed":false});
    let tx = journal.db.unchecked_transaction()?;
    ensure(
        tx.execute(
            "UPDATE meta SET value='' WHERE key='pending_action' AND value=?1",
            [id],
        )? == 1,
        "Pending intention changed during local reconciliation",
    )?;
    tx.execute(
        "INSERT INTO events(at,kind,data) VALUES(?1,'local_intention_reconciled',?2)",
        params![now(), proof.to_string()],
    )?;
    let sequence = tx.last_insert_rowid();
    tx.commit()?;
    println!(
        "{}",
        json!({"event":"local_intention_reconciled","sequence":sequence,"data":proof})
    );
    Ok(Some(proof))
}
