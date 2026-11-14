//! Operator CLI entry points only; no profile, broker or Minecraft session.
use minecraft_sdk_brain::Journal;
use serde_json::{Value, json};
use std::fs;
use tokio::process::Command;

#[tokio::test]
async fn both_existing_binaries_page_owned_journals_without_controller_startup_or_writes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private journal.sqlite");
    let journal = Journal::open(&path).unwrap();
    journal
        .event(
            "fixture",
            json!({"action_id":"original-control","health":7}),
        )
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    for executable in [
        env!("CARGO_BIN_EXE_minecraft-sdk-brain"),
        env!("CARGO_BIN_EXE_body-probe"),
    ] {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Command::new(executable)
                .arg("journal-events")
                .arg(&path)
                .args(["0", "1", "4096"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.len() <= 4096);
        let page: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(page["rows"][0]["id"], 1);
        let data: Value =
            serde_json::from_str(page["rows"][0]["data_json"].as_str().unwrap()).unwrap();
        assert_eq!(data["action_id"], "original-control");
        assert_eq!(page["models_called"], 0);
        assert_eq!(page["native_connections_opened"], 0);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let missing = root.path().join("never-created.sqlite");
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Command::new(executable)
                .arg("journal-events")
                .arg(&missing)
                .arg("0")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!output.status.success());
        assert!(!missing.exists());
    }
    assert!(!root.path().join("controller.lock").exists());
    assert!(!root.path().join("actors").exists());
}
