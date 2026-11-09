//! Paired native waypoint conformance diagnostic, not autonomous gameplay.
//! Operator supplies a connected, empty, sprinting Survival character on known
//! flat ground with >=11 clear blocks in +X and safe passive stopping space.
use minecraft_sdk::*;
use serde_json::json;
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn record(file: &mut std::fs::File, value: serde_json::Value) -> Result<()> {
    serde_json::to_writer(&mut *file, &value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}
fn position(o: &Observation) -> Result<[f64; 3]> {
    Ok([
        o.player["position"][0].as_f64().ok_or("x missing")?,
        o.player["position"][1].as_f64().ok_or("y missing")?,
        o.player["position"][2].as_f64().ok_or("z missing")?,
    ])
}
async fn execute(
    client: &Client,
    file: &mut std::fs::File,
    id: String,
    action: Action,
) -> Result<Receipt> {
    match client.action_status(&id).await {
        Err(e) if e.code == ErrorCode::NotFound => {}
        _ => return Err("Fresh IDs required; inspect existing work without replay".into()),
    }
    let request = ActionRequest {
        action_id: id,
        action,
    };
    let started = Instant::now();
    record(file, json!({"event":"dispatch","request":request}))?;
    let receipt = client.perform(&request, Duration::from_secs(8)).await?;
    record(
        file,
        json!({"event":"receipt","request":request,"receipt":receipt,"elapsed_ms":started.elapsed().as_millis()}),
    )?;
    Ok(receipt)
}
fn precise(target: [f64; 3]) -> Action {
    Action::StartMove {
        target: Position {
            x: target[0],
            y: target[1],
            z: target[2],
        },
        jump: false,
        max_ticks: 100,
    }
}
async fn reset(
    client: &Client,
    file: &mut std::fs::File,
    prefix: &str,
    origin: [f64; 3],
) -> Result<()> {
    // Use ordinary finite controls for repositioning; no console or teleport.
    for step in 0..8 {
        let p = position(&client.observe().await?)?;
        let dx = origin[0] - p[0];
        if dx.abs() < 0.08 && (p[2] - origin[2]).abs() < 0.08 {
            return Ok(());
        }
        let target = [p[0] + dx.clamp(-3., 3.), origin[1], origin[2]];
        let r = execute(
            client,
            file,
            format!("{prefix}-reset-{step}"),
            precise(target),
        )
        .await?;
        if r.outcome != Outcome::Observed {
            return Err("Reset failed; inspect its receipt".into());
        }
    }
    Err("Reset exceeded bounded path".into())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 || !valid_id(&args[1]) || args[1].len() > 60 {
        return Err(
            "Usage: waypoint_passage CONNECTED_PROFILE UNIQUE_PREFIX NEW_REPORT.jsonl".into(),
        );
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    let server = client.verify_instance(&profile.instance_id).await?;
    if !server
        .capabilities
        .iter()
        .any(|c| c == "minecraft:waypoint_passthrough")
    {
        return Err("Installed adapter does not advertise waypoint passage".into());
    }
    let before = client.observe().await?;
    if before.player["on_ground"] != true
        || before.player["sprinting"] != true
        || before.player["health"].as_f64() != Some(20.)
    {
        return Err("Fresh healthy grounded sprinting fixture required".into());
    }
    let origin = position(&before)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args[2])?;
    record(
        &mut file,
        json!({"event":"started","instance_id":profile.instance_id,"before":before,
        "scope":"Five paired identical 9m paths, three 3m waypoints. Passage versus stopped arrivals; not equal endpoint precision or hostile survival."}),
    )?;
    let mut stopped = Vec::new();
    let mut passage = Vec::new();
    for pair in 0..5 {
        let order = if pair % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for through in order {
            let prefix = format!("{}-{pair}-{through}", args[1]);
            reset(&client, &mut file, &prefix, origin).await?;
            let initial = client.observe().await?;
            let started = Instant::now();
            for step in 1..=3 {
                let target = [origin[0] + f64::from(step) * 3., origin[1], origin[2]];
                let action = if through {
                    Action::StartMoveThrough {
                        target: Position {
                            x: target[0],
                            y: target[1],
                            z: target[2],
                        },
                        max_ticks: 100,
                    }
                } else {
                    precise(target)
                };
                let r =
                    execute(&client, &mut file, format!("{prefix}-step-{step}"), action).await?;
                if r.outcome != Outcome::Observed
                    || r.evidence["native_effect_verified"] != true
                    || r.evidence["input_released"] != true
                {
                    return Err("Waypoint lacked native completion and release".into());
                }
            }
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let after = client.observe().await?;
            record(
                &mut file,
                json!({"event":"trial","pair":pair,"through":through,"elapsed_ms":elapsed_ms,"useful_blocks":9.,"before":initial,"after":after}),
            )?;
            if through {
                passage.push(elapsed_ms);
            } else {
                stopped.push(elapsed_ms);
            }
        }
    }
    reset(&client, &mut file, &format!("{}-expiry", args[1]), origin).await?;
    let expired = execute(
        &client,
        &mut file,
        format!("{}-expires", args[1]),
        Action::StartMoveThrough {
            target: Position {
                x: origin[0] + 3.,
                y: origin[1],
                z: origin[2],
            },
            max_ticks: 1,
        },
    )
    .await?;
    if expired.outcome != Outcome::Expired
        || expired.evidence["input_released"] != true
        || expired.evidence["native_effect_verified"] != false
    {
        return Err("Expiry did not release input without certifying passage".into());
    }
    reset(&client, &mut file, &format!("{}-cancel", args[1]), origin).await?;
    let action_id = format!("{}-cancelled-run", args[1]);
    let request = ActionRequest {
        action_id: action_id.clone(),
        action: Action::StartMoveThrough {
            target: Position {
                x: origin[0] + 3.,
                y: origin[1],
                z: origin[2],
            },
            max_ticks: 100,
        },
    };
    let admitted = client.act(&request).await?;
    record(
        &mut file,
        json!({"event":"cancel_run_admitted","request":request,"receipt":admitted}),
    )?;
    if admitted.outcome != Outcome::Running {
        return Err("Cancellation fixture not running".into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancel = execute(
        &client,
        &mut file,
        format!("{}-cancel-request", args[1]),
        Action::CancelControl {
            action_id: action_id.clone(),
        },
    )
    .await?;
    let cancelled = client
        .wait_for_action(&action_id, Duration::from_secs(4))
        .await?;
    record(
        &mut file,
        json!({"event":"cancelled","receipt":cancelled,"cancel":cancel}),
    )?;
    if cancelled.outcome != Outcome::Cancelled
        || cancelled.evidence["input_released"] != true
        || cancelled.evidence["native_effect_verified"] != false
    {
        return Err("Cancellation did not release input without certifying passage".into());
    }
    stopped.sort_unstable();
    passage.sort_unstable();
    let gain = stopped[2] as f64 / passage[2] as f64 - 1.;
    record(
        &mut file,
        json!({"event":"finished","stopped_ms":stopped,"passage_ms":passage,"median_useful_speed_gain":gain,"throughput_passed":gain>=0.30,"survival_proven":false}),
    )?;
    file.sync_all()?;
    if gain < 0.30 {
        return Err("Observed median useful speed gain below 30 percent".into());
    }
    Ok(())
}
