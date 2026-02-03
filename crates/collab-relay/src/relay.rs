//! WebSocket relay server implementation.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// The relay server managing WebSocket connections.
#[allow(dead_code)] // Fields used in T10 implementation
pub struct RelayServer {
    /// Connected clients by user ID.
    clients: Arc<RwLock<HashMap<String, ClientConnection>>>,
    /// Document subscriptions: `doc_id` -> set of `user_id`s.
    subscriptions: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

/// A connected client.
pub struct ClientConnection {
    /// User identifier.
    pub user_id: String,
    // TODO: Add WebSocket sender
}

impl RelayServer {
    /// Create a new relay server.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start the relay server on the given address.
    ///
    /// # Errors
    ///
    /// Returns an error if binding fails.
    #[allow(clippy::unused_async)] // Will have async implementation in T10
    pub async fn start(&self, _addr: &str) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: Implement WebSocket server in T10
        Ok(())
    }
}

impl Default for RelayServer {
    fn default() -> Self {
        Self::new()
    }
}
