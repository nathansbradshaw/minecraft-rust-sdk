//! Bounded local operator reads. No controller, profile, model or game connection.
use crate::{Journal, Result, digest, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{ffi::OsString, fs, path::Path};

const DEFAULT_ROWS: usize = 32;
const DEFAULT_BYTES: usize = 262144;
const MAX_ROWS: usize = 256;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const ENVELOPE_RESERVE: usize = 1024;
const MAX_KIND_BYTES: u64 = 1024;

pub fn cli(args: &[OsString]) -> Result<Option<Value>> {
    if args.first().is_none_or(|a| a != "journal-events") {
        return Ok(None);
    }
    ensure(
        (3..=5).contains(&args.len()),
        "journal-events JOURNAL.sqlite AFTER [ROWS [BYTES]]",
    )?;
    let number = |index: usize| -> Result<&str> {
        args[index]
            .to_str()
            .ok_or("Numeric UTF-8 argument required".into())
    };
    let after: i64 = number(2)?.parse()?;
    let rows = if args.len() >= 4 {
        number(3)?.parse()?
    } else {
        DEFAULT_ROWS
    };
    let bytes = if args.len() == 5 {
        number(4)?.parse()?
    } else {
        DEFAULT_BYTES
    };
    Ok(Some(read(Path::new(&args[1]), after, rows, bytes)?))
}

/// One committed SQLite snapshot, including live WAL rows. Output is private.
pub fn read(path: &Path, after: i64, rows: usize, bytes: usize) -> Result<Value> {
    ensure(
        after >= 0 && (1..=MAX_ROWS).contains(&rows) && (4096..=MAX_BYTES).contains(&bytes),
        "Require a nonnegative cursor, 1..256 rows and 4096..2097152 bytes",
    )?;
    let meta = fs::symlink_metadata(path)?;
    ensure(
        meta.is_file() && !meta.file_type().is_symlink(),
        "Existing regular journal file required",
    )?;
    // Canonical absolute paths cannot be reinterpreted as SQLite URI options.
    let journal = Journal::open_read_only(&path.canonicalize()?)?;
    let tx = journal.db.unchecked_transaction()?;
    let page = snapshot(&tx, after, rows, bytes)?;
    tx.commit()?;
    Ok(page)
}

fn schema(tx: &Transaction<'_>) -> Result<()> {
    let kind: Option<String> = tx
        .query_row(
            "SELECT type FROM sqlite_master WHERE name='events'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    ensure(
        kind.as_deref() == Some("table"),
        "Existing events journal table required",
    )?;
    // Return scalar checks rather than loading arbitrary schema-name strings.
    let columns: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('events')",
        [],
        |r| r.get(0),
    )?;
    let valid:i64=tx.query_row("SELECT COUNT(*) FROM pragma_table_info('events') WHERE (name='id' AND upper(type)='INTEGER' AND pk=1) OR (name='at' AND upper(type)='INTEGER' AND \"notnull\"=1 AND pk=0) OR (name IN ('kind','data') AND upper(type)='TEXT' AND \"notnull\"=1 AND pk=0)",[],|r|r.get(0))?;
    ensure(
        columns == 4 && valid == 4,
        "Expected events(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL)",
    )
}

fn snapshot(tx: &Transaction<'_>, after: i64, limit: usize, max_bytes: usize) -> Result<Value> {
    schema(tx)?;
    let upper: i64 = tx.query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get(0))?;
    let mut stmt = tx.prepare("SELECT id,at,LENGTH(CAST(kind AS BLOB)),LENGTH(CAST(data AS BLOB)) FROM events WHERE id>?1 AND id<=?2 ORDER BY id LIMIT ?3")?;
    // Inspect SQLite byte lengths BEFORE requesting any raw payload. At most
    // limit+1 small metadata tuples are retained, regardless of payload size.
    let sizes = stmt
        .query_map(params![after, upper, i64::try_from(limit + 1)?], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    let mut used = 0;
    let mut cursor = after;
    let mut omitted = 0;
    let mut stop = "snapshot_end";
    let mut next_row = None;
    for (id, at, kind_bytes, data_bytes) in sizes {
        if out.len() == limit {
            stop = "row_limit";
            next_row = Some(id);
            break;
        }
        ensure(
            kind_bytes >= 0 && data_bytes >= 0,
            "Invalid journal byte lengths",
        )?;
        let kind_bytes = kind_bytes as u64;
        let data_bytes = data_bytes as u64;
        let raw_limit = (max_bytes - ENVELOPE_RESERVE) as u64;
        let marker = |reason: &str| {
            json!({"id":id,"at":at,"kind":null,"data_json":null,"truncated":true,
            "omitted":{"reason":reason,"kind_bytes":kind_bytes,"data_bytes":data_bytes,"sha256":null}})
        };
        let (mut row, mut truncated) = if kind_bytes > MAX_KIND_BYTES || data_bytes > raw_limit {
            (marker("raw_row_exceeds_limit"), true)
        } else {
            let (kind, data): (String, String) =
                tx.query_row("SELECT kind,data FROM events WHERE id=?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            ensure(
                kind.len() as u64 == kind_bytes && data.len() as u64 == data_bytes,
                "Journal row changed within snapshot",
            )?;
            (
                json!({"id":id,"at":at,"kind":kind,"data_sha256":digest(data.as_bytes()),"data_json":data,"truncated":false}),
                false,
            )
        };
        let mut size = serde_json::to_vec(&row)?.len() + 1;
        if size + used + ENVELOPE_RESERVE > max_bytes {
            if out.is_empty() {
                row = marker("encoded_row_exceeds_limit");
                truncated = true;
                size = serde_json::to_vec(&row)?.len() + 1;
            } else {
                stop = "byte_limit";
                next_row = Some(id);
                break;
            }
        }
        ensure(
            size + used + ENVELOPE_RESERVE <= max_bytes,
            "Diagnostic envelope limit exceeded",
        )?;
        used += size;
        omitted += usize::from(truncated);
        cursor = id;
        out.push(row);
    }
    let page = json!({"format":"minecraft-agent-journal-page-v1","after_sequence":after,"next_cursor":cursor,
        "snapshot_last_sequence":upper,"rows":out,"complete":stop=="snapshot_end","stop_reason":stop,
        "next_row_sequence":next_row,"truncated_rows":omitted,"private_data":true,
        "data_complete":stop=="snapshot_end" && omitted==0,
        "scope":"Committed local journal snapshot; historical data, no game or model connection",
        "limits":{"rows":limit,"bytes":max_bytes},"native_connections_opened":0,"models_called":0});
    ensure(
        serde_json::to_vec(&page)?.len() < max_bytes,
        "Diagnostic output exceeds byte limit",
    )?;
    Ok(page)
}

#[cfg(test)]
mod tests;
