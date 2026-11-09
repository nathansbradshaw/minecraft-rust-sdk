//! Read-only hearing diagnostic, never a Survival or agent-performance claim.
use minecraft_sdk::{Client, Profile, auditory};
use minecraft_sdk_brain::{Result, digest, ensure, private_read, private_write};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure(
        matches!(args.len(), 2 | 3),
        "auditory-probe PRIVATE_CONNECTED_PROFILE.json SECONDS [PRIVATE_CURSOR.json]",
    )?;
    let seconds: u64 = args[1].parse()?;
    ensure(
        (1..=30).contains(&seconds),
        "Diagnostic duration must be1–30seconds",
    )?;
    let profile: Profile = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    ensure(
        profile.session.is_some(),
        "Connect first;caller retains session lifecycle ownership",
    )?;
    let client = Client::with_profile(&profile)?;
    let server = client.verify_instance(&profile.instance_id).await?;
    ensure(
        server
            .capabilities
            .iter()
            .any(|v| v == auditory::CAPABILITY),
        "Server does not advertise minecraft:auditory_observation",
    )?;
    let cursor_path = args.get(2).map(Path::new);
    let mut cursor: auditory::Cursor = match cursor_path {
        Some(path) if path.try_exists()? => serde_json::from_slice(&private_read(path, 16384)?)?,
        _ => auditory::Cursor::default(),
    };
    println!(
        "{}",
        json!({"event":"auditory_probe_started","instance_id":profile.instance_id,
        "duration_seconds":seconds,"capability":auditory::CAPABILITY,
        "scope":"Read-only listener-scoped observations;no gameplay,inference or Survival claim"})
    );
    let started = Instant::now();
    let deadline = tokio::time::Instant::from_std(started + Duration::from_secs(seconds));
    let mut samples = 0;
    let mut heard = 0;
    while tokio::time::Instant::now() < deadline {
        let observed = match tokio::time::timeout_at(deadline, client.observe()).await {
            Ok(result) => result?,
            Err(_) => break,
        };
        let frame = auditory::frame(&observed)?;
        let sounds = cursor.fresh(&observed)?;
        heard += sounds.len();
        samples += 1;
        if let Some(path) = cursor_path {
            private_write(path, &serde_json::to_vec(&cursor)?)?;
        }
        println!(
            "{}",
            json!({"event":"auditory_probe_observation","elapsed_ms":started.elapsed().as_millis(),
            "observation_sha256":digest(&serde_json::to_vec(&observed)?),"revision":observed.revision,
            "player_uuid":observed.player["uuid"],"player_health":observed.player["health"],"player_position":observed.player["position"],
            "game_time":observed.world["game_time"],"sample_clock":observed.world["sample_clock"],
            "auditory_epoch":frame.epoch,"latest_sequence":frame.latest_sequence,"dropped":frame.dropped,
            "retained_events":frame.events.len(),"fresh_events":sounds,
            "visible_entities":observed.world["entities"].as_array().into_iter().flatten().take(128)
                .map(|row|json!({"uuid":row["uuid"],"type":row["type"],"visible":row["visible"]})).collect::<Vec<_>>(),
            "scope":frame.scope})
        );
        if observed.player["health"].as_f64().is_some_and(|v| v <= 0.) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!(
        "{}",
        json!({"event":"auditory_probe_finished","samples":samples,"fresh_events":heard,
        "elapsed_ms":started.elapsed().as_millis(),"scope":"Observation integration only;caller must disconnect"})
    );
    Ok(())
}
