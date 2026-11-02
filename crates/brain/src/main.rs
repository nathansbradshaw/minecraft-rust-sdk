use minecraft_sdk_brain::{Brain, Config, Result, private_read};
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!(
            "{}",
            serde_json::json!({"event":"brain_stopped","error":e.to_string()})
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 || args[0] != "run" {
        return Err(
            "Usage: minecraft-sdk-brain run PRIVATE_CONFIG.json (inside isolated container)".into(),
        );
    }
    let config: Config =
        serde_json::from_slice(&private_read(std::path::Path::new(&args[1]), 16384)?)?;
    let mut brain = Brain::open(config).await?;
    brain.run().await
}
