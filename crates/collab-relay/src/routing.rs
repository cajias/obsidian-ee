//! Message routing between subscribed clients.

/// Routes messages to the appropriate subscribers.
pub struct MessageRouter {
    // TODO: Implement in T11
}

impl MessageRouter {
    /// Create a new message router.
    #[must_use]
    pub const fn new() -> Self {
        Self {}
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}
