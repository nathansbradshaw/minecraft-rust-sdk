use minecraft_sdk_brain::{Brain, Config, Journal, Result, private_read};
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!(
            "{}",
            serde_json::json!({"event":if std::env::args().nth(1).as_deref()==Some("decision-worker"){"decision_worker_stopped"}else{"brain_stopped"},"error":e.to_string()})
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "decision-worker" {
        return minecraft_sdk_brain::worker::serve().await;
    }
    if args.len() == 3 && args[0] == "replace-worker" {
        let cfg: Config =
            serde_json::from_slice(&private_read(std::path::Path::new(&args[1]), 16384)?)?;
        println!(
            "{}",
            minecraft_sdk_brain::worker::replace(&cfg, std::path::Path::new(&args[2]))?
        );
        return Ok(());
    }
    if args.len() == 2 && args[0] == "navigation-worker" {
        return minecraft_sdk_brain::navigation::worker(std::path::Path::new(&args[1])).await;
    }
    if args.len() == 2 && args[0] == "container-memory" {
        let cfg: Config =
            serde_json::from_slice(&private_read(std::path::Path::new(&args[1]), 16384)?)?;
        let profile: minecraft_sdk::Profile =
            serde_json::from_slice(&private_read(&cfg.profile, 16384)?)?;
        let journal = Journal::open(&cfg.state.join("brain.sqlite"))?;
        let imported = journal
            .import_container_history(&profile.instance_id, &profile.credential.player_uuid)?;
        let context: serde_json::Value = journal
            .get("last_model_context")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default();
        let observation = minecraft_sdk::Observation {
            revision: "historical-context".into(),
            player: serde_json::json!({"uuid":profile.credential.player_uuid,"dimension":context["observation"]["player"]["dimension"]}),
            world: context["observation"]["world"].clone(),
            menu: serde_json::Value::Null,
        };
        println!(
            "{}",
            serde_json::json!({"imported":imported,"containers":journal.recalled_containers(&profile.instance_id,&observation)?,"scope":"historical local memory; visibility from last saved context, not a current observation","native_connections_opened":0,"models_called":0})
        );
        return Ok(());
    }
    if args.len() == 2 && args[0] == "memory-index" {
        let state = std::path::Path::new(&args[1]);
        let path = state.join("brain.sqlite");
        if !path.is_file() {
            return Err("Existing brain state required".into());
        }
        let count = Journal::open(&path)?.import_program_history(state)?;
        println!(
            "{}",
            serde_json::json!({"indexed_historical_files":count,"models_called":0,"native_connections_opened":0,"success_granted":false})
        );
        return Ok(());
    }
    if args.len() == 3 && args[0] == "memory-search" {
        let state = std::path::Path::new(&args[1]);
        let path = state.join("brain.sqlite");
        if !path.is_file() {
            return Err("Existing brain state required".into());
        }
        println!(
            "{}",
            Journal::open(&path)?.recall(args[2].to_str().ok_or("UTF-8 query required")?)?
        );
        return Ok(());
    }
    if args.len() == 3 && args[0] == "skill-inspect" {
        println!(
            "{}",
            minecraft_sdk_brain::versions::inspect(
                std::path::Path::new(&args[1]),
                args[2].to_str().ok_or("UTF-8 version required")?
            )?
        );
        return Ok(());
    }
    if args.len() == 4 && args[0] == "skill-restore" {
        println!(
            "{}",
            minecraft_sdk_brain::versions::restore(
                std::path::Path::new(&args[1]),
                std::path::Path::new(&args[2]),
                args[3].to_str().ok_or("UTF-8 version required")?
            )?
        );
        return Ok(());
    }
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
    if args.len() != 2 || (args[0] != "run" && args[0] != "reconcile-pending") {
        return Err(
            "Usage: minecraft-sdk-brain run|reconcile-pending PRIVATE_CONFIG.json (inside isolated container)".into(),
        );
    }
    let reconcile_only = args[0] == "reconcile-pending";
    let config: Config =
        serde_json::from_slice(&private_read(std::path::Path::new(&args[1]), 16384)?)?;
    if config.conversation && !config.body.enabled {
        return Err("Independent conversation requires the responsive body".into());
    }
    let mut brain = Brain::open(config).await?;
    let conversation = if brain.cfg.conversation && !reconcile_only {
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
        result = async {
            if reconcile_only {
                let id=brain.journal.get("pending_action")?.unwrap_or_default();
                minecraft_sdk_brain::ensure(!id.is_empty(),"No pending local intention to reconcile")?;
                let result=brain.settle(&id).await?;
                println!("{}",serde_json::json!({"event":"pending_reconciliation_result","result":result,"model_requests":0,"mutation_replayed":false}));
                Ok(())
            } else {brain.run().await}
        } => result,
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
