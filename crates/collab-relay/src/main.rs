//! Collab relay server binary.

use collab_relay::RelayServer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("collab_relay=debug".parse()?))
        .init();

    let addr = std::env::var("RELAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    tracing::info!("Starting relay server on {}", addr);

    let server = RelayServer::new();
    let bound = server.bind(&addr).await?;

    tracing::info!("Relay server listening on {}", bound.addr);

    // Wait for shutdown signal (Ctrl+C)
    tokio::signal::ctrl_c().await?;
    bound.handle.shutdown();

    Ok(())
}
