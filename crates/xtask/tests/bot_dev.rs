use serde_json::Value;
use std::{fs, process::Command};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_bot-dev")
}
fn report(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path.join("summary.json")).unwrap()).unwrap()
}

#[test]
fn replay_returns_success_only_for_the_expected_reproduced_failure() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("new-parent/nested/replay");
    let output = Command::new(binary())
        .args(["replay", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["reproduced"], true);
    assert_eq!(result["diagnosis"]["summed_model_ms"], 285186);
}

#[test]
fn launches_expected_failure_and_preserves_diagnostics_and_private_files() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("capture");
    let status = Command::new(binary())
        .args([
            "capture",
            path.to_str().unwrap(),
            "5",
            "--",
            binary(),
            "fixture-worker",
        ])
        .status()
        .unwrap();
    assert!(!status.success());
    let captured = report(&path);
    assert_eq!(captured["exit_code"], 2);
    assert_eq!(captured["timed_out"], false);
    let diagnosis: Value =
        serde_json::from_slice(&fs::read(path.join("stdout.log")).unwrap()).unwrap();
    assert_eq!(diagnosis["repeated_menu_results"], 17);
    assert_eq!(captured["artifacts"][0]["file"], "diagnosis.json");
    let bytes = fs::read(path.join("artifacts/diagnosis.json")).unwrap();
    assert_eq!(
        captured["artifacts"][0]["sha256"],
        minecraft_sdk_xtask::workflow::digest(&bytes)
    );
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    assert_eq!(
        fs::metadata(path.join("stdout.log"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    // A repeated destination must fail without replacing original evidence.
    assert!(
        !Command::new(binary())
            .args(["replay", path.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(captured, report(&path));
}

#[test]
fn interruption_preserves_evidence_and_stops_descendants() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("interrupt");
    let heartbeat = root.path().join("heartbeat");
    let mut parent = Command::new(binary())
        .env("BOT_DEV_TEST_FIXTURE", "1")
        .env("BOT_DEV_HEARTBEAT", &heartbeat)
        .args([
            "capture",
            path.to_str().unwrap(),
            "5",
            "--",
            binary(),
            "fixture-process-tree",
        ])
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !heartbeat.exists() && started.elapsed() < std::time::Duration::from_secs(3) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(heartbeat.exists());
    assert_eq!(unsafe { libc::kill(parent.id() as i32, libc::SIGTERM) }, 0);
    assert!(!parent.wait().unwrap().success());
    let captured = report(&path);
    assert_eq!(captured["interrupted"], true);
    let before = fs::metadata(&heartbeat).unwrap().len();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(before, fs::metadata(&heartbeat).unwrap().len());
}

#[test]
fn spawn_failure_is_saved_and_returns_nonzero() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing");
    assert!(
        !Command::new(binary())
            .args([
                "capture",
                path.to_str().unwrap(),
                "1",
                "--",
                "/no-such-bot-dev-program"
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(report(&path)["spawn_error"].is_string());
}

#[test]
fn output_threshold_and_artifact_symlinks_fail_with_saved_evidence() {
    let root = tempfile::tempdir().unwrap();
    for fixture in ["fixture-output", "fixture-artifact-link"] {
        let path = root.path().join(fixture);
        let output = Command::new(binary())
            .env("BOT_DEV_TEST_FIXTURE", "1")
            .args([
                "capture",
                path.to_str().unwrap(),
                "5",
                "--",
                binary(),
                fixture,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let captured = report(&path);
        if fixture == "fixture-output" {
            assert_eq!(captured["output_limit"], true);
            assert!(captured["stdout"]["bytes"].as_u64().unwrap() > 8 * 1024 * 1024);
        } else {
            assert!(!captured["artifact_errors"].as_array().unwrap().is_empty());
            assert_eq!(captured["artifacts"], serde_json::json!([]));
        }
    }
}

#[test]
fn timeout_cleans_up_the_owned_process_group() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("timeout");
    let heartbeat = root.path().join("heartbeat");
    assert!(
        !Command::new(binary())
            .env("BOT_DEV_TEST_FIXTURE", "1")
            .env("BOT_DEV_HEARTBEAT", &heartbeat)
            .args([
                "capture",
                path.to_str().unwrap(),
                "1",
                "--",
                binary(),
                "fixture-process-tree"
            ])
            .status()
            .unwrap()
            .success()
    );
    let captured = report(&path);
    assert_eq!(captured["timed_out"], true);
    let pid = captured["pid"].as_u64().unwrap() as i32;
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "owned child was not reaped"
    );
    let before = fs::metadata(&heartbeat).unwrap().len();
    assert!(before > 0, "descendant never started");
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        before,
        fs::metadata(&heartbeat).unwrap().len(),
        "descendant still running"
    );
}
