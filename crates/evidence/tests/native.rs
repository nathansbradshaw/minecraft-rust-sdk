use minecraft_sdk_core::{Action, ActionRequest, Outcome, Receipt};
use minecraft_sdk_evidence::{ExpectedAction, ResourceUsage, Spec, capture};
use rusqlite::{Connection, params};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
fn fixture() -> (tempfile::TempDir, Connection, Spec) {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("gateway.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE metadata(key TEXT,value TEXT);CREATE TABLE bots(id TEXT,uuid TEXT);CREATE TABLE actions(bot TEXT,id TEXT,intent TEXT,receipt TEXT);").unwrap();
    std::fs::set_permissions(
        dir.path().join("gateway.sqlite"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    db.execute(
        "INSERT INTO metadata VALUES('instance',?1)",
        ["f".repeat(64)],
    )
    .unwrap();
    db.execute_batch("INSERT INTO bots VALUES('actor-a','00000000-0000-0000-0000-000000000001');INSERT INTO bots VALUES('actor-b','00000000-0000-0000-0000-000000000002');").unwrap();
    let request = ActionRequest {
        action_id: "real-action-1".into(),
        action: Action::Look {
            yaw: 15.,
            pitch: 0.,
        },
    };
    let receipt = Receipt {
        action_id: request.action_id.clone(),
        outcome: Outcome::Observed,
        adapter_version: "fixture".into(),
        evidence: json!({"native_effect_verified":true,"before":{"player":{"yaw":0}},"after":{"player":{"yaw":15}}}),
        error: None,
    };
    db.execute(
        "INSERT INTO actions VALUES('actor-a',?1,?2,?3)",
        params![
            request.action_id,
            serde_json::to_string(&request.action).unwrap(),
            serde_json::to_string(&receipt).unwrap()
        ],
    )
    .unwrap();
    let spec = Spec {
        run_id: "g0-fixture".into(),
        goal: "Observe a native turn".into(),
        instance_id: "f".repeat(64),
        player_uuid: "00000000-0000-0000-0000-000000000001".into(),
        source_revision: "a".repeat(40),
        controller_sha256: "b".repeat(64),
        program_source_sha256: "c".repeat(64),
        program_binary_sha256: "d".repeat(64),
        trace_sha256: "e".repeat(64),
        model_requests: Default::default(),
        resources: ResourceUsage {
            measurement: "unmeasured".into(),
            container_memory_bytes: None,
            cpu_percent: None,
            pids: None,
            notes: "Fixture only".into(),
        },
        assists: vec![],
        interventions: vec![],
        failures: vec![],
        actions: vec![ExpectedAction {
            request,
            outcome: Outcome::Observed,
        }],
    };
    (dir, db, spec)
}
#[test]
fn captures_a_live_wal_journal_without_changing_its_actor_or_actions() {
    let (dir, db, spec) = fixture();
    let report = capture(&dir.path().join("gateway.sqlite"), &spec).unwrap();
    assert_eq!(report["verified_observed_actions"], 1);
    assert_eq!(
        report["native_actions"][0]["receipt"]["evidence"]["after"]["player"]["yaw"],
        15
    );
    assert!(
        report["authority"]
            .as_str()
            .unwrap()
            .contains("does not certify")
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM actions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn rejects_cross_actor_instance_intent_id_and_outcome_forgery() {
    let (dir, _, spec) = fixture();
    let path = dir.path().join("gateway.sqlite");
    let mut bad = spec.clone();
    bad.player_uuid = "00000000-0000-0000-0000-000000000002".into();
    assert!(capture(&path, &bad).is_err());
    let mut bad = spec.clone();
    bad.instance_id = "0".repeat(64);
    assert!(capture(&path, &bad).is_err());
    let mut bad = spec.clone();
    bad.actions[0].request.action_id = "invented-success".into();
    assert!(capture(&path, &bad).is_err());
    let mut bad = spec.clone();
    bad.actions[0].request.action = Action::Look {
        yaw: 90.,
        pitch: 0.,
    };
    assert!(capture(&path, &bad).is_err());
    let mut bad = spec;
    bad.actions[0].outcome = Outcome::Reconciled;
    assert!(capture(&path, &bad).is_err());
}
#[test]
fn unverified_native_effects_cannot_be_promoted_by_manifest_text() {
    let (dir, db, spec) = fixture();
    let mut receipt: serde_json::Value = serde_json::from_str(
        &db.query_row("SELECT receipt FROM actions", [], |r| r.get::<_, String>(0))
            .unwrap(),
    )
    .unwrap();
    receipt["evidence"]["native_effect_verified"] = json!(false);
    db.execute("UPDATE actions SET receipt=?1", [receipt.to_string()])
        .unwrap();
    assert!(capture(&dir.path().join("gateway.sqlite"), &spec).is_err());
    let mut forged = serde_json::to_value(&spec).unwrap();
    forged["completed"] = json!(true);
    assert!(serde_json::from_value::<Spec>(forged).is_err());
    let mut missing = spec;
    missing.trace_sha256.clear();
    assert!(missing.validate().is_err());
}
#[test]
fn reconciled_state_is_recordable_but_never_an_observed_effect() {
    let (dir, db, mut spec) = fixture();
    let receipt = Receipt {
        action_id: "real-action-1".into(),
        outcome: Outcome::Reconciled,
        adapter_version: "fixture".into(),
        evidence: json!({"current_state_only":true}),
        error: None,
    };
    db.execute(
        "UPDATE actions SET receipt=?1",
        [serde_json::to_string(&receipt).unwrap()],
    )
    .unwrap();
    spec.actions[0].outcome = Outcome::Reconciled;
    assert_eq!(
        capture(&dir.path().join("gateway.sqlite"), &spec).unwrap()["verified_observed_actions"],
        0
    );
}
#[test]
fn cli_writes_private_reports_and_refuses_overwrite_or_input_symlinks() {
    let (dir, _db, spec) = fixture();
    let input = dir.path().join("spec.json");
    std::fs::write(&input, serde_json::to_vec(&spec).unwrap()).unwrap();
    std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = dir.path().join("report.json");
    let execute = |input: &std::path::Path, output: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_mc-sdk-evidence"))
            .arg("capture")
            .arg(dir.path().join("gateway.sqlite"))
            .arg(input)
            .arg(output)
            .output()
            .unwrap()
    };
    let result = execute(&input, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::metadata(&output).unwrap().permissions().mode() & 0o077,
        0
    );
    let original = std::fs::read(&output).unwrap();
    assert!(!execute(&input, &output).status.success());
    assert_eq!(std::fs::read(&output).unwrap(), original);
    let linked = dir.path().join("linked.json");
    std::os::unix::fs::symlink(&input, &linked).unwrap();
    assert!(
        !execute(&linked, &dir.path().join("refused.json"))
            .status
            .success()
    );
    assert!(!dir.path().join("refused.json").exists());
}
