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
    if config.conversation && !config.body.enabled {
        return Err("Independent conversation requires the responsive body".into());
    }
    let mut brain = Brain::open(config).await?;
    let conversation = if brain.cfg.conversation {
        match minecraft_sdk_brain::conversation::Handle::start(
            brain.cfg.clone(),
            brain
                .body
                .as_ref()
                .ok_or("Independent conversation requires the responsive body")?
                .shared
                .clone(),
        ) {
            Ok(handle) => Some(handle),
            Err(e) => {
                if let Some(body) = brain.body.take() {
                    let _ = body.stop().await;
                }
                let _ = brain.client.disconnect().await;
                return Err(e);
            }
        }
    } else {
        None
    };
    let body = brain.body.as_ref().map(|h| h.shared.clone());
    let conversation_failed = conversation.as_ref().map(|h| h.failed.clone());
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = tokio::select! {
        result = brain.run() => result,
        _ = terminate.recv() => Ok(()),
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = async {if let Some(body)=body {body.failed.notified().await;}else{std::future::pending::<()>().await;}} => Err("Background Survival controller stopped".into()),
        _ = async {if let Some(failed)=conversation_failed {failed.notified().await;}else{std::future::pending::<()>().await;}} => Err("Independent conversation controller stopped".into()),
    };
    // A bounded pilot ending must not leave its character unattended in Survival.
    // Pending IDs stay in the journal for explicit reconciliation on reconnect.
    let conversation_stopped = if let Some(conversation) = conversation {
        conversation.stop().await
    } else {
        Ok(())
    };
    let body_stopped = if let Some(body) = brain.body.take() {
        body.stop().await
    } else {
        Ok(())
    };
    let disconnected = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        brain.client.disconnect(),
    )
    .await;
    let clean = matches!(disconnected, Ok(Ok(())));
    brain.journal.event("controller_exit", serde_json::json!({"disconnected":clean,"pending_action":brain.journal.get("pending_action")?,"error":result.as_ref().err().map(ToString::to_string)}))?;
    result?;
    conversation_stopped?;
    body_stopped?;
    if !clean {
        return Err("Controller stopped but SDK disconnect was not confirmed".into());
    }
    Ok(())
}
