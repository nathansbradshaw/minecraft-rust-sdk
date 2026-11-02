use minecraft_sdk_brain::{Brain, Config, Journal, Result, private_read};
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!(
            "{}",
            serde_json::json!({"event":"brain_stopped","error":e.to_string()})
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "context" {
        let path = std::path::Path::new(&args[1]).join("brain.sqlite");
        if !path.is_file() {
            return Err("Existing brain state required".into());
        }
        println!(
            "{}",
            Journal::open(&path)?
                .get("last_model_context")?
                .ok_or("No model context recorded")?
        );
        return Ok(());
    }
    if args.len() != 2 || args[0] != "run" {
        return Err(
            "Usage: minecraft-sdk-brain run PRIVATE_CONFIG.json (inside isolated container)".into(),
        );
    }
    let config: Config =
        serde_json::from_slice(&private_read(std::path::Path::new(&args[1]), 16384)?)?;
    let mut brain = Brain::open(config).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = tokio::select! {
        result = brain.run() => result,
        _ = terminate.recv() => Ok(()),
        _ = tokio::signal::ctrl_c() => Ok(()),
    };
    // A bounded pilot ending must not leave its character unattended in Survival.
    // Pending IDs stay in the journal for explicit reconciliation on reconnect.
    let disconnected = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        brain.client.disconnect(),
    )
    .await;
    let clean = matches!(disconnected, Ok(Ok(())));
    brain.journal.event("controller_exit", serde_json::json!({"disconnected":clean,"pending_action":brain.journal.get("pending_action")?,"error":result.as_ref().err().map(ToString::to_string)}))?;
    result?;
    if !clean {
        return Err("Controller stopped but SDK disconnect was not confirmed".into());
    }
    Ok(())
}
