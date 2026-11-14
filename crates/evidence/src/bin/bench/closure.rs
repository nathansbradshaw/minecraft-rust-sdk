//! Operator-attested authoritative journal snapshots. Never mutate or migrate it.
use super::*;
use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::MetadataExt;

pub fn gateway_absent(f: &Fixture, case: &Path) -> Result<bool> {
    let result = snapshot(f, case);
    if let Err(error) = &result {
        let secrets = cleanup_host_profile(f)?;
        let error = secrets.scrub(error.to_string().as_bytes());
        save(
            &case.join("gateway-closure-snapshot-failed.json"),
            &json!({"verified":false,
            "error":String::from_utf8_lossy(&error),"journal_mutation_attempted":false}),
        )?;
    }
    result
}
fn snapshot(f: &Fixture, case: &Path) -> Result<bool> {
    let path = f
        .operator_gateway_journal
        .as_ref()
        .ok_or("no reviewed authoritative operator gateway journal")?;
    canonical(path)?;
    ensure(
        !path.starts_with(&f.workspace) && !path.starts_with(&f.secret_dir),
        "operator journal exposed to bot",
    )?;
    minecraft_sdk_evidence::private_file(path, 64 * 1024 * 1024 * 1024)?;
    let before = fs::symlink_metadata(path)?;
    cleanup_host_profile(f)?;
    let profile = json(f.host_profile()?, 16384)?;
    let bot = profile["credential"]["bot_id"]
        .as_str()
        .ok_or("operator bot ID absent")?;
    let started = utc_ms()?;
    let mut db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(Duration::from_secs(3))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF")?;
    let tx = db.transaction()?;
    let instance: String =
        tx.query_row("SELECT value FROM metadata WHERE key='instance'", [], |r| {
            r.get(0)
        })?;
    ensure(
        instance == f.instance_id,
        "wrong authoritative gateway instance",
    )?;
    let actor: String = tx.query_row("SELECT uuid FROM bots WHERE id=?1", [bot], |r| r.get(0))?;
    ensure(
        actor == f.player_uuid,
        "authoritative gateway bot ID/UUID differs",
    )?;
    // Any row blocks closure, including connecting or unrecognized states.
    let sessions: i64 = tx.query_row("SELECT count(*) FROM sessions WHERE bot=?1", [bot], |r| {
        r.get(0)
    })?;
    tx.commit()?;
    let after = fs::symlink_metadata(path)?;
    ensure(
        before.dev() == after.dev() && before.ino() == after.ino(),
        "authoritative journal file identity changed",
    )?;
    save(
        &case.join("gateway-closure-snapshot.json"),
        &json!({"instance_id":instance,
        "player_uuid":actor,"bot_id":bot,"sessions_including_connecting":sessions,"gateway_session_absent":sessions == 0,
        "journal_device":before.dev(),"journal_inode":before.ino(),"started_utc_ms":started,"finished_utc_ms":utc_ms()?,
        "read_only":true,"scope":"Read-only transaction on operator-attested authoritative journal; no tokens or keys retained. Separate native actor absence is required; no continuous admission fence."}),
    )?;
    Ok(sessions == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_snapshot_binds_exact_identity_and_counts_all_session_states_without_writes() {
        for scenario in 0..7 {
            let mut s = super::super::tests::setup();
            let path =
                s.f.workspace
                    .parent()
                    .unwrap()
                    .join("operator-gateway.sqlite");
            drop(create(&path).unwrap());
            let db = Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT); CREATE TABLE bots(id TEXT PRIMARY KEY,uuid TEXT); CREATE TABLE sessions(bot TEXT PRIMARY KEY,state TEXT);").unwrap();
            db.execute(
                "INSERT INTO metadata VALUES('instance',?1)",
                [if scenario == 4 {
                    "wrong-instance"
                } else {
                    &s.f.instance_id
                }],
            )
            .unwrap();
            db.execute(
                "INSERT INTO bots VALUES(?1,?2)",
                rusqlite::params![
                    if scenario == 6 { "wrong-bot" } else { "owned" },
                    if scenario == 5 {
                        "wrong-uuid"
                    } else {
                        &s.f.player_uuid
                    }
                ],
            )
            .unwrap();
            if (1..=3).contains(&scenario) {
                db.execute(
                    "INSERT INTO sessions VALUES('owned',?1)",
                    [["", "connecting", "connected", "unknown"][scenario]],
                )
                .unwrap();
            }
            drop(db);
            let original = fs::read(&path).unwrap();
            s.f.operator_gateway_journal = Some(path.clone());
            let result = gateway_absent(&s.f, &s.case);
            if scenario < 4 {
                assert_eq!(result.unwrap(), scenario == 0);
            } else {
                assert!(result.is_err());
            }
            assert_eq!(fs::read(&path).unwrap(), original);
        }
    }
}
