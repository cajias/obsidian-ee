//! Collab relay server binary.

use collab_relay::RelayServer;
use tracing_subscriber::EnvFilter;

/// Read `RELAY_SUBSCRIBE_AUTHZ` as a two-way override of an ON default: unset
/// leaves subscribe authorization ENABLED, and only an explicit falsey value
/// (`0`, `false`, `no`, `off` — case-insensitive, trimmed) turns it off.
///
/// An unrecognised value therefore lands ON. That direction is deliberate: authz
/// on is the more restrictive state, so a typo'd or set-but-empty value fails
/// closed rather than silently opening content fan-out.
fn subscribe_authz_enabled(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "no" | "off")
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

    // Per-document subscribe authorization (issue #29), ON by default since #72.
    // It used to be opt-in because gating `Subscribe` itself deadlocked the
    // MLS-handshake-over-relay bootstrap (a joiner must subscribe to receive its
    // Welcome, but can only mint a capability after joining). #72 moved the gate
    // off `Subscribe` and onto `YrsUpdate` fan-out: a capability-less subscribe
    // now succeeds as handshake-only, so the join still bootstraps, while content
    // reaches only subscribers authorized at the doc's current anchor epoch.
    // Set `RELAY_SUBSCRIBE_AUTHZ=0` to turn it back off.
    let subscribe_authz =
        subscribe_authz_enabled(std::env::var("RELAY_SUBSCRIBE_AUTHZ").ok().as_deref());
    if subscribe_authz {
        tracing::info!("Per-document subscribe authorization is ENABLED (the default)");
    } else {
        tracing::warn!(
            "Per-document subscribe authorization is DISABLED (RELAY_SUBSCRIBE_AUTHZ is falsey) \
             — every subscriber receives content"
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

#[cfg(test)]
mod tests {
    use super::subscribe_authz_enabled;

    /// The default this binary ships with. `RelayServer::new()` still defaults
    /// the flag OFF for the library's own tests; the DEPLOYED default is this
    /// function, and unset must mean ON.
    #[test]
    fn unset_enables_subscribe_authz() {
        assert!(subscribe_authz_enabled(None), "an unset env var must leave authz ON");
    }

    #[test]
    fn only_explicit_falsey_values_disable_it() {
        for off in ["0", "false", "no", "off", "FALSE", "Off", "  no  "] {
            assert!(!subscribe_authz_enabled(Some(off)), "{off:?} must disable authz");
        }
    }

    /// Anything not recognised as an explicit "off" stays ON: authz on is the
    /// more restrictive state, so an unknown value fails closed. Set-but-empty
    /// (`RELAY_SUBSCRIBE_AUTHZ=` from a shell) is pinned here too.
    #[test]
    fn unrecognised_values_fail_closed_to_enabled() {
        for on in ["1", "true", "yes", "on", "TRUE", " On ", "", "maybe", "0.0", "disabled"] {
            assert!(subscribe_authz_enabled(Some(on)), "{on:?} must leave authz ON");
        }
    }
}
