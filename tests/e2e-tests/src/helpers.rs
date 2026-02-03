//! Test helper utilities.

use std::time::Duration;

/// Default timeout for E2E operations.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Test server wrapper for E2E tests.
pub struct TestServer {
    pub url: String,
}

impl TestServer {
    /// Start a test server.
    #[allow(clippy::unused_async)] // Will have async implementation in T16
    pub async fn start() -> Self {
        // TODO: Implement in T16
        Self { url: "ws://localhost:8080".to_string() }
    }

    /// Get the server URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}
