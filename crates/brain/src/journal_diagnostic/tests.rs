use super::*;
use rusqlite::Connection;
use std::{
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Journal) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("body.sqlite");
    let journal = Journal::open(&path).unwrap();
    (dir, path, journal)
}

#[test]
fn raw_rows_cursors_and_append_progress_are_exact() {
    let (_dir, path, journal) = fixture();
    let raw = " { \"action_id\": \"original-id\", \"health\": 7 }\n";
    journal
        .db
        .execute(
            "INSERT INTO events(at,kind,data) VALUES(10,'native',?1)",
            [raw],
        )
        .unwrap();
    journal.event("next", json!({"phase":"two"})).unwrap();
    let first = read(&path, 0, 1, 4096).unwrap();
    assert_eq!(first["rows"][0]["id"], 1);
    assert_eq!(first["rows"][0]["at"], 10);
    assert_eq!(first["rows"][0]["kind"], "native");
    assert_eq!(first["rows"][0]["data_json"], raw);
    assert_eq!(first["rows"][0]["truncated"], false);
    assert_eq!(first["complete"], false);
    assert_eq!(first["next_cursor"], 1);
    assert_eq!(first["next_row_sequence"], 2);
    journal.event("append", json!({"phase":"three"})).unwrap();
    let next = read(&path, 1, 32, 4096).unwrap();
    assert_eq!(
        next["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(next["snapshot_last_sequence"], 3);
    assert_eq!(next["next_cursor"], 3);
    assert_eq!(next["data_complete"], true);
    let beyond = read(&path, 99, 32, 4096).unwrap();
    assert_eq!(beyond["next_cursor"], 99, "never rewind caller cursor");
    assert_eq!(beyond["rows"], json!([]));
}

#[test]
fn one_read_only_transaction_keeps_a_consistent_snapshot_across_writer_appends() {
    let (_dir, path, writer) = fixture();
    writer.event("first", json!({"n":1})).unwrap();
    let reader = Journal::open_read_only(&path).unwrap();
    let tx = reader.db.unchecked_transaction().unwrap();
    assert_eq!(
        tx.query_row("SELECT MAX(id) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    writer.event("later", json!({"n":2})).unwrap();
    let old = snapshot(&tx, 0, 32, 4096).unwrap();
    assert_eq!(old["snapshot_last_sequence"], 1);
    assert_eq!(old["rows"].as_array().unwrap().len(), 1);
    tx.commit().unwrap();
    assert_eq!(read(&path, 1, 32, 4096).unwrap()["rows"][0]["id"], 2);
}

#[test]
fn oversized_rows_are_sized_before_fetch_and_explicitly_omitted_without_blocking_progress() {
    let (_dir, path, journal) = fixture();
    // An8MiB BLOB would fail String decoding if accidentally fetched; the
    // metadata-only oversized path must still return its exact sequence/size.
    journal
        .db
        .execute(
            "INSERT INTO events(at,kind,data) VALUES(10,'oversized',zeroblob(8388608))",
            [],
        )
        .unwrap();
    journal.event("useful", json!({"health":6})).unwrap();
    let page = read(&path, 0, 32, 4096).unwrap();
    assert_eq!(page["rows"][0]["id"], 1);
    assert_eq!(page["rows"][0]["data_json"], Value::Null);
    assert_eq!(page["rows"][0]["omitted"]["data_bytes"], 8388608);
    assert_eq!(
        page["rows"][0]["omitted"]["reason"],
        "raw_row_exceeds_limit"
    );
    assert_eq!(page["rows"][1]["id"], 2);
    assert_eq!(page["next_cursor"], 2);
    assert_eq!(page["complete"], true, "cursor exhausted this snapshot");
    assert_eq!(
        page["data_complete"], false,
        "explicit omission is not complete data"
    );
    assert!(serde_json::to_vec(&page).unwrap().len() + 1 <= 4096);
}

#[test]
fn encoded_byte_bounds_and_partial_pages_never_skip_unreturned_rows() {
    let (_dir, path, journal) = fixture();
    for n in 1..=3 {
        journal
            .event("payload", json!({"n":n,"text":"x".repeat(1500)}))
            .unwrap();
    }
    let mut cursor = 0;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let page = read(&path, cursor, 32, 4096).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() + 1 <= 4096);
        ids.extend(
            page["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_i64().unwrap()),
        );
        let next = page["next_cursor"].as_i64().unwrap();
        assert!(next > cursor);
        cursor = next;
        if page["complete"] == true {
            break;
        }
        assert_eq!(page["next_row_sequence"], cursor + 1);
    }
    assert_eq!(ids, vec![1, 2, 3]);
    let (_dir2, path2, journal2) = fixture();
    let encoded = serde_json::to_string(&"\\".repeat(1500)).unwrap();
    assert!(encoded.len() < 4096 - ENVELOPE_RESERVE);
    journal2
        .db
        .execute(
            "INSERT INTO events(at,kind,data) VALUES(1,'escaped',?1)",
            [encoded],
        )
        .unwrap();
    let page = read(&path2, 0, 32, 4096).unwrap();
    assert_eq!(
        page["rows"][0]["omitted"]["reason"],
        "encoded_row_exceeds_limit"
    );
    assert_eq!(page["next_cursor"], 1);
}

#[test]
fn missing_read_only_and_wrong_schema_files_are_never_created_migrated_or_chmodded() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sqlite");
    assert!(read(&missing, 0, 32, 4096).is_err());
    assert!(!missing.exists());
    let cold = dir.path().join("cold.sqlite");
    let db = Connection::open(&cold).unwrap();
    db.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL);INSERT INTO events VALUES(1,1,'cold','{\"n\":1}');").unwrap();
    drop(db);
    fs::set_permissions(&cold, fs::Permissions::from_mode(0o400)).unwrap();
    let before = fs::read(&cold).unwrap();
    assert_eq!(read(&cold, 0, 32, 4096).unwrap()["rows"][0]["id"], 1);
    assert_eq!(fs::read(&cold).unwrap(), before);
    assert_eq!(
        fs::metadata(&cold).unwrap().permissions().mode() & 0o777,
        0o400
    );
    assert!(!cold.with_extension("sqlite-wal").exists());
    assert!(!cold.with_extension("sqlite-shm").exists());
    let wrong = dir.path().join("wrong.sqlite");
    let db = Connection::open(&wrong).unwrap();
    db.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL,unexpected TEXT);").unwrap();
    drop(db);
    let before = fs::read(&wrong).unwrap();
    assert!(read(&wrong, 0, 32, 4096).is_err());
    assert_eq!(fs::read(&wrong).unwrap(), before);
    let alias = dir.path().join("alias.sqlite");
    std::os::unix::fs::symlink(&cold, &alias).unwrap();
    assert!(read(&alias, 0, 32, 4096).is_err());
}

#[test]
fn exclusive_locks_fail_with_bounded_busy_wait_and_no_empty_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.sqlite");
    let writer = Connection::open(&path).unwrap();
    writer.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL);BEGIN EXCLUSIVE;").unwrap();
    let started = Instant::now();
    assert!(read(&path, 0, 32, 4096).is_err());
    assert!(started.elapsed() < Duration::from_secs(8));
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(read(&path, 0, 32, 4096).unwrap()["rows"], json!([]));
}

#[test]
fn closed_cli_bounds_reject_invalid_arguments_before_any_file_open() {
    assert!(cli(&[OsString::from("run")]).unwrap().is_none());
    for args in [
        vec!["journal-events"],
        vec!["journal-events", "missing", "-1"],
        vec!["journal-events", "missing", "0", "0"],
        vec!["journal-events", "missing", "0", "1", "2097153"],
        vec!["journal-events", "missing", "0", "257"],
    ] {
        assert!(cli(&args.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err());
    }
}
