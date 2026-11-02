//! Ordinary non-operator peer for declared native chat/conversation tests.
use minecraft_sdk::{Action, Observation};
use minecraft_sdk_brain::{Brain, Config, Result, private_read};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};
fn received(o: &Observation) -> serde_json::Value {
    json!({"epoch":o.world["chat_epoch"],"messages":o.world["chat"],"dropped":o.world["chat_dropped"]})
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 || !matches!(args[1].as_str(), "chat" | "command") {
        return Err("peer-probe PRIVATE_CONFIG.json chat|command MESSAGE SECONDS".into());
    }
    let seconds: u64 = args[3].parse()?;
    if !(1..=600).contains(&seconds) {
        return Err("Finite peer duration required".into());
    }
    let mut cfg: Config = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    cfg.conversation = false;
    cfg.await_chat_goal = false;
    let mut brain = Brain::open(cfg).await?;
    let result = async {
        let action = if args[1] == "chat" {
            Action::Chat {
                message: args[2].clone(),
            }
        } else {
            Action::Command {
                command: args[2].clone(),
            }
        };
        let sent = brain.act(action).await?;
        brain.journal.event("peer_send", sent)?;
        let started = Instant::now();
        let mut last = serde_json::Value::Null;
        while started.elapsed() < Duration::from_secs(seconds) {
            brain.refresh().await?;
            let chat = received(&brain.observed);
            if chat != last {
                brain.journal.event("peer_received", chat.clone())?;
                last = chat;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;
    if let Some(body) = brain.body.take() {
        body.stop().await?;
    }
    let disconnected = brain.client.disconnect().await;
    brain.journal.event("peer_exit",json!({"disconnected":disconnected.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}))?;
    result?;
    disconnected?;
    Ok(())
}
