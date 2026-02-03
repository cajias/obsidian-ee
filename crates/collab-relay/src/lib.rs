//! WebSocket relay server for collaborative document editing.
//!
//! The relay server:
//! - Accepts WebSocket connections from clients
//! - Routes messages between clients subscribed to the same document
//! - Persists messages for offline clients
//! - Does NOT have access to encryption keys (E2E encrypted)

pub mod relay;
pub mod routing;
pub mod storage;

pub use relay::{BoundServer, ClientHandle, RelayServer, ServerHandle};
pub use routing::MessageRouter;
pub use storage::OfflineQueue;
