//! Persistent storage for offline message queuing.

/// Stores messages for offline clients.
pub struct OfflineQueue {
    // TODO: Implement with DynamoDB in T12
}

impl OfflineQueue {
    /// Create a new offline queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {}
    }
}

impl Default for OfflineQueue {
    fn default() -> Self {
        Self::new()
    }
}
