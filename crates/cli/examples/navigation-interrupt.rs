//! Declared no-model integration diagnostic. Uses one existing controller session.
use minecraft_sdk::{
    navigation::{self, Goal, Options},
    *,
};
use serde_json::json;
use std::{fs, path::Path, time::Duration};

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("navigation-interrupt PRIVATE_PROFILE GOAL.json NEW_NAV_JOURNAL.json".into());
    }
    let meta = fs::symlink_metadata(&args[0])?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 16384 {
        return Err("Private bounded profile required".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Private profile permissions required".into());
        }
    }
    let profile: Profile = serde_json::from_slice(&fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let goal: Goal = serde_json::from_slice(&fs::read(&args[1])?)?;
    let path = Path::new(&args[2]);
    if path.exists() {
        return Err("Use a new diagnostic journal; preserve earlier attempts".into());
    }
    let options = Options {
        max_steps: 64,
        timeout_ms: 120000,
        ..Options::default()
    };
    let mut running = Box::pin(navigation::navigate(
        &client,
        goal.clone(),
        path,
        options.clone(),
    ));
    let watch = async {
        loop {
            if let Ok(bytes) = fs::read(path) {
                if let Ok(state) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(id) = state["pending"]["request"]["action_id"].as_str() {
                        if let Ok(r) = client.action_status(id).await {
                            if r.outcome == Outcome::Running
                                && r.evidence["progress"]["frames_sent"].as_u64().unwrap_or(0) > 0
                            {
                                return Ok::<_, Box<dyn std::error::Error>>(id.to_owned());
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let id = tokio::select! {biased;
        result=tokio::time::timeout(Duration::from_secs(15),watch)=>result??,
        result=&mut running=>return Err(format!("Navigation ended before a running intention could be interrupted: {result:?}").into()),
    };
    drop(running); // Drop the execution future, retaining its durable intention.
    let cancel_id = format!("cancel-{id}");
    let mut cancel = client
        .act(&ActionRequest {
            action_id: cancel_id.clone(),
            action: Action::CancelControl {
                action_id: id.clone(),
            },
        })
        .await?;
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while !cancel.outcome.terminal() {
        if tokio::time::Instant::now() > until {
            return Err("Cancellation did not finish within the diagnostic bound".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel = client.action_status(&cancel_id).await?;
    }
    let before = client.action_status(&id).await?;
    if cancel.outcome != Outcome::Observed
        || before.outcome != Outcome::Cancelled
        || before.evidence["input_released"] != true
    {
        return Err(
            "The declared fixture did not cancel a running movement and release its input".into(),
        );
    }
    println!(
        "{}",
        json!({"event":"navigation_interrupted","action_id":id,"outcome":before.outcome,"input_released":before.evidence["input_released"],"cancel_action_id":cancel_id,"models_called":0})
    );
    let resumed = navigation::navigate(&client, goal, path, options).await?;
    let after = client.action_status(&id).await?;
    if serde_json::to_value(&before)? != serde_json::to_value(after)? {
        return Err("Old terminal receipt changed during resume".into());
    }
    if resumed.status != "reached" {
        return Err(format!("Resumed navigation did not reach its goal: {resumed:?}").into());
    }
    println!(
        "{}",
        json!({"event":"navigation_resumed","report":resumed,"old_receipt_unchanged":true,"models_called":0,"scope":"Declared interruption/cancellation fixture; not autonomous gameplay"})
    );
    Ok(())
}
