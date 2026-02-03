//! Persistent storage for offline message queuing.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use collab_proto::{ServerMessage, UserId};
use tokio::sync::RwLock;

/// Stores messages for offline clients.
///
/// In-memory implementation for now; will be backed by DynamoDB later.
pub struct OfflineQueue {
    /// Queued messages per user.
    queues: Arc<RwLock<HashMap<UserId, VecDeque<ServerMessage>>>>,
    /// Maximum messages to store per user (prevents unbounded memory growth).
    max_per_user: usize,
}

impl OfflineQueue {
    /// Default maximum messages per user.
    pub const DEFAULT_MAX_PER_USER: usize = 1000;

    /// Create a new offline queue with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: Arc::new(RwLock::new(HashMap::new())),
            max_per_user: Self::DEFAULT_MAX_PER_USER,
        }
    }

    /// Create a new offline queue with a custom max messages per user.
    #[must_use]
    pub fn with_max_per_user(max_per_user: usize) -> Self {
        Self {
            queues: Arc::new(RwLock::new(HashMap::new())),
            max_per_user,
        }
    }

    /// Queue a message for an offline user.
    ///
    /// If the queue exceeds `max_per_user`, the oldest message is dropped.
    pub async fn enqueue(&self, user_id: &str, message: ServerMessage) {
        let mut queues = self.queues.write().await;
        let queue = queues.entry(user_id.to_string()).or_default();

        queue.push_back(message);

        // Enforce max limit
        while queue.len() > self.max_per_user {
            queue.pop_front();
        }
    }

    /// Retrieve and clear all queued messages for a user.
    ///
    /// Returns messages in the order they were queued (FIFO).
    pub async fn drain(&self, user_id: &str) -> Vec<ServerMessage> {
        let mut queues = self.queues.write().await;
        queues
            .remove(user_id)
            .map(Vec::from)
            .unwrap_or_default()
    }

    /// Check if there are queued messages for a user.
    pub async fn has_messages(&self, user_id: &str) -> bool {
        let queues = self.queues.read().await;
        queues
            .get(user_id)
            .is_some_and(|q| !q.is_empty())
    }

    /// Get the number of queued messages for a user.
    #[cfg(test)]
    pub async fn message_count(&self, user_id: &str) -> usize {
        let queues = self.queues.read().await;
        queues.get(user_id).map_or(0, VecDeque::len)
    }
}

impl Default for OfflineQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_update(doc_id: &str, from: &str, data: u8) -> ServerMessage {
        ServerMessage::YrsUpdate {
            doc_id: doc_id.into(),
            from: from.into(),
            encrypted: vec![data],
            epoch: 1,
            signature: vec![],
        }
    }

    #[tokio::test]
    async fn test_offline_user_receives_on_reconnect() {
        let queue = OfflineQueue::new();

        // Bob is offline, Alice sends messages
        queue.enqueue("bob", make_update("doc1", "alice", 1)).await;
        queue.enqueue("bob", make_update("doc1", "alice", 2)).await;
        queue.enqueue("bob", make_update("doc1", "alice", 3)).await;

        // Verify messages are queued
        assert!(queue.has_messages("bob").await);
        assert_eq!(queue.message_count("bob").await, 3);

        // Bob reconnects and drains messages
        let messages = queue.drain("bob").await;
        assert_eq!(messages.len(), 3);

        // Verify order (FIFO)
        for (i, msg) in messages.iter().enumerate() {
            match msg {
                ServerMessage::YrsUpdate { encrypted, .. } => {
                    assert_eq!(encrypted[0], (i + 1) as u8);
                }
                _ => panic!("Expected YrsUpdate"),
            }
        }

        // Queue should now be empty
        assert!(!queue.has_messages("bob").await);
        assert!(queue.drain("bob").await.is_empty());
    }

    #[tokio::test]
    async fn test_max_messages_limit() {
        let queue = OfflineQueue::with_max_per_user(3);

        // Queue 5 messages for bob (exceeds limit of 3)
        for i in 1..=5 {
            queue.enqueue("bob", make_update("doc1", "alice", i)).await;
        }

        // Only last 3 should remain
        assert_eq!(queue.message_count("bob").await, 3);

        let messages = queue.drain("bob").await;
        assert_eq!(messages.len(), 3);

        // Should have messages 3, 4, 5 (oldest 1, 2 dropped)
        for (i, msg) in messages.iter().enumerate() {
            match msg {
                ServerMessage::YrsUpdate { encrypted, .. } => {
                    assert_eq!(encrypted[0], (i + 3) as u8);
                }
                _ => panic!("Expected YrsUpdate"),
            }
        }
    }

    #[tokio::test]
    async fn test_multiple_users() {
        let queue = OfflineQueue::new();

        // Messages for different users
        queue.enqueue("bob", make_update("doc1", "alice", 1)).await;
        queue.enqueue("charlie", make_update("doc2", "alice", 2)).await;
        queue.enqueue("bob", make_update("doc1", "alice", 3)).await;

        assert_eq!(queue.message_count("bob").await, 2);
        assert_eq!(queue.message_count("charlie").await, 1);

        // Drain bob's messages
        let bob_msgs = queue.drain("bob").await;
        assert_eq!(bob_msgs.len(), 2);

        // Charlie's messages still there
        assert_eq!(queue.message_count("charlie").await, 1);
    }

    #[tokio::test]
    async fn test_empty_queue() {
        let queue = OfflineQueue::new();

        assert!(!queue.has_messages("nobody").await);
        assert!(queue.drain("nobody").await.is_empty());
    }
}
