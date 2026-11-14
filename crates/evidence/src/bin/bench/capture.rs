use super::*;
use minecraft_sdk_core::{Action, ActionRequest, Receipt};
use minecraft_sdk_evidence::{ExpectedAction, Spec};
use rusqlite::{Connection, OpenFlags, params};
use std::collections::{BTreeMap, BTreeSet};

/// Only authoritative rows are read; no Store, migrate, status or reconcile calls.
pub fn native(
    f: &Fixture,
    case: &Path,
    journal: &Path,
    trace: &Path,
    template: &Path,
    stats: &[PathBuf],
) -> Result<()> {
    native_with_space(f, case, journal, trace, template, stats, || {
        free_bytes(case)
    })
}
pub(super) fn native_with_space(
    f: &Fixture,
    case: &Path,
    journal: &Path,
    trace: &Path,
    template: &Path,
    stats: &[PathBuf],
    mut available: impl FnMut() -> Result<u64>,
) -> Result<()> {
    if f.is_combined() {
        ensure(
            available()? >= 502_000_000_000,
            "combined capture needs2GB evidence headroom above500GB reserve",
        )?;
        ensure(
            trace.with_extension("status.json").is_file(),
            "combined capture requires original complete-stream status",
        )?;
    }
    let trace_bytes = read(trace, f.trace_bytes_limit())?;
    ensure(
        f.secrets()?.scrub(&trace_bytes) == trace_bytes,
        "trace contains allocated credential; refusing credential log copy",
    )?;
    let mut sequences = BTreeMap::new();
    let mut prefixes = BTreeSet::new();
    let mut projection = Vec::new();
    let mut non_json = 0;
    for line in trace_bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        match serde_json::from_slice::<Value>(line) {
            Ok(value) => {
                projection.extend_from_slice(line);
                projection.push(b'\n');
                if value["event"] == "body_action_intent" {
                    let seq = value["sequence"].as_u64().ok_or("intent sequence absent")?;
                    let action: Action = serde_json::from_value(value["data"]["action"].clone())?;
                    ensure(
                        sequences.insert(seq, action).is_none() || f.is_combined(),
                        "duplicate original intent sequence",
                    )?;
                }
                if value["event"] == "body_action_receipt" {
                    let action_id = value["data"]["action_id"]
                        .as_str()
                        .ok_or("receipt action ID absent")?;
                    let (prefix, suffix) = action_id
                        .rsplit_once('-')
                        .ok_or("body action ID shape differs")?;
                    let _: u64 = suffix.parse()?;
                    ensure(
                        prefix.starts_with("body-") && id(prefix),
                        "body epoch identity invalid",
                    )?;
                    prefixes.insert(format!("{prefix}-"));
                }
            }
            Err(_) => non_json += 1,
        }
    }
    let mut projected = create(&case.join("native-projection.jsonl"))?;
    projected.write_all(&projection)?;
    projected.sync_all()?;
    // The original trace is copied intact too: a final fatal non-JSON line cannot disappear.
    let mut original = create(&case.join("native-original.log"))?;
    original.write_all(&trace_bytes)?;
    original.sync_all()?;
    ensure(
        f.is_combined() || prefixes.len() <= 1,
        "multiple body epochs require separate explicit capture",
    )?;
    let prefix = if f.is_combined() {
        String::new()
    } else {
        match (&f.trace_action_prefix, prefixes.first()) {
        (Some(expected), Some(found)) => {
            ensure(expected == found, "frozen action prefix differs")?;
            expected.clone()
        }
        (Some(expected), None) => expected.clone(),
        (None, Some(found)) => found.clone(),
        (None, None) => return Err(
            "cannot infer original action epoch without a receipt; bind exact prefix explicitly"
                .into(),
        ),
    }
    };
    let intentions: BTreeMap<_, _> = sequences
        .into_iter()
        .map(|(seq, action)| (format!("{prefix}{seq}"), action))
        .collect();
    ensure(
        intentions.len() <= f.action_limit(),
        "too many actions for bounded capture",
    )?;
    minecraft_sdk_evidence::private_file(journal, 64 * 1024 * 1024 * 1024)?;
    let mut db = Connection::open_with_flags(journal, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(3))?;
    db.execute_batch("PRAGMA query_only=ON")?;
    let tx = db.transaction()?;
    let instance: String =
        tx.query_row("SELECT value FROM metadata WHERE key='instance'", [], |r| {
            r.get(0)
        })?;
    ensure(instance == f.instance_id, "wrong authoritative instance")?;
    let bot: String = tx.query_row("SELECT id FROM bots WHERE uuid=?1", [&f.player_uuid], |r| {
        r.get(0)
    })?;
    let actor_total: i64 =
        tx.query_row("SELECT count(*) FROM actions WHERE bot=?1", [&bot], |r| {
            r.get(0)
        })?;
    if f.is_combined() {
        let baseline = json(
            &trace
                .parent()
                .ok_or("trace parent absent")?
                .join("combined-episode-baseline.json"),
            16384,
        )?;
        let profile = json(f.host_profile()?, 16384)?;
        ensure(
            baseline["instance_id"] == f.instance_id
                && baseline["player_uuid"] == f.player_uuid
                && baseline["bot_id"] == bot
                && profile["credential"]["bot_id"] == bot
                && baseline["actions_before"] == 0
                && baseline["sessions_before"] == 0
                && baseline["fixture_sha256"] == sha256(&read(&case.join("fixture.json"), 65536)?),
            "combined actor-wide capture requires its pristine same-fixture baseline",
        )?;
        ensure(
            actor_total >= 0 && actor_total as u64 <= f.action_limit() as u64,
            "combined actor-wide action count exceeds frozen bound; no truncated export",
        )?;
    }
    let mut rows = Vec::new();
    let mut expected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    {
        let mut query=tx.prepare("SELECT id,intent,receipt FROM actions WHERE bot=?1 AND (?2='' OR substr(id,1,length(?2))=?2) ORDER BY id LIMIT ?3")?;
        let found = query.query_map(params![bot, prefix, (f.action_limit() + 1) as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in found {
            let (id, intent, receipt) = row?;
            let secrets = f.secrets()?;
            ensure(
                secrets.scrub(intent.as_bytes()) == intent.as_bytes()
                    && secrets.scrub(receipt.as_bytes()) == receipt.as_bytes(),
                "native row contains allocated credential; refusing log copy",
            )?;
            total = total
                .saturating_add(intent.len())
                .saturating_add(receipt.len());
            ensure(
                total as u64 <= f.trace_bytes_limit() && rows.len() < f.action_limit(),
                "original row capture exceeds budget",
            )?;
            let action: Action = serde_json::from_str(&intent)?;
            let parsed: Receipt = serde_json::from_str(&receipt)?;
            ensure(parsed.action_id == id, "receipt ID differs")?;
            rows.push(json!({"action_id":id,"intent":serde_json::from_str::<Value>(&intent)?,"receipt":serde_json::from_str::<Value>(&receipt)?,"receipt_sha256":sha256(receipt.as_bytes())}));
            seen.insert(id.clone());
            expected.push(ExpectedAction {
                request: ActionRequest {
                    action_id: id,
                    action,
                },
                outcome: parsed.outcome,
            });
        }
    }
    tx.commit()?;
    ensure(
        !f.is_combined() || seen.len() as i64 == actor_total,
        "combined snapshot omitted original actor rows",
    )?;
    save(
        &case.join("original-rows.json"),
        &json!({"rows":rows,"authority":"Read-only original SQLite snapshot; running/uncertain are not fabricated terminal outcomes."}),
    )?;
    let matches = f.is_combined()
        || (seen.len() == intentions.len()
            && expected.iter().all(|e| {
                intentions.get(&e.request.action_id).is_some_and(|a| {
                    serde_json::to_value(a).ok() == serde_json::to_value(&e.request.action).ok()
                })
            }));
    save(
        &case.join("intent-match.json"),
        &json!({"exact_match":if f.is_combined(){Value::Null}else{json!(matches)},"actor_wide_pristine_baseline":f.is_combined(),"trace_count":intentions.len(),"stored_count":seen.len(),"non_json_lines_retained":non_json,"scope":if f.is_combined(){"All actor prefixes from a pristine authoritative baseline; body-prefix intention matching is not asserted for model-generated actions."}else{"Exact frozen body-prefix intention matching"}}),
    )?;
    ensure(matches, "trace does not match ALL stored original actions")?;
    let mut spec: Spec = serde_json::from_slice(&read(template, 65536)?)?;
    ensure(
        spec.player_uuid == f.player_uuid
            && spec.instance_id == f.instance_id
            && spec.source_revision == f.source_revision
            && f.artifacts
                .iter()
                .find(|a| a.container_path == f.handler)
                .is_some_and(|a| a.file.sha256 == spec.controller_sha256)
            && spec.trace_sha256 == sha256(&trace_bytes),
        "capture template identity/hash differs",
    )?;
    let mut files = Vec::new();
    let mut handler_streams = Vec::new();
    for (extension, name, limit) in [
        ("stderr", "source-stderr.log", f.trace_bytes_limit()),
        ("status.json", "source-status.json", 16384),
    ] {
        let path = trace.with_extension(extension);
        if path.exists() {
            let bytes = read(&path, limit)?;
            if f.is_combined() && extension == "status.json" {
                let status: Value = serde_json::from_slice(&bytes)?;
                ensure(
                    status["output_limit_exceeded"] == false,
                    "combined trace truncation or old unknown stream-limit status; evidence incomplete",
                )?;
            }
            ensure(
                f.secrets()?.scrub(&bytes) == bytes,
                "handler sidecar contains allocated credential; refusing copy",
            )?;
            let mut out = create(&case.join(name))?;
            out.write_all(&bytes)?;
            out.sync_all()?;
            handler_streams.push(json!({"file":name,"sha256":sha256(&bytes),"scope":"Complete supplied handler sidecar, including fatal stderr/nonzero status"}));
        }
    }
    let mut groups = Vec::new();
    let mut begin = 0;
    while begin < expected.len() {
        let mut end = begin;
        let mut budget = 0;
        while end < expected.len() && end - begin < 128 {
            let bytes = serde_json::to_vec_pretty(&rows[end])?.len();
            if budget + bytes > 6 * 1024 * 1024 {
                break;
            }
            budget += bytes;
            end += 1;
        }
        ensure(end > begin, "one native row exceeds bounded report budget")?;
        groups.push(&expected[begin..end]);
        begin = end;
    }
    let mut report_bytes = 0usize;
    for (index, chunk) in groups.into_iter().enumerate() {
        spec.actions = chunk.to_vec();
        let name = format!("native-shard-{index:03}.json");
        let value = minecraft_sdk_evidence::capture(journal, &spec)?;
        let bytes = serde_json::to_vec_pretty(&value)?;
        ensure(
            bytes.len() <= 8 * 1024 * 1024,
            "native shard exceeds evidence budget",
        )?;
        report_bytes = report_bytes.saturating_add(bytes.len());
        if f.is_combined() {
            ensure(
                report_bytes <= 512 * 1024 * 1024
                    && available()? >= 500_000_000_000 + bytes.len() as u64,
                "combined reports exceed bounded budget or disk reserve; original database must be retained",
            )?;
        }
        let mut out = create(&case.join(&name))?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        files.push(json!({"file":name,"sha256":sha256(&bytes),"actions":chunk.len()}));
    }
    let mut stats_files = Vec::new();
    ensure(
        stats.len() <= 4,
        "at most before/after player and stats files",
    )?;
    for (index, path) in stats.iter().enumerate() {
        let bytes = read(path, 4 * 1024 * 1024)?;
        let _: Value = serde_json::from_slice(&bytes)?;
        let name = format!("saved-input-{index}.json");
        let mut out = create(&case.join(&name))?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        stats_files.push(json!({"file":name,"sha256":sha256(&bytes),"interpretation":"Operator-supplied projection, not an automatic death or goal verdict"}));
    }
    let mut outcomes = BTreeMap::new();
    for e in &expected {
        *outcomes
            .entry(
                serde_json::to_value(&e.outcome)?
                    .as_str()
                    .ok_or("outcome string absent")?
                    .to_owned(),
            )
            .or_insert(0usize) += 1;
    }
    save(
        &case.join("capture-manifest.json"),
        &json!({"case_id":f.case_id,"player_uuid":f.player_uuid,"trace_sha256":sha256(&trace_bytes),"original_rows_sha256":hash_file(&case.join("original-rows.json"))?,"all_original_actions":expected.len(),"action_scope":if f.is_combined(){"all actor prefixes after a pristine baseline"}else{"one frozen body prefix"},"action_limit":f.action_limit(),"trace_byte_limit":f.trace_bytes_limit(),"native_report_bytes":report_bytes,"outcomes":outcomes,"native_reports":files,"saved_inputs":stats_files,"handler_sidecars":handler_streams,"missing_sidecars_are_unknown":true,"projection_sha256":sha256(&projection),"non_json_lines_retained":non_json,"certifies_goal_completion":false,"aggregate_reports_warning":"Do not average shard percentiles or drop missing/uncertain attempts."}),
    )
}
