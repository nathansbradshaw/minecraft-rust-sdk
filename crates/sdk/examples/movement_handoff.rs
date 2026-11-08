//! Declared flat-ground conformance fixture, not an autonomous strategy.
//! Uses a connected disposable sprinting character; never prepares the world.
use minecraft_sdk::*;
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn record(file: &mut std::fs::File, event: Value) -> AnyResult<()> {
    serde_json::to_writer(&mut *file, &event)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}
fn position(o: &Observation) -> AnyResult<[f64; 3]> {
    Ok([
        o.player["position"][0].as_f64().ok_or("x missing")?,
        o.player["position"][1].as_f64().ok_or("y missing")?,
        o.player["position"][2].as_f64().ok_or("z missing")?,
    ])
}
#[tokio::main]
async fn main() -> AnyResult<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 || !valid_id(&args[1]) || args[1].len() > 80 {
        return Err(
            "Usage: movement_handoff CONNECTED_PROFILE UNIQUE_PREFIX NEW_REPORT.jsonl".into(),
        );
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let before = client.observe().await?;
    if before.player["on_ground"] != true || before.player["sprinting"] != true {
        return Err("Grounded sprinting character required".into());
    }
    let origin = position(&before)?;
    let run = ActionRequest {
        action_id: format!("{}-run", args[1]),
        action: Action::StartMove {
            target: Position {
                x: origin[0] + 3.,
                y: origin[1],
                z: origin[2],
            },
            jump: false,
            max_ticks: 100,
        },
    };
    let cancel = ActionRequest {
        action_id: format!("{}-cancel", args[1]),
        action: Action::CancelControl {
            action_id: run.action_id.clone(),
        },
    };
    let hop = ActionRequest {
        action_id: format!("{}-hop", args[1]),
        action: Action::StartMove {
            target: Position {
                x: origin[0] + 3.8,
                y: origin[1],
                z: origin[2],
            },
            jump: true,
            max_ticks: 100,
        },
    };
    for request in [&run, &cancel, &hop] {
        match client.action_status(&request.action_id).await {
            Err(e) if e.code == ErrorCode::NotFound => {}
            _ => {
                return Err(
                    "Fresh IDs required; inspect interrupted work instead of replaying".into(),
                );
            }
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args[2])?;
    record(
        &mut file,
        json!({"event":"fixture_started","before":before,"instance_id":profile.instance_id,"scope":"run-up then explicit cancel/hop handoff; not seamless full-speed retargeting"}),
    )?;
    let started = Instant::now();
    let admitted = client.act(&run).await?;
    record(
        &mut file,
        json!({"event":"run_admitted","request":run,"receipt":admitted}),
    )?;
    if admitted.outcome != Outcome::Running {
        return Err("Run-up was not admitted".into());
    }
    while started.elapsed() < Duration::from_millis(300) {
        let o = client.observe().await?;
        record(
            &mut file,
            json!({"event":"run_sample","elapsed_ms":started.elapsed().as_millis(),"observation":o}),
        )?;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let released = client.perform(&cancel, Duration::from_secs(2)).await?;
    record(
        &mut file,
        json!({"event":"run_released","receipt":released}),
    )?;
    if released.outcome != Outcome::Observed || released.evidence["native_effect_verified"] != true
    {
        return Err("Run-up release was not natively verified".into());
    }
    let handoff = client.observe().await?;
    record(
        &mut file,
        json!({"event":"handoff_observation","observation":handoff}),
    )?;
    if position(&handoff)?[0] - origin[0] < 0.15 || handoff.player["on_ground"] != true {
        return Err("Native grounded run-up displacement missing".into());
    }
    let admitted = client.act(&hop).await?;
    record(
        &mut file,
        json!({"event":"hop_admitted","request":hop,"receipt":admitted}),
    )?;
    if admitted.outcome != Outcome::Running {
        return Err("Hop was not admitted".into());
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let o = client.observe().await?;
        record(&mut file, json!({"event":"hop_sample","observation":o}))?;
        let receipt = client.action_status(&hop.action_id).await?;
        if receipt.outcome != Outcome::Running {
            record(&mut file, json!({"event":"hop_finished","receipt":receipt}))?;
            file.sync_all()?;
            if receipt.outcome != Outcome::Observed
                || receipt.evidence["native_effect_verified"] != true
            {
                return Err("Hop did not complete with native evidence".into());
            }
            println!("Run-up, released-input handoff and native hop completion recorded");
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("Inspect the original hop ID; recording deadline reached".into());
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}
