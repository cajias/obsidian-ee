//! Message routing between subscribed clients.
//!
//! The router is the single source of truth for three things:
//! - **client sessions** (`clients`): the live [`ClientHandle`] for each
//!   currently-connected user, keyed by user id and tagged with a per-connection
//!   id so a stale connection can never evict a newer one.
//! - **subscriptions**: which users are subscribed to which documents. These are
//!   *retained across disconnect* so that updates for a briefly-offline
//!   subscriber can be queued rather than silently dropped.
//! - the **offline queue** (see [`crate::storage::OfflineQueue`]).
//!
//! Subscriptions are bounded to keep memory safe against a malicious client:
//! the total number of documents and the subscribers-per-document are capped.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use collab_proto::{DocumentId, ServerMessage, UserId};
use tokio::sync::RwLock;

use crate::relay::ClientHandle;
use crate::storage::OfflineQueue;

/// Routes messages to the appropriate subscribers.
pub struct MessageRouter {
    /// Document subscriptions: `doc_id` -> set of `user_id`s. Retained across
    /// disconnect so offline subscribers can be queued for.
    subscriptions: Arc<RwLock<HashMap<DocumentId, HashSet<UserId>>>>,
    /// Live client handles by user ID.
    clients: Arc<RwLock<HashMap<UserId, ClientHandle>>>,
    /// Buffer for messages to subscribed-but-disconnected users.
    offline: OfflineQueue,
    /// Maximum number of distinct documents tracked.
    max_documents: usize,
    /// Maximum number of subscribers per document.
    max_subscribers_per_doc: usize,
}

impl MessageRouter {
    /// Default maximum number of distinct documents tracked.
    pub const DEFAULT_MAX_DOCUMENTS: usize = 100_000;

    /// Default maximum number of subscribers per document.
    pub const DEFAULT_MAX_SUBSCRIBERS_PER_DOC: usize = 1_000;

    /// Create a new message router with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::new(),
            max_documents: Self::DEFAULT_MAX_DOCUMENTS,
            max_subscribers_per_doc: Self::DEFAULT_MAX_SUBSCRIBERS_PER_DOC,
        }
    }

    /// Register a client for message routing.
    ///
    /// If a different connection is already registered for this user, that older
    /// session is explicitly evicted (a best-effort [`ServerMessage::Error`] with
    /// [`collab_proto::ErrorCode::SessionReplaced`] is sent, then its connection
    /// is signalled to close). The newer connection always wins, which — combined
    /// with the connection-id check in [`Self::unregister_client`] — makes the
    /// reconnect path deterministic and free of the stale-cleanup race.
    pub async fn register_client(&self, handle: ClientHandle) {
        let mut clients = self.clients.write().await;
        let stale =
            clients.get(&handle.user_id).filter(|previous| previous.conn_id() != handle.conn_id());
        if let Some(previous) = stale {
            tracing::debug!(
                user = %handle.user_id,
                old_conn = previous.conn_id(),
                new_conn = handle.conn_id(),
                "Replacing existing session for user"
            );
            let _ = previous.send(ServerMessage::Error {
                code: collab_proto::ErrorCode::SessionReplaced,
                message: "Replaced by a newer connection".to_string(),
            });
            previous.signal_close();
        }
        clients.insert(handle.user_id.clone(), handle);
    }

    /// Unregister a client's live handle on disconnect.
    ///
    /// Uses compare-and-remove: the handle is only removed if the stored session
    /// still belongs to `conn_id`. This prevents a stale connection's teardown
    /// from evicting a newer session that took over the same user id.
    ///
    /// Subscriptions are intentionally **not** removed here — they are retained
    /// so that updates for this now-offline user are queued (see
    /// [`Self::route_message`]) and delivered when the user reconnects.
    pub async fn unregister_client(&self, user_id: &str, conn_id: u64) {
        let mut clients = self.clients.write().await;
        if clients.get(user_id).is_some_and(|h| h.conn_id() == conn_id) {
            clients.remove(user_id);
        }
    }

    /// Drain any queued offline messages for a user (called on reconnect).
    pub async fn drain_offline(&self, user_id: &str) -> Vec<ServerMessage> {
        self.offline.drain(user_id).await
    }

    /// Subscribe a user to a document.
    ///
    /// Returns `false` (and does not subscribe) if a resource limit would be
    /// exceeded: the global document cap or the per-document subscriber cap.
    /// Re-subscribing an already-subscribed user is idempotent and returns
    /// `true`.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // guard needed across all branches
    pub async fn subscribe(&self, user_id: &str, doc_id: &str) -> bool {
        let mut subs = self.subscriptions.write().await;

        // Re-subscribing an existing member is idempotent.
        if subs.get(doc_id).is_some_and(|set| set.contains(user_id)) {
            return true;
        }
        // A brand-new document counts against the global document cap.
        if !subs.contains_key(doc_id) && subs.len() >= self.max_documents {
            tracing::warn!(doc_id = %doc_id, "Rejecting subscribe: document cap reached");
            return false;
        }
        let set = subs.entry(doc_id.to_string()).or_default();
        // An existing document counts against the per-document subscriber cap.
        if set.len() >= self.max_subscribers_per_doc {
            tracing::warn!(doc_id = %doc_id, "Rejecting subscribe: per-document cap reached");
            return false;
        }
        set.insert(user_id.to_string());
        true
    }

    /// Unsubscribe a user from a document, pruning the set if it becomes empty.
    pub async fn unsubscribe(&self, user_id: &str, doc_id: &str) {
        let mut subs = self.subscriptions.write().await;
        let Some(subscribers) = subs.get_mut(doc_id) else {
            return;
        };
        subscribers.remove(user_id);
        if subscribers.is_empty() {
            subs.remove(doc_id);
        }
    }

    /// Route a message to all subscribers of a document except the sender.
    ///
    /// Online subscribers are sent the message directly. Subscribers with no
    /// live connection have the message queued to the offline buffer for
    /// delivery on reconnect. A subscriber whose send channel is full is treated
    /// as a too-slow consumer: its connection is signalled to close and the
    /// message is queued for redelivery when it reconnects.
    ///
    /// Returns the number of clients the message was delivered to directly.
    pub async fn route_message(
        &self,
        doc_id: &str,
        from_user: &str,
        message: ServerMessage,
    ) -> usize {
        let Some(subscribers) = self.subscribers_except(doc_id, from_user).await else {
            return 0;
        };

        let (sent_count, offline, slow) = self.fan_out(&subscribers, &message).await;

        // Disconnect slow consumers so subsequent messages (and this one) are
        // buffered until they reconnect.
        if !slow.is_empty() {
            self.disconnect_slow(&slow).await;
        }

        for subscriber_id in offline.iter().chain(slow.iter()) {
            self.offline.enqueue(subscriber_id, message.clone()).await;
        }

        sent_count
    }

    /// Snapshot of the subscribers of `doc_id`, excluding the sender. Returns
    /// `None` if the document has no subscribers at all.
    async fn subscribers_except(&self, doc_id: &str, from_user: &str) -> Option<Vec<String>> {
        let subs = self.subscriptions.read().await;
        let result =
            subs.get(doc_id).map(|set| set.iter().filter(|id| *id != from_user).cloned().collect());
        drop(subs);
        result
    }

    /// Attempt to deliver `message` to each subscriber, classifying them into
    /// `(sent_count, offline, slow)`.
    async fn fan_out(
        &self,
        subscribers: &[String],
        message: &ServerMessage,
    ) -> (usize, Vec<String>, Vec<String>) {
        let clients = self.clients.read().await;
        let mut sent_count = 0;
        let mut offline: Vec<String> = Vec::new();
        let mut slow: Vec<String> = Vec::new();
        for subscriber_id in subscribers {
            match clients.get(subscriber_id) {
                Some(client) if client.send(message.clone()).is_ok() => sent_count += 1,
                Some(_) => slow.push(subscriber_id.clone()),
                None => offline.push(subscriber_id.clone()),
            }
        }
        (sent_count, offline, slow)
    }

    /// Remove and signal-close a set of too-slow consumers.
    #[allow(clippy::excessive_nesting, clippy::significant_drop_tightening)]
    async fn disconnect_slow(&self, slow: &[String]) {
        let mut clients = self.clients.write().await;
        for subscriber_id in slow {
            let Some(handle) = clients.remove(subscriber_id) else {
                continue;
            };
            tracing::warn!(subscriber = %subscriber_id, "Disconnecting slow consumer");
            handle.signal_close();
        }
    }

    /// Get all subscribers for a document.
    #[cfg(test)]
    pub async fn get_subscribers(&self, doc_id: &str) -> HashSet<String> {
        self.subscriptions.read().await.get(doc_id).cloned().unwrap_or_default()
    }

    /// Check if a user is subscribed to a document.
    pub async fn is_subscribed(&self, user_id: &str, doc_id: &str) -> bool {
        self.subscriptions.read().await.get(doc_id).is_some_and(|subs| subs.contains(user_id))
    }

    /// Number of live client sessions (for testing).
    #[cfg(test)]
    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Get a live client handle by user ID (for testing).
    #[cfg(test)]
    pub async fn get_client(&self, user_id: &str) -> Option<ClientHandle> {
        self.clients.read().await.get(user_id).cloned()
    }

    /// Whether a user currently has queued offline messages (for testing).
    #[cfg(test)]
    pub async fn has_offline_messages(&self, user_id: &str) -> bool {
        self.offline.has_messages(user_id).await
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::ClientHandle;
    use tokio::sync::mpsc;

    /// Build a test client with a bounded channel, mirroring production.
    fn create_test_client(
        user_id: &str,
        conn_id: u64,
    ) -> (ClientHandle, mpsc::Receiver<ServerMessage>) {
        let (tx, rx) = mpsc::channel(64);
        let handle = ClientHandle::new(user_id.to_string(), conn_id, tx);
        (handle, rx)
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_subscribe_and_receive() {
        let router = MessageRouter::new();

        let (alice_handle, _alice_rx) = create_test_client("alice", 1);
        let (bob_handle, mut bob_rx) = create_test_client("bob", 2);

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;

        assert!(router.subscribe("alice", "doc1").await);
        assert!(router.subscribe("bob", "doc1").await);

        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 1);

        let received = bob_rx.try_recv().unwrap();
        match received {
            ServerMessage::YrsUpdate { from, doc_id, .. } => {
                assert_eq!(from, "alice");
                assert_eq!(doc_id, "doc1");
            }
            _ => panic!("Expected YrsUpdate"),
        }
    }

    #[tokio::test]
    async fn test_unsubscribed_client_does_not_receive() {
        let router = MessageRouter::new();

        let (alice_handle, _alice_rx) = create_test_client("alice", 1);
        let (bob_handle, mut bob_rx) = create_test_client("bob", 2);
        let (eve_handle, mut eve_rx) = create_test_client("eve", 3);

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;
        router.register_client(eve_handle).await;

        assert!(router.subscribe("alice", "doc1").await);
        assert!(router.subscribe("bob", "doc1").await);

        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 1);

        assert!(bob_rx.try_recv().is_ok());
        assert!(eve_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_sender_does_not_receive_own_message() {
        let router = MessageRouter::new();

        let (alice_handle, mut alice_rx) = create_test_client("alice", 1);
        router.register_client(alice_handle).await;
        assert!(router.subscribe("alice", "doc1").await);

        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 0);
        assert!(alice_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let router = MessageRouter::new();

        let (alice_handle, _alice_rx) = create_test_client("alice", 1);
        let (bob_handle, mut bob_rx) = create_test_client("bob", 2);

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;

        assert!(router.subscribe("alice", "doc1").await);
        assert!(router.subscribe("bob", "doc1").await);

        router.unsubscribe("bob", "doc1").await;

        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 0);
        assert!(bob_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_unsubscribe_prunes_empty_set() {
        let router = MessageRouter::new();
        let (alice_handle, _rx) = create_test_client("alice", 1);
        router.register_client(alice_handle).await;

        assert!(router.subscribe("alice", "doc1").await);
        assert!(!router.get_subscribers("doc1").await.is_empty());

        router.unsubscribe("alice", "doc1").await;
        // The now-empty document set must be removed, not left as a leak.
        assert!(router.get_subscribers("doc1").await.is_empty());
        assert!(!router.is_subscribed("alice", "doc1").await);
    }

    #[tokio::test]
    async fn test_multiple_documents() {
        let router = MessageRouter::new();

        let (alice_handle, _alice_rx) = create_test_client("alice", 1);
        let (bob_handle, mut bob_rx) = create_test_client("bob", 2);
        let (charlie_handle, mut charlie_rx) = create_test_client("charlie", 3);

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;
        router.register_client(charlie_handle).await;

        assert!(router.subscribe("alice", "doc1").await);
        assert!(router.subscribe("bob", "doc1").await);
        assert!(router.subscribe("alice", "doc2").await);
        assert!(router.subscribe("charlie", "doc2").await);

        let msg1 = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1],
            epoch: 1,
        };
        router.route_message("doc1", "alice", msg1).await;

        assert!(bob_rx.try_recv().is_ok());
        assert!(charlie_rx.try_recv().is_err());

        let msg2 = ServerMessage::YrsUpdate {
            doc_id: "doc2".into(),
            from: "alice".into(),
            encrypted: vec![2],
            epoch: 1,
        };
        router.route_message("doc2", "alice", msg2).await;

        assert!(charlie_rx.try_recv().is_ok());
        assert!(bob_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_offline_subscriber_is_queued_and_drained_on_reconnect() {
        let router = MessageRouter::new();

        let (alice_handle, _alice_rx) = create_test_client("alice", 1);
        let (bob_handle, _bob_rx) = create_test_client("bob", 2);
        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;

        assert!(router.subscribe("alice", "doc1").await);
        assert!(router.subscribe("bob", "doc1").await);

        // Bob disconnects; his subscription is retained.
        router.unregister_client("bob", 2).await;
        assert!(router.is_subscribed("bob", "doc1").await);

        // Alice sends two updates while Bob is offline.
        let update = |data: u8| ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![data],
            epoch: 1,
        };
        assert_eq!(router.route_message("doc1", "alice", update(10)).await, 0);
        assert_eq!(router.route_message("doc1", "alice", update(20)).await, 0);
        assert!(router.has_offline_messages("bob").await);

        // Bob reconnects and drains his queued messages in order.
        let queued = router.drain_offline("bob").await;
        assert_eq!(queued.len(), 2);
        assert!(!router.has_offline_messages("bob").await);
    }

    #[tokio::test]
    async fn test_unregister_is_conn_id_scoped() {
        let router = MessageRouter::new();

        // New connection (conn 2) takes over from an older one (conn 1).
        let (old_handle, _old_rx) = create_test_client("alice", 1);
        let (new_handle, _new_rx) = create_test_client("alice", 2);
        router.register_client(old_handle).await;
        router.register_client(new_handle).await;
        assert_eq!(router.client_count().await, 1);

        // The stale connection's teardown must NOT evict the newer session.
        router.unregister_client("alice", 1).await;
        assert_eq!(router.client_count().await, 1);
        assert_eq!(router.get_client("alice").await.map(|h| h.conn_id()), Some(2));

        // The current connection's teardown does remove it.
        router.unregister_client("alice", 2).await;
        assert_eq!(router.client_count().await, 0);
    }

    #[tokio::test]
    async fn test_subscribe_respects_per_doc_cap() {
        let router = MessageRouter {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::new(),
            max_documents: 100,
            max_subscribers_per_doc: 2,
        };

        assert!(router.subscribe("a", "doc1").await);
        assert!(router.subscribe("b", "doc1").await);
        // Third distinct subscriber exceeds the per-document cap.
        assert!(!router.subscribe("c", "doc1").await);
        // Re-subscribing an existing member is still fine.
        assert!(router.subscribe("a", "doc1").await);
    }

    #[tokio::test]
    async fn test_subscribe_respects_document_cap() {
        let router = MessageRouter {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::new(),
            max_documents: 2,
            max_subscribers_per_doc: 100,
        };

        assert!(router.subscribe("a", "doc1").await);
        assert!(router.subscribe("a", "doc2").await);
        // Third distinct document exceeds the global document cap.
        assert!(!router.subscribe("a", "doc3").await);
        // Subscribing to an existing document is still fine.
        assert!(router.subscribe("b", "doc1").await);
    }
}
