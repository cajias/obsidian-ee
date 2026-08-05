//! Collab relay server binary.

use collab_relay::RelayServer;
use tracing_subscriber::EnvFilter;

/// Interpret an env-var value as a boolean flag. Truthy: `1`, `true`, `yes`, `on`
/// (case-insensitive). Anything else (including unset) is false.
fn is_truthy(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("collab_relay=debug".parse()?))
        .init();

    let addr = std::env::var("RELAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    // Optional bearer token. When set, clients must present it in `Identify`.
    let auth_token = std::env::var("RELAY_AUTH_TOKEN").ok().filter(|t| !t.is_empty());
    if auth_token.is_some() {
        tracing::info!("Client authentication is ENABLED (RELAY_AUTH_TOKEN set)");
    } else {
        tracing::warn!("Client authentication is DISABLED (RELAY_AUTH_TOKEN not set)");
    }

    // Optional per-document subscribe authorization (issue #29). Opt-in because
    // on-by-default deadlocks the MLS-handshake-over-relay bootstrap (a joiner
    // must subscribe to receive its Welcome, but can only mint a capability after
    // joining). Enable it where every subscriber becomes a member out-of-band.
    let subscribe_authz = is_truthy(std::env::var("RELAY_SUBSCRIBE_AUTHZ").ok().as_deref());
    if subscribe_authz {
        tracing::info!(
            "Per-document subscribe authorization is ENABLED (RELAY_SUBSCRIBE_AUTHZ set)"
        );
    } else {
        tracing::warn!(
            "Per-document subscribe authorization is DISABLED (RELAY_SUBSCRIBE_AUTHZ not set)"
        );
    }

    tracing::info!("Starting relay server on {}", addr);

    let mut server =
        RelayServer::new().with_auth_token(auth_token).with_subscribe_authz(subscribe_authz);
    if let Some(max) = std::env::var("RELAY_MAX_CONNECTIONS").ok().and_then(|v| v.parse().ok()) {
        server = server.with_max_connections(max);
    }
    let bound = server.bind(&addr).await?;

    tracing::info!("Relay server listening on {}", bound.addr);

    // Wait for shutdown signal (Ctrl+C)
    tokio::signal::ctrl_c().await?;
    bound.handle.shutdown();

    Ok(())
}
