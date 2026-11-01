//! Linux operator-only destructive-to-this-adapter test. Never exposed through HTTP/MCP.
use minecraft_sdk::*;
use std::{path::Path, time::Duration};
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 6 {
        return Err(
            "Usage: interruption_probe CONNECTED_PROFILE ADAPTER_PID ACTION_ID X Y Z".into(),
        );
    }
    let pid: i32 = args[1].parse()?;
    if pid <= 1 {
        return Err("Invalid adapter PID".into());
    }
    let command = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    let program = std::str::from_utf8(command.split(|b| *b == 0).next().ok_or("Missing command")?)?;
    if Path::new(program).file_name().and_then(|n| n.to_str()) != Some("minecraft-sdk-java-1-21-1")
    {
        return Err("Refusing to signal a process other than the SDK adapter".into());
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let state = client.observe().await?;
    if state.player["health"].as_f64().is_none_or(|h| h <= 0.) {
        return Err(
            "Respawn the disposable character before this test; no process was signaled".into(),
        );
    }
    let target = [
        args[3].parse::<f64>()?,
        args[4].parse::<f64>()?,
        args[5].parse::<f64>()?,
    ];
    let distance = (0..3)
        .map(|i| (state.player["position"][i].as_f64().unwrap_or(f64::NAN) - target[i]).powi(2))
        .sum::<f64>()
        .sqrt();
    if !distance.is_finite() || distance < 2.5 {
        return Err(
            "Choose a supported waypoint at least 2.5 blocks away; no process was signaled".into(),
        );
    }
    let id = args[2].clone();
    if client.action_status(&id).await.is_ok() {
        return Err("Use a fresh action ID for this test".into());
    }
    let request = ActionRequest {
        action_id: id.clone(),
        action: Action::Move {
            target: Position {
                x: args[3].parse()?,
                y: args[4].parse()?,
                z: args[5].parse()?,
            },
            on_ground: false,
            jump: false,
        },
    };
    let sender = Client::with_profile(&profile)?;
    let operation = tokio::spawn(async move { sender.act(&request).await });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(receipt) = client.action_status(&id).await {
            if receipt.outcome != Outcome::Prepared {
                return Err("Action settled before interruption; no process was signaled".into());
            }
            break;
        }
        if std::time::Instant::now() > deadline {
            return Err("Action was not prepared; no process was signaled".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let receipt = operation.await??;
    if receipt.outcome != Outcome::Uncertain {
        return Err("Interrupted mutation was not retained as uncertain".into());
    }
    if client.discover().await?.ready {
        return Err("Failed adapter was not quarantined".into());
    }
    println!(
        "Interrupted action {id} is uncertain and adapter quarantined; restart gateway, reconnect, verify duplicate returns the same receipt, and explicitly reconcile without replay"
    );
    Ok(())
}
