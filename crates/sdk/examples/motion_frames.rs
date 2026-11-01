//! Run only with an explicitly connected disposable character on flat ground.
use minecraft_sdk::*;
use serde_json::json;
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[tokio::main]
async fn main() -> AnyResult<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("Usage: motion_frames PROFILE UNIQUE_PREFIX".into());
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let start = client.observe().await?;
    if start.player["on_ground"] != true {
        return Err("Ground support required".into());
    }
    let xyz = |o: &Observation| -> AnyResult<[f64; 3]> {
        Ok([
            o.player["position"][0].as_f64().ok_or("x")?,
            o.player["position"][1].as_f64().ok_or("y")?,
            o.player["position"][2].as_f64().ok_or("z")?,
        ])
    };
    let origin = xyz(&start)?;
    let mut current = origin;
    let mut velocity = 0.42;
    let mut landed = false;
    for step in 0..20 {
        let y = (current[1] + velocity).max(origin[1]);
        let receipt = client
            .act(&ActionRequest {
                action_id: format!("{}-{step}", args[2]),
                action: Action::MotionFrame {
                    target: Position {
                        x: origin[0],
                        y,
                        z: origin[2],
                    },
                },
            })
            .await?;
        if receipt.outcome != Outcome::Observed {
            return Err(format!("Frame {step}: {:?}", receipt.outcome).into());
        }
        let after = client.observe().await?;
        current = xyz(&after)?;
        if step == 0 {
            let bad = client
                .act(&ActionRequest {
                    action_id: format!("{}-flight-rejected", args[2]),
                    action: Action::MotionFrame {
                        target: Position {
                            x: origin[0],
                            y: current[1] + 0.42,
                            z: origin[2],
                        },
                    },
                })
                .await?;
            if bad.outcome != Outcome::Rejected {
                return Err("Repeated upward frame was not rejected".into());
            }
        }
        if step > 0 && after.player["on_ground"] == true && (current[1] - origin[1]).abs() < 0.001 {
            landed = true;
            break;
        }
        velocity = (velocity - 0.08) * 0.98;
    }
    if !landed || (current[1] - origin[1]).abs() > 0.001 {
        return Err("Jump did not return to native supported origin".into());
    }
    let error = client
        .read_observation(&ObservationQuery {
            capability: "not_installed:read_only_view".into(),
            input: json!({}),
        })
        .await
        .unwrap_err();
    if error.code != ErrorCode::UnsupportedCapability {
        return Err("Missing observation capability was not explicit".into());
    }
    println!(
        "Jump frames observed, repeated upward flight rejected, stable landing observed; missing provider rejected"
    );
    Ok(())
}
