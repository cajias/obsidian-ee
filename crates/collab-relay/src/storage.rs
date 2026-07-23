//! Storage for offline message queuing.
//!
//! Messages destined for a subscribed-but-disconnected user are buffered here
//! and drained when the user reconnects, so briefly-offline peers do not lose
//! updates (which would cause the CRDT replicas to diverge permanently).
//!
//! This is an in-memory implementation. Memory is bounded on three axes to keep
//! the zero-knowledge relay safe from a client that queues without ever
//! reconnecting:
//! - `max_per_user` caps the messages retained for a single user (oldest first).
//! - `max_users` caps the number of distinct users tracked; when full, the
//!   least-recently-inserted user's queue is evicted.
//! - `max_total_bytes` caps the aggregate payload bytes retained across all
//!   users. Without it the per-message and per-user count caps still allow
//!   `max_users * max_per_user * MAX_MESSAGE_SIZE` (~1 TiB) of retained memory,
//!   because subscriptions survive disconnect: an attacker can amass many
//!   offline-but-subscribed user ids on a document and push max-size frames to
//!   each. The byte budget is the ceiling that actually prevents OOM.
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
    /// Running sum of [`message_bytes`] over every message in `queues`.
    /// Maintained incrementally: charged on enqueue, credited back on every
    /// removal (drain, per-user count-cap drop, user eviction). Never recomputed
    /// by scanning, so all queue operations stay O(1) in the byte accounting.
    total_bytes: usize,
}

/// Payload byte size charged against the queue's byte budget for a message.
///
/// Only the variable-length encrypted/handshake payload is counted — that is the
/// attacker-controlled part that can approach `MAX_MESSAGE_SIZE`. Fixed-size
/// fields (ids, epoch) are negligible and are ignored so the charge/credit is a
/// cheap, unambiguous `O(1)` value that is identical every time it is computed
/// for the same message.
const fn message_bytes(message: &ServerMessage) -> usize {
    match message {
        ServerMessage::YrsUpdate { encrypted, .. } => encrypted.len(),
        ServerMessage::MlsHandshake { payload, .. } => payload.len(),
        _ => 0,
    }
}

/// Drop oldest messages from `queue` until it holds at most `max_per_user`,
/// returning the total payload bytes dropped so the caller can credit the
/// aggregate counter.
fn trim_to_cap(queue: &mut VecDeque<ServerMessage>, max_per_user: usize) -> usize {
    let mut freed = 0;
    while queue.len() > max_per_user {
        freed += queue.pop_front().map_or(0, |m| message_bytes(&m));
    }
    freed
}

/// Evict the least-recently-inserted user's queue while at or above capacity.
///
/// Returns the evicted user id, if any, so the caller can prune that user's
/// subscriptions — offline-queue retention is what keeps a subscription alive.
fn evict_if_full(inner: &mut Inner, max_users: usize) -> Option<UserId> {
    while inner.queues.len() >= max_users {
        let oldest = inner.order.pop_front()?;
        if let Some(queue) = inner.queues.remove(&oldest) {
            inner.total_bytes -= queue.iter().map(message_bytes).sum::<usize>();
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
    /// Maximum aggregate payload bytes retained across all users (prevents OOM;
    /// see the module docs for the exploit this closes).
    max_total_bytes: usize,
}

impl OfflineQueue {
    /// Default maximum messages per user.
    pub const DEFAULT_MAX_PER_USER: usize = 1000;

    /// Default maximum number of distinct users tracked.
    pub const DEFAULT_MAX_USERS: usize = 10_000;

    /// Default aggregate payload byte budget: 128 MiB.
    ///
    /// Sized to comfortably hold a realistic burst — e.g. 128 users each
    /// receiving a 1 MiB `MAX_MESSAGE_SIZE` frame while briefly offline — yet
    /// four orders of magnitude below the ~1 TiB the count caps alone would
    /// permit, so a single relay process cannot be driven to OOM by retained
    /// offline messages.
    pub const DEFAULT_MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;

    /// Create a new offline queue with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            max_per_user: Self::DEFAULT_MAX_PER_USER,
            max_users: Self::DEFAULT_MAX_USERS,
            max_total_bytes: Self::DEFAULT_MAX_TOTAL_BYTES,
        }
    }

    /// Create a new offline queue with custom per-user and user-count caps.
    ///
    /// The aggregate byte budget stays at [`Self::DEFAULT_MAX_TOTAL_BYTES`]; use
    /// [`Self::with_byte_limit`] to override it.
    #[must_use]
    pub fn with_limits(max_per_user: usize, max_users: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            max_per_user,
            max_users,
            max_total_bytes: Self::DEFAULT_MAX_TOTAL_BYTES,
        }
    }

    /// Create a new offline queue with custom per-user, user-count, and
    /// aggregate-byte caps.
    #[must_use]
    pub fn with_byte_limit(max_per_user: usize, max_users: usize, max_total_bytes: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            max_per_user,
            max_users,
            max_total_bytes,
        }
    }

    /// Queue a message for an offline user.
    ///
    /// If the user's queue exceeds `max_per_user`, the oldest message is
    /// dropped. If tracking this user would exceed `max_users`, the
    /// least-recently-inserted user's entire queue is evicted first. If queuing
    /// the message would push the aggregate payload bytes over `max_total_bytes`,
    /// the message is refused (dropped) instead — nothing is mutated on that
    /// path, so no other user loses data or subscriptions.
    ///
    /// Returns the evicted user id (if any). The router uses this to also prune
    /// that user's subscriptions, so a never-reconnecting user cannot pin
    /// subscription slots forever.
    pub async fn enqueue(&self, user_id: &str, message: ServerMessage) -> Option<UserId> {
        let msg_bytes = message_bytes(&message);
        let mut inner = self.inner.write().await;

        // Refuse rather than exceed the aggregate byte budget. "Drop oldest"
        // globally would mean evicting whole *other* users (there is no
        // cross-user message ordering to drop from), destroying innocent peers'
        // queued data and subscriptions on an attacker's oversized burst.
        // Refusing the single breaching message is surgical and O(1); the
        // refused update is simply resynced by this user on reconnect, exactly
        // as the per-user count cap already tolerates loss under pressure.
        // ponytail: global byte cap only; a per-user byte cap is unneeded
        // because the per-user *count* cap already bounds a single user — add
        // one only if 1000 messages/user proves too large in practice.
        if inner.total_bytes.saturating_add(msg_bytes) > self.max_total_bytes {
            drop(inner);
            return None;
        }

        let evicted = if inner.queues.contains_key(user_id) {
            None
        } else {
            let evicted = evict_if_full(&mut inner, self.max_users);
            inner.order.push_back(user_id.to_string());
            evicted
        };

        inner.total_bytes += msg_bytes;
        let queue = inner.queues.entry(user_id.to_string()).or_default();
        queue.push_back(message);
        let freed = trim_to_cap(queue, self.max_per_user);
        inner.total_bytes -= freed;
        drop(inner);
        evicted
    }

    /// Retrieve and clear all queued messages for a user.
    ///
    /// Returns messages in the order they were queued (FIFO).
    pub async fn drain(&self, user_id: &str) -> Vec<ServerMessage> {
        let mut inner = self.inner.write().await;
        let drained = inner.queues.remove(user_id).map(Vec::from);
        if let Some(messages) = drained.as_ref() {
            let freed: usize = messages.iter().map(message_bytes).sum();
            inner.total_bytes -= freed;
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

    /// Get the running aggregate payload-byte total.
    #[cfg(test)]
    pub async fn total_bytes(&self) -> usize {
        let inner = self.inner.read().await;
        inner.total_bytes
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

    /// A `YrsUpdate` whose encrypted payload is exactly `bytes` long.
    fn make_sized(doc_id: &str, from: &str, bytes: usize) -> ServerMessage {
        ServerMessage::YrsUpdate {
            doc_id: doc_id.into(),
            from: from.into(),
            encrypted: vec![0u8; bytes],
            epoch: 1,
        }
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

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_byte_budget_refuses_overflow() {
        // Budget for 3 messages of 100 bytes; plenty of per-user/user headroom
        // so only the byte cap can bite.
        let queue = OfflineQueue::with_byte_limit(1000, 1000, 300);

        // Three 100-byte messages across two users exactly fill the budget.
        assert_eq!(queue.enqueue("alice", make_sized("doc1", "x", 100)).await, None);
        assert_eq!(queue.enqueue("alice", make_sized("doc1", "x", 100)).await, None);
        assert_eq!(queue.enqueue("bob", make_sized("doc1", "x", 100)).await, None);
        assert_eq!(queue.total_bytes().await, 300);

        // The fourth would breach the budget: refused, nothing mutated.
        assert_eq!(queue.enqueue("carol", make_sized("doc1", "x", 100)).await, None);
        assert!(queue.total_bytes().await <= 300, "byte budget must hold");
        assert_eq!(queue.total_bytes().await, 300);
        assert!(!queue.has_messages("carol").await, "refused message must not be stored");
        assert_eq!(queue.tracked_users().await, 2, "refused enqueue must not track a new user");

        // Draining credits bytes back, making room again.
        let drained = queue.drain("alice").await;
        assert_eq!(drained.len(), 2);
        assert_eq!(queue.total_bytes().await, 100);
        assert_eq!(queue.enqueue("carol", make_sized("doc1", "x", 100)).await, None);
        assert_eq!(queue.total_bytes().await, 200);
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_byte_accounting_survives_count_cap_and_eviction() {
        // Per-user cap of 2 messages, 2 users max, generous byte budget.
        let queue = OfflineQueue::with_byte_limit(2, 2, 1_000_000);

        // Overflow alice's per-user count cap: oldest dropped, bytes credited.
        for _ in 0..5 {
            queue.enqueue("alice", make_sized("doc1", "x", 100)).await;
        }
        assert_eq!(queue.message_count("alice").await, 2);
        assert_eq!(queue.total_bytes().await, 200, "count-cap drops must credit bytes");

        // Fill user slots, then force eviction of the oldest user (alice).
        queue.enqueue("bob", make_sized("doc1", "x", 100)).await;
        assert_eq!(queue.total_bytes().await, 300);
        let evicted = queue.enqueue("carol", make_sized("doc1", "x", 100)).await;
        assert_eq!(evicted, Some("alice".to_string()));
        // alice's 200 bytes credited on eviction; bob(100) + carol(100) remain.
        assert_eq!(queue.total_bytes().await, 200, "user eviction must credit bytes");
    }
}
