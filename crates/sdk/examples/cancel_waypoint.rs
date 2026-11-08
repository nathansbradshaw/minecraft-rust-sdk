//! Bounded operator conformance on a connected disposable character.
//! Cancels in one process, then disconnects even if validation fails.
use minecraft_sdk::*;
use serde_json::json;
use std::time::Duration;
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> AnyResult<()> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    if a.len() != 5 || !valid_id(&a[1]) || a[1].len() > 80 {
        return Err("Usage: cancel_waypoint CONNECTED_PROFILE UNIQUE_PREFIX X Y Z".into());
    }
    let p: Profile = serde_json::from_slice(&std::fs::read(&a[0])?)?;
    let c = Client::with_profile(&p)?;
    c.verify_instance(&p.instance_id).await?;
    let run = ActionRequest {
        action_id: format!("{}-run", a[1]),
        action: Action::StartMove {
            target: Position {
                x: a[2].parse()?,
                y: a[3].parse()?,
                z: a[4].parse()?,
            },
            jump: false,
            max_ticks: 100,
        },
    };
    let cancel = ActionRequest {
        action_id: format!("{}-cancel", a[1]),
        action: Action::CancelControl {
            action_id: run.action_id.clone(),
        },
    };
    let result = async {
        for id in [&run.action_id, &cancel.action_id] {
            if !matches!(c.action_status(id).await,Err(e) if e.code==ErrorCode::NotFound) {
                return Err(
                    "Fresh IDs required; inspect existing intentions instead of replaying".into(),
                );
            }
        }
        let admitted = c.act(&run).await?;
        println!("{}", json!({"event":"admitted","receipt":admitted}));
        if admitted.outcome != Outcome::Running {
            return Err("Control was not admitted".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let before = c.observe().await?;
        println!("{}", json!({"event":"before_cancel","observation":before}));
        let released = c.perform(&cancel, Duration::from_secs(4)).await?;
        let original = c.action_status(&run.action_id).await?;
        println!(
            "{}",
            json!({"event":"released","original":original,"cancellation":released})
        );
        if original.outcome != Outcome::Cancelled
            || released.outcome != Outcome::Observed
            || released.evidence["native_effect_verified"] != true
        {
            return Err("In-flight cancellation with native release was not verified".into());
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let disconnected = c.disconnect().await;
    println!(
        "{}",
        json!({"event":"exit","disconnected":disconnected.is_ok(),"error":result.as_ref().err().map(ToString::to_string)})
    );
    result?;
    disconnected?;
    Ok(())
}
