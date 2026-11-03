//! Native trajectory recorder. Use only a declared, disposable conformance fixture.
//! This uses ordinary authenticated gameplay; it cannot teleport or prepare a world.
use minecraft_sdk::*;
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !(args.len() == 4 || args.len() == 5) {
        return Err("Usage: continuous_physics CONNECTED_PROFILE ACTION.json DURATION_MS NEW_REPORT.json [CANCEL_AFTER_MS]".into());
    }
    let duration: u64 = args[2].parse()?;
    if !(500..=60_000).contains(&duration) {
        return Err("Duration must be 500..60000 ms".into());
    }
    let cancel_after: Option<u64> = args.get(4).map(|v| v.parse()).transpose()?;
    if cancel_after.is_some_and(|v| v >= duration) {
        return Err("Cancellation must precede the recording deadline".into());
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let request: ActionRequest = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    if !matches!(request.action, Action::StartMove { .. }) {
        return Err("A finite movement request is required".into());
    }
    request.action.validate()?;
    match client.action_status(&request.action_id).await {
        Err(e) if e.code == ErrorCode::NotFound => {},
        _ => return Err("Use a fresh action ID; uncertain requests must be inspected, never replayed by this probe".into()),
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args[3])?;
    let before = client.observe().await?;
    let start = Instant::now();
    let admitted = client.act(&request).await?;
    let mut samples = Vec::new();
    let mut cancellation: Option<Value> = None;
    while start.elapsed() < Duration::from_millis(duration) {
        if cancellation.is_none()
            && cancel_after.is_some_and(|v| start.elapsed() >= Duration::from_millis(v))
        {
            let cancel = ActionRequest {
                action_id: format!("{}-cancel", request.action_id),
                action: Action::CancelControl {
                    action_id: request.action_id.clone(),
                },
            };
            cancellation = Some(
                json!({"elapsed_ms":start.elapsed().as_millis(),"request":cancel,"receipt":client.act(&cancel).await}),
            );
        }
        let result = tokio::time::timeout(Duration::from_secs(2), client.observe()).await;
        let sample = match result {
            Ok(Ok(o)) => {
                json!({"elapsed_ms":start.elapsed().as_millis(),"revision":o.revision,"player":o.player,"sample_clock":o.world["sample_clock"]})
            }
            Ok(Err(e)) => json!({"elapsed_ms":start.elapsed().as_millis(),"error":e}),
            Err(_) => {
                json!({"elapsed_ms":start.elapsed().as_millis(),"error":"native observation timed out"})
            }
        };
        samples.push(sample);
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    let receipt = client.action_status(&request.action_id).await;
    let report = json!({"format":"continuous-physics-native-v1","instance_id":profile.instance_id,"bot_id":profile.credential.bot_id,"request":request,"before":before,"admitted":admitted,"cancellation":cancellation,"samples":samples,"final_receipt":receipt,"scope":"Declared native engine conformance; no autonomous competence claim"});
    output.write_all(&serde_json::to_vec_pretty(&report)?)?;
    output.sync_all()?;
    println!("Saved {} native samples to {}", samples.len(), args[3]);
    Ok(())
}
