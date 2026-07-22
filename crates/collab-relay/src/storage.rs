//! Storage for offline message queuing.
//!
//! Messages destined for a subscribed-but-disconnected user are buffered here
//! and drained when the user reconnects, so briefly-offline peers do not lose
//! updates (which would cause the CRDT replicas to diverge permanently).
//!
//! This is an in-memory implementation. Memory is bounded on two axes to keep
//! the zero-knowledge relay safe from a client that queues without ever
//! reconnecting:
//! - `max_per_user` caps the messages retained for a single user (oldest first).
//! - `max_users` caps the number of distinct users tracked; when full, the
//!   least-recently-inserted user's queue is evicted.
//!
//! A `DynamoDB`-backed implementation can be introduced later behind a Cargo
//! feature (see `Cargo.toml`).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use collab_proto::{ServerMessage, UserId};
use tokio::sync::RwLock;

/// Internal, lock-guarded state for [`OfflineQueue`].
#[derive(Default)]
struct Inner {
    /// Queued messages per user.
    queues: HashMap<UserId, VecDeque<ServerMessage>>,
    /// Insertion order of the currently-tracked users, used to evict the
    /// least-recently-inserted user when `max_users` is exceeded. Kept in sync
    /// with `queues`: every key in `queues` appears exactly once here.
    order: VecDeque<UserId>,
}

/// Evict the least-recently-inserted user's queue while at or above capacity.
///
/// Returns the evicted user id, if any, so the caller can prune that user's
/// subscriptions — offline-queue retention is what keeps a subscription alive.
fn evict_if_full(inner: &mut Inner, max_users: usize) -> Option<UserId> {
    while inner.queues.len() >= max_users {
        let oldest = inner.order.pop_front()?;
        if inner.queues.remove(&oldest).is_some() {
            tracing::warn!(evicted = %oldest, "Offline queue at capacity; evicted oldest user");
            return Some(oldest);
        }
    }
    None
}

/// Stores messages for offline clients.
pub struct OfflineQueue {
    inner: Arc<RwLock<Inner>>,
    /// Maximum messages to store per user (prevents unbounded per-user growth).
    max_per_user: usize,
    /// Maximum number of distinct users tracked (prevents unbounded key growth).
    max_users: usize,
}

impl OfflineQueue {
    /// Default maximum messages per user.
    pub const DEFAULT_MAX_PER_USER: usize = 1000;

    /// Default maximum number of distinct users tracked.
    pub const DEFAULT_MAX_USERS: usize = 10_000;

    /// Create a new offline queue with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            max_per_user: Self::DEFAULT_MAX_PER_USER,
            max_users: Self::DEFAULT_MAX_USERS,
        }
    }

    /// Create a new offline queue with custom per-user and user-count caps.
    #[must_use]
    pub fn with_limits(max_per_user: usize, max_users: usize) -> Self {
        Self { inner: Arc::new(RwLock::new(Inner::default())), max_per_user, max_users }
    }

    /// Queue a message for an offline user.
    ///
    /// If the user's queue exceeds `max_per_user`, the oldest message is
    /// dropped. If tracking this user would exceed `max_users`, the
    /// least-recently-inserted user's entire queue is evicted first.
    ///
    /// Returns the evicted user id (if any). The router uses this to also prune
    /// that user's subscriptions, so a never-reconnecting user cannot pin
    /// subscription slots forever.
    pub async fn enqueue(&self, user_id: &str, message: ServerMessage) -> Option<UserId> {
        let mut inner = self.inner.write().await;

        let evicted = if inner.queues.contains_key(user_id) {
            None
        } else {
            let evicted = evict_if_full(&mut inner, self.max_users);
            inner.order.push_back(user_id.to_string());
            evicted
        };

        let queue = inner.queues.entry(user_id.to_string()).or_default();
        queue.push_back(message);
        while queue.len() > self.max_per_user {
            queue.pop_front();
        }
        drop(inner);
        evicted
    }

    /// Retrieve and clear all queued messages for a user.
    ///
    /// Returns messages in the order they were queued (FIFO).
    pub async fn drain(&self, user_id: &str) -> Vec<ServerMessage> {
        let mut inner = self.inner.write().await;
        let drained = inner.queues.remove(user_id).map(Vec::from);
        if drained.is_some() {
            inner.order.retain(|u| u != user_id);
        }
        drop(inner);
        drained.unwrap_or_default()
    }

    /// Check if there are queued messages for a user.
    pub async fn has_messages(&self, user_id: &str) -> bool {
        let inner = self.inner.read().await;
        inner.queues.get(user_id).is_some_and(|q| !q.is_empty())
    }

    /// Get the number of queued messages for a user.
    #[cfg(test)]
    pub async fn message_count(&self, user_id: &str) -> usize {
        let inner = self.inner.read().await;
        inner.queues.get(user_id).map_or(0, VecDeque::len)
    }

    /// Get the number of distinct users currently tracked.
    #[cfg(test)]
    pub async fn tracked_users(&self) -> usize {
        let inner = self.inner.read().await;
        inner.queues.len()
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
        }
    }

    fn extract_update_data(msg: &ServerMessage) -> u8 {
        let ServerMessage::YrsUpdate { encrypted, .. } = msg else {
            panic!("Expected YrsUpdate");
        };
        encrypted[0]
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
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
            #[allow(clippy::cast_possible_truncation)]
            let expected = (i + 1) as u8;
            assert_eq!(extract_update_data(msg), expected);
        }

        // Queue should now be empty
        assert!(!queue.has_messages("bob").await);
        assert!(queue.drain("bob").await.is_empty());
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_max_messages_limit() {
        let queue = OfflineQueue::with_limits(3, OfflineQueue::DEFAULT_MAX_USERS);

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
            #[allow(clippy::cast_possible_truncation)]
            let expected = (i + 3) as u8;
            assert_eq!(extract_update_data(msg), expected);
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

    #[tokio::test]
    async fn test_max_users_evicts_oldest() {
        // Capacity for 2 users, 10 messages each.
        let queue = OfflineQueue::with_limits(10, 2);

        queue.enqueue("alice", make_update("doc1", "x", 1)).await;
        queue.enqueue("bob", make_update("doc1", "x", 2)).await;
        assert_eq!(queue.tracked_users().await, 2);

        // Adding a third user evicts the oldest (alice).
        queue.enqueue("carol", make_update("doc1", "x", 3)).await;
        assert_eq!(queue.tracked_users().await, 2);
        assert!(!queue.has_messages("alice").await, "oldest user should be evicted");
        assert!(queue.has_messages("bob").await);
        assert!(queue.has_messages("carol").await);
    }

    #[tokio::test]
    async fn test_drain_frees_capacity_and_order() {
        let queue = OfflineQueue::with_limits(10, 2);

        queue.enqueue("alice", make_update("doc1", "x", 1)).await;
        queue.enqueue("bob", make_update("doc1", "x", 2)).await;

        // Draining alice frees a slot; carol can be tracked without evicting bob.
        let drained = queue.drain("alice").await;
        assert_eq!(drained.len(), 1);

        queue.enqueue("carol", make_update("doc1", "x", 3)).await;
        assert_eq!(queue.tracked_users().await, 2);
        assert!(queue.has_messages("bob").await, "bob must not be evicted after alice drained");
        assert!(queue.has_messages("carol").await);
    }
}
