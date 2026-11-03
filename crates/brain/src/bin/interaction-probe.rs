//! Container-local child/proxy integration diagnostic, never an intelligence benchmark.
use minecraft_sdk::{Action, ActionRequest, Client, Outcome, Profile};
use minecraft_sdk_brain::{Brain, Config, Result, ensure, private_read, tools};
use serde_json::json;
use std::io::Write;
use std::{fs, os::unix::fs::OpenOptionsExt, path::Path};

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "child" {
        let path = std::env::var_os("MC_SDK_PROFILE").ok_or("Engine-provided profile required")?;
        let profile: Profile = serde_json::from_slice(&private_read(Path::new(&path), 16384)?)?;
        let request: ActionRequest =
            serde_json::from_slice(&private_read(Path::new(&args[1]), 16384)?)?;
        let client = Client::with_profile(&profile)?;
        // Deliberately use act, not perform: this tests the protected program
        // boundary that previously returned premature Running experiment results.
        let receipt = client.act(&request).await?;
        println!(
            "{}",
            json!({"action_id":receipt.action_id,"outcome":receipt.outcome,"native_effect_verified":receipt.evidence["native_effect_verified"],"error":receipt.error})
        );
        ensure(
            receipt.outcome == Outcome::Observed
                && receipt.evidence["native_effect_verified"] == true,
            "Protected child did not receive verified completion",
        )?;
        return Ok(());
    }
    if args.len() != 2 {
        return Err(
            "interaction-probe PRIVATE_CONFIG.json PRIVATE_ACTION.json (inside isolated container)"
                .into(),
        );
    }
    let action: Action = serde_json::from_slice(&private_read(Path::new(&args[1]), 16384)?)?;
    action.validate()?;
    let mut cfg: Config = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    cfg.body.enabled = true;
    cfg.conversation = false;
    cfg.await_chat_goal = false;
    let mut brain = Brain::open(cfg).await?;
    let result=async {
        let request=ActionRequest{action_id:format!("interaction-probe-{}",brain.run_id),action};
        let request_file=brain.cfg.state.join("interaction-probe-request.json");
        let mut file=fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&request_file)?;
        file.write_all(&serde_json::to_vec(&request)?)?;file.sync_all()?;drop(file);
        let command=format!("{} child {}",quoted(&std::env::current_exe()?),quoted(&request_file));
        let outcome=tools::terminal(&mut brain,&command).await?;
        ensure(outcome["success"]==true,"Native child experiment failed; inspect retained output")?;
        let recorded=outcome["local_action_feedback"]["requests"].as_array().and_then(|rows|rows.iter().find(|row|row["request"]["action_id"]==request.action_id)).ok_or("Native action missing from local feedback")?;
        ensure(recorded["response"]["outcome"]=="observed" && recorded["response"]["native_effect_verified"]==true,"Final native result missing from experiment feedback")?;
        let observed=brain.client.observe().await?;
        let close=if let Some(id)=observed.menu["menu_id"].as_i64().filter(|id|*id!=0) {Some(brain.act(Action::CloseMenu{menu_id:i32::try_from(id)?}).await?)}else{None};
        brain.journal.event("interaction_probe_verified",json!({"request":request,"terminal":outcome,"close":close,"health":observed.player["health"],"body":brain.body.as_ref().map(|b|b.shared.view()),"models_called":0,"scope":"Host-directed container integration, not autonomous task success; local feedback is not authoritative"}))?;
        Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
    }.await;
    let stopped = if let Some(body) = brain.body.take() {
        body.stop().await
    } else {
        Ok(())
    };
    let disconnected = brain.client.disconnect().await;
    brain.journal.event("interaction_probe_exit",json!({"disconnected":disconnected.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}))?;
    result?;
    stopped?;
    disconnected?;
    Ok(())
}
