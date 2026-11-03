use minecraft_sdk::{Client, Profile};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os("MC_SDK_PROFILE"))
        .ok_or("Pass your private mc-sdk profile filename or set MC_SDK_PROFILE")?;
    let profile: Profile = serde_json::from_slice(&std::fs::read(file)?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let observation = client.observe().await?;
    println!("{}", serde_json::to_string_pretty(&observation)?);
    Ok(())
}
