//! Move an existing stack into an empty open menu slot and return it.
//! Operator-run acceptance example; refuses to overwrite an existing stack.
use minecraft_sdk::*;
use serde_json::Value;
use std::time::Duration;
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn stack(o: &Observation, slot: i16) -> AnyResult<Value> {
    let row = o.menu["slots"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["slot"] == slot))
        .ok_or("Menu slot unavailable")?;
    Ok(serde_json::json!({"item":row["item"],"count":row["count"],"components":row["components"]}))
}
#[tokio::main]
async fn main() -> AnyResult<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "Usage: menu_transfer CONNECTED_PROFILE SOURCE_SLOT EMPTY_DESTINATION_SLOT".into(),
        );
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let source: i16 = args[1].parse()?;
    let destination: i16 = args[2].parse()?;
    let before = client.observe().await?;
    let original = stack(&before, source)?;
    if source == destination
        || before.menu["menu_id"] == 0
        || before.menu["carried"]["count"] != 0
        || original["count"].as_i64().is_none_or(|n| n <= 0)
        || stack(&before, destination)?["count"] != 0
    {
        return Err(
            "Requires an open menu, occupied source, empty different destination and empty cursor"
                .into(),
        );
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    for (step, slot) in [source, destination, destination, source]
        .into_iter()
        .enumerate()
    {
        let current = client.observe().await?;
        if current.menu["menu_id"] != before.menu["menu_id"]
            || current.menu["type"] != before.menu["type"]
        {
            return Err("Menu changed; inspect before continuing".into());
        }
        let receipt = client
            .perform(
                &ActionRequest {
                    action_id: format!("menu-transfer-{nonce}-{step}"),
                    action: Action::ClickSlot {
                        menu_id: current.menu["menu_id"].as_i64().ok_or("Menu missing")? as i32,
                        state_id: current.menu["state_id"]
                            .as_i64()
                            .ok_or("Revision missing")? as i32,
                        slot,
                        button: 0,
                        mode: 0,
                    },
                },
                Duration::from_secs(10),
            )
            .await?;
        println!("{} {:?}", receipt.action_id, receipt.outcome);
        if receipt.outcome != Outcome::Observed {
            return Err(
                "Inspect and explicitly reconcile; this example never retries mutations".into(),
            );
        }
    }
    let after = client.observe().await?;
    if stack(&after, source)? != original
        || stack(&after, destination)?["count"] != 0
        || after.menu["carried"]["count"] != 0
    {
        return Err("Menu round-trip state differs; inspect manually".into());
    }
    println!(
        "Verified transfer and return through {}",
        before.menu["type"]
    );
    Ok(())
}
