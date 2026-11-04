//! Real subprocess boundaries; no Minecraft connection or provider is opened.
use minecraft_sdk_brain::{Config, Journal, worker};
use serde_json::json;
use std::{
    fs,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::Path,
};
fn fixture() -> (tempfile::TempDir, Config, Journal) {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("private-worker.bin");
    fs::copy(env!("CARGO_BIN_EXE_decision-probe"), &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let cfg=serde_json::from_value(json!({"profile":dir.path().join("unused-profile"),"model_key":dir.path().join("unused-key"),"model_endpoint":"https://example.invalid/","workspace":dir.path(),"state":dir.path(),"goal":"fixture","decision_worker":executable})).unwrap();
    let journal = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
    (dir, cfg, journal)
}
#[tokio::test]
async fn subprocess_success_failure_and_identity_are_distinct_and_bounded() {
    let (dir, cfg, journal) = fixture();
    let (choice, usage) = worker::choose(&cfg, &journal, &json!({"goal":"fixture"}), false, false)
        .await
        .unwrap();
    assert_eq!(choice.tool, "observe");
    assert_eq!(usage["models_called"], 0);
    fs::write(
        dir.path().join(".worker-probe.json"),
        r#"{"wrong_identity":true}"#,
    )
    .unwrap();
    assert!(
        worker::choose(&cfg, &journal, &json!({}), false, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("identity")
    );
    fs::write(dir.path().join(".worker-probe.json"), r#"{"fail":true}"#).unwrap();
    assert!(
        worker::choose(&cfg, &journal, &json!({}), false, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("unsuccessfully")
    );
    fs::remove_file(dir.path().join(".worker-probe.json")).unwrap();
    assert!(
        worker::choose(&cfg, &journal, &json!({}), false, false)
            .await
            .is_ok(),
        "A process failure cannot poison a later changed invocation"
    );
}
#[tokio::test]
async fn frame_and_retry_metadata_do_not_expand_context_or_allowances() {
    let (dir, cfg, journal) = fixture();
    assert!(
        worker::choose(
            &cfg,
            &journal,
            &json!({"text":"x".repeat(32768)}),
            false,
            false
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("context")
    );
    let retry = minecraft_sdk_brain::now() + 60;
    fs::write(
        dir.path().join(".worker-probe.json"),
        json!({"error":{"message":"allocation_daily_limit","retry_at":retry,"daily_reset":retry}})
            .to_string(),
    )
    .unwrap();
    let error = worker::choose(&cfg, &journal, &json!({}), true, true)
        .await
        .unwrap_err();
    assert_eq!(
        minecraft_sdk_brain::model::retry_at(error.as_ref()),
        Some(retry)
    );
    fs::write(
        dir.path().join(".worker-probe.json"),
        json!({"error":{"message":"invalid","retry_at":retry+86400,"daily_reset":retry+86400}})
            .to_string(),
    )
    .unwrap();
    assert!(
        worker::choose(&cfg, &journal, &json!({}), true, true)
            .await
            .unwrap_err()
            .to_string()
            .contains("metadata")
    );
}
#[tokio::test]
async fn activation_preserves_native_state_and_rejects_invalid_or_mutated_images() {
    let (dir, mut cfg, journal) = fixture();
    let candidate = cfg.decision_worker.clone().unwrap();
    cfg.decision_worker = Some(dir.path().join("missing-initial-worker"));
    let mut native_lock = fs::File::create(dir.path().join("controller.lock")).unwrap();
    assert_eq!(
        unsafe { libc::flock(native_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    std::io::Write::write_all(&mut native_lock, b"resident ownership witness").unwrap();
    journal
        .event(
            "native_witness",
            json!({"pending_action":"retained-id","session":"unchanged"}),
        )
        .unwrap();
    let result = worker::replace(&cfg, &candidate).unwrap();
    assert_eq!(result["selected"], true);
    assert_eq!(result["native_connections_opened"], 0);
    assert_eq!(result["controller_lock_acquired"], false);
    assert_eq!(
        fs::read(dir.path().join("controller.lock")).unwrap(),
        b"resident ownership witness"
    );
    let before = fs::read(dir.path().join("decision-worker.json")).unwrap();
    assert!(worker::replace(&cfg, &dir.path().join("missing-new-worker")).is_err());
    assert_eq!(
        fs::read(dir.path().join("decision-worker.json")).unwrap(),
        before
    );
    assert!(
        worker::choose(&cfg, &journal, &json!({}), false, false)
            .await
            .is_ok()
    );
    let manifest: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let image = Path::new(manifest["executable"].as_str().unwrap());
    fs::set_permissions(image, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(image, b"corrupt image").unwrap();
    assert!(
        worker::choose(&cfg, &journal, &json!({}), false, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("hash")
    );
}
#[tokio::test]
async fn cancelling_a_stalled_worker_reaps_its_process_group() {
    let (dir, cfg, journal) = fixture();
    fs::write(
        dir.path().join(".worker-probe.json"),
        r#"{"delay_ms":10000}"#,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(150),
            worker::choose(&cfg, &journal, &json!({}), false, false)
        )
        .await
        .is_err()
    );
    let db = rusqlite::Connection::open(dir.path().join("brain.sqlite")).unwrap();
    let pid:i32=db.query_row("SELECT json_extract(data,'$.pid') FROM events WHERE kind='decision_worker_started' ORDER BY id DESC LIMIT 1",[],|r|r.get(0)).unwrap();
    for _ in 0..100 {
        if unsafe { libc::kill(-pid, 0) } < 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("Cancelled worker process group survived");
}
#[test]
fn builtin_worker_returns_a_bound_error_without_opening_native_or_model_connections() {
    use std::io::Write;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_minecraft-sdk-brain"))
        .arg("decision-worker")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(json!({"version":1,"request_id":"missing-key-fixture","context":{},"endpoint":"https://example.invalid/","key_file":"/definitely-missing-worker-model-key","use_sol":false,"development":false}).to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["request_id"], "missing-key-fixture");
    assert!(response["answer"].is_null());
    assert!(response["error"]["message"].is_string());
    assert!(response["error"]["retry_at"].is_null());
}
