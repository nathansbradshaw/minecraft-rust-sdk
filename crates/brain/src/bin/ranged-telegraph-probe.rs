//! Bounded read-only native observation diagnostic. Never claims Survival.
use minecraft_sdk::{Client, Profile};
use minecraft_sdk_brain::{Result, digest, ensure, private_read};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure(
        args.len() == 3,
        "ranged-telegraph-probe PRIVATE_CONNECTED_PROFILE.json SECONDS ENTITY_UUID",
    )?;
    let seconds: u64 = args[1].parse()?;
    ensure(
        (1..=30).contains(&seconds),
        "Diagnostic duration must be1–30seconds",
    )?;
    ensure(
        args[2].len() == 36 && args[2].bytes().all(|v| v.is_ascii_hexdigit() || v == b'-'),
        "Entity UUID required",
    )?;
    let profile: Profile = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    ensure(
        profile.session.is_some(),
        "Connect with mc-sdk first; caller retains session lifecycle ownership",
    )?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let server = client.discover().await?;
    ensure(
        server
            .capabilities
            .iter()
            .any(|v| v == "minecraft:visible_item_use"),
        "Server does not advertise minecraft:visible_item_use",
    )?;
    println!(
        "{}",
        json!({"event":"ranged_probe_started","instance_id":profile.instance_id,
        "target_uuid":args[2],"duration_seconds":seconds,"capability":"minecraft:visible_item_use",
        "scope":"Read-only SDK observations, no movement, targeting, healing, inference or Survival claim"})
    );
    let started = Instant::now();
    let deadline = tokio::time::Instant::from_std(started + Duration::from_secs(seconds));
    let mut samples = 0;
    while started.elapsed() < Duration::from_secs(seconds) {
        let observed = match tokio::time::timeout_at(deadline, client.observe()).await {
            Ok(result) => result?,
            Err(_) => {
                println!(
                    "{}",
                    json!({"event":"ranged_probe_read_timeout","scope":"Declared deadline ended observation wait;no gameplay mutation"})
                );
                break;
            }
        };
        let entity = observed.world["entities"].as_array().and_then(|rows| {
            rows.iter().find(|row| {
                row["visible"] == true && row["uuid"].as_str() == Some(args[2].as_str())
            })
        });
        let projectiles: Vec<_> = observed.world["entities"].as_array().into_iter().flatten()
            .filter(|row| row["visible"] == true && row["projectile"] == true)
            .take(16).map(|row| json!({"uuid":row["uuid"],"type":row["type"],
                "owner_uuid":row["owner_uuid"],"position":row["position"],"velocity":row["velocity"]})).collect();
        println!(
            "{}",
            json!({"event":"ranged_probe_observation","elapsed_ms":started.elapsed().as_millis(),
            "observation_sha256":digest(&serde_json::to_vec(&observed)?),"revision":observed.revision,
            "player_uuid":observed.player["uuid"],"player_health":observed.player["health"],
            "game_time":observed.world["game_time"],"sample_clock":observed.world["sample_clock"],
            "target_observed":entity.is_some(),"target":entity.map(|row|json!({"uuid":row["uuid"],
                "type":row["type"],"position":row["position"],"main_hand":row["main_hand"],"item_use":row["item_use"]})),
            "visible_projectiles":projectiles})
        );
        samples += 1;
        if observed.player["health"].as_f64().is_some_and(|v| v <= 0.) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!(
        "{}",
        json!({"event":"ranged_probe_finished","samples":samples,
        "elapsed_ms":started.elapsed().as_millis(),"scope":"Observational integration only;caller must disconnect"})
    );
    Ok(())
}
