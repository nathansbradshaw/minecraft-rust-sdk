//! Editable bot program. No strategy is imposed by the SDK or gateway.
use minecraft_sdk::{Client, Profile};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() != Some("observe") {
        println!(
            "This character is imported but stopped. Connect explicitly with mc-sdk connect bot.key YOUR_CONTROLLER_ID, then cargo run -- observe. Edit this program to add your strategy."
        );
        return Ok(());
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read("bot.key")?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let observation = client.observe().await?;
    println!("{}", serde_json::to_string_pretty(&observation)?);
    Ok(())
}
