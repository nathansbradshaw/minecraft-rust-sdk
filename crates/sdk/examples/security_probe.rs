//! Explicit operator-run HTTP adversarial trial against a connected disposable bot.
use minecraft_sdk::*;
use serde_json::json;
use std::{io::Write, time::Duration};
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn expected<T>(result: Result<T>, code: ErrorCode) -> AnyResult<()> {
    match result {
        Err(e) if e.code == code => Ok(()),
        _ => Err(format!("Expected {code:?}").into()),
    }
}
#[tokio::main]
async fn main() -> AnyResult<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: security_probe CONNECTED_PROFILE NEW_PRIVATE_TEST_PROFILE".into());
    }
    use std::os::unix::fs::OpenOptionsExt;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args[1])?;
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .to_string();
    let anonymous = Client::new(&profile.endpoint)?;
    let credential = anonymous.register(&format!("probe-{nonce}")).await?;
    let mut stranger = Profile {
        endpoint: profile.endpoint.clone(),
        instance_id: profile.instance_id.clone(),
        credential,
        session: None,
    };
    let old = stranger.clone();
    let new = Client::with_profile(&stranger)?.rotate_key().await?;
    stranger.credential = new;
    output.write_all(&serde_json::to_vec(&stranger)?)?;
    output.sync_all()?;
    expected(
        Client::with_profile(&old)?.connect("old-key").await,
        ErrorCode::Unauthorized,
    )?;
    let mut forged = stranger.clone();
    forged.credential.bot_id = profile.credential.bot_id.clone();
    forged.session = profile.session.clone();
    expected(
        Client::with_profile(&forged)?.observe().await,
        ErrorCode::Unauthorized,
    )?;
    expected(
        client.connect(&format!("competing-{nonce}")).await,
        ErrorCode::Conflict,
    )?;
    let mut wrong_session = profile.clone();
    wrong_session
        .session
        .as_mut()
        .ok_or("Connected profile required")?
        .session_id = "forged-session".into();
    expected(
        Client::with_profile(&wrong_session)?.observe().await,
        ErrorCode::SessionRequired,
    )?;
    let id = format!("security-{nonce}");
    let request = ActionRequest {
        action_id: id.clone(),
        action: Action::Look {
            yaw: 45.,
            pitch: 0.,
        },
    };
    let receipt = client.act(&request).await?;
    if receipt.outcome != Outcome::Observed {
        return Err("Benign probe action did not produce a native effect".into());
    }
    if serde_json::to_value(&receipt)? != serde_json::to_value(client.act(&request).await?)? {
        return Err("Duplicate mutated its durable receipt".into());
    }
    expected(
        client
            .act(&ActionRequest {
                action_id: id.clone(),
                action: Action::Look {
                    yaw: 46.,
                    pitch: 0.,
                },
            })
            .await,
        ErrorCode::Conflict,
    )?;
    expected(
        Client::with_profile(&stranger)?.action_status(&id).await,
        ErrorCode::NotFound,
    )?;
    expected(
        client
            .act(&ActionRequest {
                action_id: format!("unknown-{nonce}"),
                action: Action::Extension {
                    capability: "probe:uninstalled".into(),
                    input: json!({"plugin":"must-not-install"}),
                },
            })
            .await,
        ErrorCode::UnsupportedCapability,
    )?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let endpoint = profile.endpoint.trim_end_matches('/');
    let path = format!("{endpoint}/v1/bots/{}/actions", profile.credential.bot_id);
    expected(
        Client::with_profile(&forged)?
            .read_observation(&ObservationQuery {
                capability: "probe:view".into(),
                input: json!({}),
            })
            .await,
        ErrorCode::Unauthorized,
    )?;
    expected(
        client
            .read_observation(&ObservationQuery {
                capability: "probe:uninstalled_view".into(),
                input: json!({}),
            })
            .await,
        ErrorCode::UnsupportedCapability,
    )?;
    let forged_query = http
        .post(format!(
            "{endpoint}/v1/bots/{}/observations/query",
            profile.credential.bot_id
        ))
        .bearer_auth(&profile.credential.key)
        .header(
            "x-minecraft-session",
            &profile
                .session
                .as_ref()
                .ok_or("Session required")?
                .session_id,
        )
        .json(&json!({"capability":"probe:view","input":{},"player_uuid":"forged-other-player"}))
        .send()
        .await?
        .status();
    if !matches!(forged_query.as_u16(), 400 | 422) {
        return Err("Observation query accepted a forged actor field".into());
    }
    for action in [
        json!({"kind":"chat","message":"must-not-be-dispatched","actor":"forged-other-player"}),
        json!({"kind":"operator_bot_reset"}),
        json!({"kind":"raw_packet","packet":7}),
    ] {
        let status = http
            .post(&path)
            .bearer_auth(&profile.credential.key)
            .header(
                "x-minecraft-session",
                &profile.session.as_ref().unwrap().session_id,
            )
            .json(&json!({"action_id":format!("forged-{nonce}"),"action":action}))
            .send()
            .await?
            .status();
        if !matches!(status.as_u16(), 400 | 422) {
            return Err("Forged actor or privileged request was not rejected".into());
        }
    }
    let status = http
        .post(&path)
        .bearer_auth(&profile.credential.key)
        .header("content-type", "application/json")
        .body(vec![b' '; 17000])
        .send()
        .await?
        .status();
    if status.as_u16() != 413 {
        return Err("Request body budget not enforced".into());
    }
    for private in ["v1/admit", "v1/control", "v1/operator/reset"] {
        let status = http
            .post(format!("{endpoint}/{private}"))
            .bearer_auth(&profile.credential.key)
            .json(&json!({}))
            .send()
            .await?
            .status();
        if status.as_u16() != 404 {
            return Err("Private operation was exposed on public HTTP".into());
        }
    }
    println!(
        "HTTP security trial passed: scoped keys/rotation, controller fencing, forged actors, privileged operations, duplicate IDs, capability errors and body budgets"
    );
    Ok(())
}
