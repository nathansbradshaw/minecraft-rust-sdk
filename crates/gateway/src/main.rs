use minecraft_sdk_gateway::{Config, Gateway, router};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Usage: minecraft-sdk-gateway CONFIG.json")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    let listen = config.listen;
    let operator_path = config.journal.with_extension("operator.sock");
    let gateway = Gateway::start(config).await?;
    let operator_listener = minecraft_sdk_gateway::operator::bind(operator_path)?;
    let operator_gateway = gateway.clone();
    let operator = tokio::spawn(async move {
        minecraft_sdk_gateway::operator::serve(operator_gateway, operator_listener).await
    });
    let listener = tokio::net::TcpListener::bind(listen).await?;
    axum::serve(listener, router(gateway))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    operator.abort();
    Ok(())
}
