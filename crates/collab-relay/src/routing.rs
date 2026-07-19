//! Message routing between subscribed clients.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use collab_proto::{DocumentId, ServerMessage, UserId};
use tokio::sync::RwLock;

use crate::relay::ClientHandle;

/// Routes messages to the appropriate subscribers.
pub struct MessageRouter {
    /// Document subscriptions: `doc_id` -> set of `user_id`s.
    subscriptions: Arc<RwLock<HashMap<DocumentId, HashSet<UserId>>>>,
    /// Client handles by user ID.
    clients: Arc<RwLock<HashMap<UserId, ClientHandle>>>,
}

impl MessageRouter {
    /// Create a new message router.
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a client for message routing.
    pub async fn register_client(&self, handle: ClientHandle) {
        self.clients.write().await.insert(handle.user_id.clone(), handle);
    }

    /// Unregister a client.
    pub async fn unregister_client(&self, user_id: &str) {
        self.clients.write().await.remove(user_id);
        // Also remove from all subscriptions
        let mut subs = self.subscriptions.write().await;
        for subscribers in subs.values_mut() {
            subscribers.remove(user_id);
        }
    }

    /// Subscribe a user to a document.
    pub async fn subscribe(&self, user_id: &str, doc_id: &str) {
        self.subscriptions
            .write()
            .await
            .entry(doc_id.to_string())
            .or_default()
            .insert(user_id.to_string());
    }

    /// Unsubscribe a user from a document.
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
    /// Returns the number of clients the message was sent to.
    #[allow(clippy::excessive_nesting, clippy::significant_drop_tightening)]
    pub async fn route_message(
        &self,
        doc_id: &str,
        from_user: &str,
        message: ServerMessage,
    ) -> usize {
        let subscribers: Vec<String> = {
            let subs = self.subscriptions.read().await;
            match subs.get(doc_id) {
                Some(set) => set.iter().cloned().collect(),
                None => return 0,
            }
        };

        let clients = self.clients.read().await;
        let mut sent_count = 0;

        for subscriber_id in subscribers.iter().filter(|id| *id != from_user) {
            let Some(client) = clients.get(subscriber_id) else {
                continue;
            };
            if client.send(message.clone()).is_ok() {
                sent_count += 1;
            } else {
                tracing::warn!(
                    subscriber = %subscriber_id,
                    doc_id = %doc_id,
                    "Failed to route message to subscriber - channel closed"
                );
            }
        }

        sent_count
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
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tokio::sync::mpsc::unbounded_channel;

    fn create_test_client(user_id: &str) -> (ClientHandle, mpsc::UnboundedReceiver<ServerMessage>) {
        let (tx, rx) = unbounded_channel();
        let handle = ClientHandle::new(user_id.to_string(), tx);
        (handle, rx)
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_subscribe_and_receive() {
        let router = MessageRouter::new();

        // Create two clients: alice and bob
        let (alice_handle, _alice_rx) = create_test_client("alice");
        let (bob_handle, mut bob_rx) = create_test_client("bob");

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;

        // Both subscribe to doc1
        router.subscribe("alice", "doc1").await;
        router.subscribe("bob", "doc1").await;

        // Alice sends an update
        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 1);

        // Bob should receive it
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

        // Create three clients: alice, bob, eve
        let (alice_handle, _alice_rx) = create_test_client("alice");
        let (bob_handle, mut bob_rx) = create_test_client("bob");
        let (eve_handle, mut eve_rx) = create_test_client("eve");

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;
        router.register_client(eve_handle).await;

        // Alice and Bob subscribe to doc1, Eve does NOT
        router.subscribe("alice", "doc1").await;
        router.subscribe("bob", "doc1").await;
        // Eve is NOT subscribed

        // Alice sends an update
        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 1); // Only Bob should receive

        // Bob should receive it
        assert!(bob_rx.try_recv().is_ok());

        // Eve should NOT receive it
        assert!(eve_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_sender_does_not_receive_own_message() {
        let router = MessageRouter::new();

        let (alice_handle, mut alice_rx) = create_test_client("alice");
        router.register_client(alice_handle).await;
        router.subscribe("alice", "doc1").await;

        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 0); // No one else subscribed

        // Alice should NOT receive her own message
        assert!(alice_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let router = MessageRouter::new();

        let (alice_handle, _alice_rx) = create_test_client("alice");
        let (bob_handle, mut bob_rx) = create_test_client("bob");

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;

        router.subscribe("alice", "doc1").await;
        router.subscribe("bob", "doc1").await;

        // Bob unsubscribes
        router.unsubscribe("bob", "doc1").await;

        let message = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
        };

        let sent = router.route_message("doc1", "alice", message).await;
        assert_eq!(sent, 0); // Bob unsubscribed

        // Bob should NOT receive it
        assert!(bob_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_multiple_documents() {
        let router = MessageRouter::new();

        let (alice_handle, _alice_rx) = create_test_client("alice");
        let (bob_handle, mut bob_rx) = create_test_client("bob");
        let (charlie_handle, mut charlie_rx) = create_test_client("charlie");

        router.register_client(alice_handle).await;
        router.register_client(bob_handle).await;
        router.register_client(charlie_handle).await;

        // Alice and Bob on doc1, Alice and Charlie on doc2
        router.subscribe("alice", "doc1").await;
        router.subscribe("bob", "doc1").await;
        router.subscribe("alice", "doc2").await;
        router.subscribe("charlie", "doc2").await;

        // Alice sends to doc1
        let msg1 = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![1],
            epoch: 1,
        };
        router.route_message("doc1", "alice", msg1).await;

        // Bob gets it, Charlie doesn't
        assert!(bob_rx.try_recv().is_ok());
        assert!(charlie_rx.try_recv().is_err());

        // Alice sends to doc2
        let msg2 = ServerMessage::YrsUpdate {
            doc_id: "doc2".into(),
            from: "alice".into(),
            encrypted: vec![2],
            epoch: 1,
        };
        router.route_message("doc2", "alice", msg2).await;

        // Charlie gets it, Bob doesn't
        assert!(charlie_rx.try_recv().is_ok());
        assert!(bob_rx.try_recv().is_err());
    }
}
