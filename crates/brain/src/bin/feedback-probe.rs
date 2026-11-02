//! Container-only feedback/cache integration diagnostic, not a gameplay benchmark.
use minecraft_sdk::{Action, Outcome};
use minecraft_sdk_brain::{Brain, Config, Result, digest, ensure, private_read, tools};
use serde_json::json;
use std::{fs, path::Path};

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.as_slice() == ["emit"] {
        println!("{}\nLATE_STDOUT_EVIDENCE", "x".repeat(20000));
        eprintln!("{}\nLATE_RUNTIME_FAILURE", "y".repeat(16000));
        std::process::exit(23);
    }
    if args.len() != 2 {
        return Err("feedback-probe PRIVATE_CONFIG.json OWN_COMPLETED_ACTION_ID".into());
    }
    let mut cfg: Config = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    cfg.body.enabled = true;
    cfg.conversation = false;
    cfg.await_chat_goal = false;
    let mut brain = Brain::open(cfg).await?;
    let result = async {
        let terminal = tools::terminal(&mut brain, &format!("{} emit", quoted(&std::env::current_exe()?))).await?;
        ensure(terminal["exit_code"] == 23 && terminal["success"] == false, "Declared runtime failure was not returned")?;
        for (stream, marker) in [("stdout", "LATE_STDOUT_EVIDENCE"), ("stderr", "LATE_RUNTIME_FAILURE")] {
            let path = Path::new(terminal["output_files"][stream]["path"].as_str().ok_or("Full output missing")?);
            let bytes = fs::read(path)?;
            ensure(bytes.len() > 8000 && digest(&bytes) == terminal["output_files"][stream]["sha256"], "Saved output is incomplete or hash differs")?;
            let retrieved = tools::terminal(&mut brain, &format!("tail -n 1 {}", quoted(path))).await?;
            ensure(retrieved["success"] == true && retrieved["stdout"].as_str().is_some_and(|s| s.contains(marker)), "Late feedback was not retrievable through the terminal")?;
        }
        let receipt = brain.inspect(&args[1]).await?;
        ensure(receipt.outcome.terminal(), "Diagnostic requires an already completed own action")?;
        brain.journal.set("pending_action", &args[1])?;
        let next = brain.act(Action::Sprint { enabled: false }).await?;
        ensure(next["receipt"]["outcome"] == serde_json::to_value(Outcome::Observed)? && next["receipt"]["evidence"]["native_effect_verified"] == true && brain.journal.get("pending_action")?.as_deref() == Some(""), "Completed intention still blocks the next mutation")?;
        brain.journal.event("feedback_probe_verified", json!({"complete_output_files":terminal["output_files"],"late_feedback_retrieved":true,"old_completed_action":args[1],"new_action":next["receipt"],"health":brain.observed.player["health"],"models_called":0,"scope":"Container integration only; no collection, navigation or intelligence claim"}))?;
        Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
    }.await;
    let stopped = if let Some(body) = brain.body.take() {
        body.stop().await
    } else {
        Ok(())
    };
    let disconnected = brain.client.disconnect().await;
    brain.journal.event("feedback_probe_exit",json!({"disconnected":disconnected.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}))?;
    result?;
    stopped?;
    disconnected?;
    Ok(())
}
