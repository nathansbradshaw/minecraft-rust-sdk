//! One caller-selected recipe batch; run inside an existing controller session.
use minecraft_sdk::{
    Client, Profile,
    menu::{self, CraftStatus},
};
use std::{path::Path, time::Duration};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("Usage: craft_once RECIPE_ID UNIQUE_OPERATION_PREFIX PRIVATE_JOURNAL (MC_SDK_PROFILE must name your connected profile)".into());
    }
    let profile = std::env::var_os("MC_SDK_PROFILE").ok_or("MC_SDK_PROFILE required")?;
    let profile: Profile = serde_json::from_slice(&std::fs::read(profile)?)?;
    let client = Client::with_profile(&profile)?;
    let result = menu::craft_once(
        &client,
        &args[0],
        &args[1],
        Path::new(&args[2]),
        Duration::from_secs(15),
    )
    .await?;
    println!("{}", serde_json::to_string(&result)?);
    if !matches!(
        result.status,
        CraftStatus::Completed | CraftStatus::AlreadyCompleted
    ) {
        return Err("Craft operation stopped; inspect its reported original action and private journal before continuing".into());
    }
    Ok(())
}
