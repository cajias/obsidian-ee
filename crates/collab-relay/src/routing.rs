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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use collab_proto::{DocumentId, ServerMessage, UserId};
use tokio::sync::RwLock;

use crate::relay::ClientHandle;
use crate::storage::OfflineQueue;

/// A per-document subscribe-authorization anchor (issue #29).
///
/// Public, non-secret data: the current MLS `epoch` and the `Ed25519`
/// verifying key derived from that epoch's exporter secret. The relay stores
/// this to verify [`SubscribeCapability`](collab_proto::SubscribeCapability)s
/// without ever running MLS or holding a group secret.
#[derive(Debug, Clone, Copy)]
pub struct DocAnchor {
    /// MLS epoch this anchor is bound to; capabilities must match it exactly.
    pub epoch: u64,
    /// `Ed25519` verifying key for this epoch (public, non-secret).
    pub verifying_key: [u8; 32],
}

/// Per-document subscribers and their content-authorization state (issue #72):
/// `doc_id` -> `user_id` -> `None` (handshake only) or `Some(anchor epoch)`.
type Subscriptions = Arc<RwLock<HashMap<DocumentId, HashMap<UserId, Option<u64>>>>>;

/// Routes messages to the appropriate subscribers.
pub struct MessageRouter {
    /// Document subscriptions: `doc_id` -> `user_id` -> content-authorization
    /// state (issue #72). `None` means subscribed for the MLS handshake only;
    /// `Some(epoch)` means a subscribe capability was verified at that anchor
    /// epoch. ONE collection, deliberately: a second "authorized" map would have
    /// to be kept in sync on unsubscribe / eviction and leaks authorization when
    /// the two drift. Retained across disconnect so offline subscribers can be
    /// queued for.
    subscriptions: Subscriptions,
    /// Per-document subscribe-authorization anchors (issue #29). Public
    /// verification data — never a group secret.
    anchors: Arc<RwLock<HashMap<DocumentId, DocAnchor>>>,
    /// Live client handles by user ID.
    clients: Arc<RwLock<HashMap<UserId, ClientHandle>>>,
    /// Buffer for messages to subscribed-but-disconnected users.
    offline: OfflineQueue,
    /// Maximum number of distinct documents tracked.
    max_documents: usize,
    /// Maximum number of subscribers per document.
    max_subscribers_per_doc: usize,
    /// When true, `YrsUpdate` fan-out is restricted to content-authorized
    /// subscribers (issue #72). Mirrors `RelayServer::require_subscribe_authz`,
    /// which sets it. Atomic so the flag can be set through the `Arc` the
    /// server already holds.
    content_gating: AtomicBool,
}

impl MessageRouter {
    /// Default maximum number of distinct documents tracked.
    pub const DEFAULT_MAX_DOCUMENTS: usize = 100_000;

    /// Default maximum number of subscribers per document.
    pub const DEFAULT_MAX_SUBSCRIBERS_PER_DOC: usize = 1_000;

    /// Largest epoch accepted for a FIRST (TOFU) anchor registration.
    ///
    /// A real MLS group starts at epoch 1 and advances by exactly 1 per commit,
    /// so a first-seen anchor claiming a huge epoch is bogus. Capping it blunts
    /// the `u64::MAX` pre-emption lockout: without this an attacker who won the
    /// TOFU race with `epoch == u64::MAX` would permanently reject every real
    /// (strictly-higher) rotation. A real group won't have rekeyed a million
    /// times, so this ceiling never bites a legitimate registrant.
    pub const MAX_INITIAL_ANCHOR_EPOCH: u64 = 1_000_000;

    /// Create a new message router with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            anchors: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::new(),
            max_documents: Self::DEFAULT_MAX_DOCUMENTS,
            max_subscribers_per_doc: Self::DEFAULT_MAX_SUBSCRIBERS_PER_DOC,
            content_gating: AtomicBool::new(false),
        }
    }

    /// Restrict `YrsUpdate` fan-out to content-authorized subscribers (#72).
    ///
    /// Off by default, so fan-out is unchanged unless the relay turns on
    /// subscribe authorization. See [`Self::route_message`].
    pub(crate) fn set_content_gating(&self, enabled: bool) {
        self.content_gating.store(enabled, Ordering::Relaxed);
    }

    /// Get the subscribe-authorization anchor for a document, if one is set.
    pub async fn get_anchor(&self, doc_id: &str) -> Option<DocAnchor> {
        self.anchors.read().await.get(doc_id).copied()
    }

    /// Set (or rotate) a document's subscribe-authorization anchor (issue #29).
    ///
    /// Accepts iff either:
    /// - **Rotation** of an existing anchor: `epoch` is strictly greater than the
    ///   current anchor's epoch (monotonic forward rotation). This reuses the
    ///   existing map slot, so it is always allowed regardless of the cap.
    /// - **First registration** (TOFU — first registration wins, like the relay's
    ///   first-Identify-wins for `user_id`): allowed only if the anchor map is
    ///   below `max_documents` AND `epoch <= MAX_INITIAL_ANCHOR_EPOCH`.
    ///
    /// Returns `false` on a stale/equal epoch, an implausibly large first epoch,
    /// or when the document cap is reached — leaving any existing anchor
    /// untouched. O(1). The caller MUST have already verified the registrant's
    /// self-proof under `verifying_key`; this method enforces monotonicity and
    /// the resource bounds only.
    ///
    /// Bounding the map by document count matters: `handle_register_doc_key` runs
    /// regardless of the subscribe-authz toggle, so without this cap any
    /// identified client could flood `RegisterDocKey` for unbounded distinct
    /// `doc_id`s and OOM the relay (CLAUDE.md resource-bounds rule).
    pub async fn set_anchor(&self, doc_id: &str, epoch: u64, verifying_key: [u8; 32]) -> bool {
        let mut anchors = self.anchors.write().await;
        let current_len = anchors.len();
        let accept = anchors.get(doc_id).map_or_else(
            // First (TOFU) registration: a NEW map entry — enforce the bounds.
            || self.accept_first_anchor(doc_id, epoch, current_len),
            // Rotation of an existing anchor: strictly-higher epoch only. Reuses
            // the existing map slot, so the document-count bound does not apply.
            |existing| epoch > existing.epoch,
        );
        if !accept {
            return false;
        }
        anchors.insert(doc_id.to_string(), DocAnchor { epoch, verifying_key });
        true
    }

    /// Whether a FIRST (TOFU) anchor for `doc_id` at `epoch` is acceptable given
    /// the current anchor-map size. Rejects when the document cap is reached (a
    /// resource bound — `handle_register_doc_key` runs regardless of the authz
    /// toggle, so an unbounded map would OOM) or when the first-seen epoch is
    /// implausibly large (blunts the `u64::MAX` pre-emption lockout).
    fn accept_first_anchor(&self, doc_id: &str, epoch: u64, current_len: usize) -> bool {
        if current_len >= self.max_documents {
            tracing::warn!(doc_id = %doc_id, "Rejecting RegisterDocKey: anchor document cap reached");
            return false;
        }
        if epoch > Self::MAX_INITIAL_ANCHOR_EPOCH {
            tracing::warn!(
                doc_id = %doc_id,
                epoch,
                "Rejecting RegisterDocKey: implausible first-anchor epoch"
            );
            return false;
        }
        true
    }

    /// Register a client for message routing.
    ///
    /// If a different connection is already registered for this user and
    /// `allow_takeover` is true, that older session is explicitly evicted (a
    /// best-effort [`ServerMessage::Error`] with
    /// [`collab_proto::ErrorCode::SessionReplaced`] is sent, then its connection
    /// is signalled to close). The newer connection then wins, which — combined
    /// with the connection-id check in [`Self::unregister_client`] — makes the
    /// reconnect path deterministic and free of the stale-cleanup race.
    ///
    /// If `allow_takeover` is false and a different live connection holds this
    /// user id, the existing session is left untouched and this returns `false`
    /// without registering. Under the shared-token model an unauthenticated peer
    /// must not be able to force-evict an arbitrary user.
    ///
    /// Returns `true` if `handle` is now the registered session.
    pub async fn register_client(&self, handle: ClientHandle, allow_takeover: bool) -> bool {
        let mut clients = self.clients.write().await;
        let has_stale_session = clients
            .get(&handle.user_id)
            .is_some_and(|previous| previous.conn_id() != handle.conn_id());

        // ponytail: no-auth mode permits self-takeover (no identity to protect); shared-token binding + session liveness detection (ping/pong or idle-read timeout to reap dead sessions promptly) deferred until multi-tenant auth exists
        if has_stale_session && !allow_takeover {
            tracing::warn!(
                user = %handle.user_id,
                "Refusing takeover of live session by unauthenticated Identify"
            );
            return false;
        }

        // A stale session reaching here means takeover is permitted: evict it.
        if let Some(previous) =
            clients.get(&handle.user_id).filter(|p| p.conn_id() != handle.conn_id())
        {
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
        true
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
    /// `authorized_epoch` is the subscription's content-authorization state
    /// (issue #72): `None` subscribes for the MLS handshake only, `Some(epoch)`
    /// records a capability verified against the doc's anchor at that epoch.
    ///
    /// Returns `false` (and does not subscribe) if a resource limit would be
    /// exceeded: the global document cap or the per-document subscriber cap.
    /// Re-subscribing an already-subscribed user is idempotent for cap
    /// accounting and returns `true`, but DOES overwrite the stored state — that
    /// is how a joiner upgrades `None` -> `Some(epoch)` once it has joined the
    /// group and can mint a capability.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // guard needed across all branches
    pub async fn subscribe(
        &self,
        user_id: &str,
        doc_id: &str,
        authorized_epoch: Option<u64>,
    ) -> bool {
        let mut subs = self.subscriptions.write().await;

        // Re-subscribing an existing member is idempotent, but re-states the
        // authorization: a fresh capability upgrades, a bare Subscribe downgrades.
        if let Some(state) = subs.get_mut(doc_id).and_then(|members| members.get_mut(user_id)) {
            *state = authorized_epoch;
            return true;
        }
        // A brand-new document counts against the global document cap.
        if !subs.contains_key(doc_id) && subs.len() >= self.max_documents {
            tracing::warn!(doc_id = %doc_id, "Rejecting subscribe: document cap reached");
            return false;
        }
        let members = subs.entry(doc_id.to_string()).or_default();
        // An existing document counts against the per-document subscriber cap.
        if members.len() >= self.max_subscribers_per_doc {
            tracing::warn!(doc_id = %doc_id, "Rejecting subscribe: per-document cap reached");
            return false;
        }
        members.insert(user_id.to_string(), authorized_epoch);
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

    /// Route a message to the eligible subscribers of a document, except the
    /// sender.
    ///
    /// Online subscribers are sent the message directly. Subscribers with no
    /// live connection have the message queued to the offline buffer for
    /// delivery on reconnect. A subscriber whose send channel is full is treated
    /// as a too-slow consumer: its connection is signalled to close and the
    /// message is queued for redelivery when it reconnects.
    ///
    /// With content gating on (issue #72) "eligible" narrows for
    /// [`ServerMessage::YrsUpdate`] only — see [`Self::recipients`]. Every other
    /// variant, `MlsHandshake` above all, always reaches every subscriber.
    ///
    /// Returns the number of clients the message was delivered to directly.
    pub async fn route_message(
        &self,
        doc_id: &str,
        from_user: &str,
        message: ServerMessage,
    ) -> usize {
        let Some(subscribers) = self.recipients(doc_id, from_user, &message).await else {
            return 0;
        };

        let (sent_count, offline, slow) = self.fan_out(&subscribers, &message).await;

        // Disconnect slow consumers so subsequent messages (and this one) are
        // buffered until they reconnect.
        if !slow.is_empty() {
            self.disconnect_slow(&slow).await;
        }

        // Enqueue for offline/slow subscribers. The offline queue may evict the
        // least-recently-seen user when full; that user's subscriptions must be
        // dropped too, otherwise a never-reconnecting user pins subscription
        // slots forever (the per-doc / global caps would fill with dead members).
        let mut evicted: Vec<UserId> = Vec::new();
        for subscriber_id in offline.iter().chain(slow.iter().map(|(id, _)| id)) {
            let user = self.offline.enqueue(subscriber_id, message.clone()).await;
            evicted.extend(user);
        }
        if !evicted.is_empty() {
            self.drop_subscriptions(&evicted).await;
        }

        sent_count
    }

    /// Remove `users` from every subscription set, pruning any document whose
    /// set becomes empty. Called when the offline queue evicts a user, since a
    /// user's subscription lifetime is tied to offline-queue retention.
    // ponytail: O(documents) scan per eviction; eviction is rare (only at
    // capacity), so a reverse user->docs index isn't worth the extra state.
    async fn drop_subscriptions(&self, users: &[UserId]) {
        let mut subs = self.subscriptions.write().await;
        subs.retain(|_doc, members| {
            members.retain(|member, _| !users.contains(member));
            !members.is_empty()
        });
    }

    /// Snapshot of the subscribers of `doc_id` eligible for `message`, excluding
    /// the sender. Returns `None` when nobody is eligible.
    ///
    /// With content gating off this is every subscriber, unchanged. With it on,
    /// a [`ServerMessage::YrsUpdate`] additionally requires the subscriber to
    /// have been authorized at the document's CURRENT anchor epoch. Comparing
    /// against the current anchor is what makes a rekey revoke: after a rotation
    /// to `N+1` a subscription stored at `N` no longer matches, with no extra
    /// bookkeeping. No anchor means nobody is content-authorized.
    ///
    /// Gating happens HERE, before [`Self::fan_out`], so an unauthorized
    /// subscriber is excluded from the offline queue too — it must not
    /// accumulate document content to be handed over on reconnect either.
    async fn recipients(
        &self,
        doc_id: &str,
        from_user: &str,
        message: &ServerMessage,
    ) -> Option<Vec<String>> {
        // Handshake traffic is never gated: a joiner must receive its Welcome
        // over the relay before it can become a member and mint a capability.
        let gated = self.content_gating.load(Ordering::Relaxed)
            && matches!(*message, ServerMessage::YrsUpdate { .. });
        // `?`: gated content for a doc with no anchor reaches nobody.
        let authorized = if gated { Some(self.get_anchor(doc_id).await?.epoch) } else { None };

        let subs = self.subscriptions.read().await;
        let result = subs.get(doc_id).map(|members| {
            members
                .iter()
                .filter(|(id, at)| id.as_str() != from_user && (!gated || **at == authorized))
                .map(|(id, _)| id.clone())
                .collect()
        });
        drop(subs);
        result
    }

    /// Attempt to deliver `message` to each subscriber, classifying them into
    /// `(sent_count, offline, slow)`. Slow subscribers carry the `conn_id` of the
    /// session that was slow, so [`Self::disconnect_slow`] can compare-and-remove
    /// and never evict a newer session that reconnected in the race window.
    async fn fan_out(
        &self,
        subscribers: &[String],
        message: &ServerMessage,
    ) -> (usize, Vec<String>, Vec<(String, u64)>) {
        let clients = self.clients.read().await;
        let mut sent_count = 0;
        let mut offline: Vec<String> = Vec::new();
        let mut slow: Vec<(String, u64)> = Vec::new();
        for subscriber_id in subscribers {
            match clients.get(subscriber_id) {
                Some(client) if client.send(message.clone()).is_ok() => sent_count += 1,
                Some(client) => slow.push((subscriber_id.clone(), client.conn_id())),
                None => offline.push(subscriber_id.clone()),
            }
        }
        (sent_count, offline, slow)
    }

    /// Remove and signal-close a set of too-slow consumers.
    ///
    /// Compare-and-remove by `conn_id` (like [`Self::unregister_client`]): a
    /// handle is only evicted if the stored session still matches the connection
    /// that was classified as slow, so a newer session that reconnected in the
    /// race window is never dropped.
    #[allow(clippy::excessive_nesting, clippy::significant_drop_tightening)]
    async fn disconnect_slow(&self, slow: &[(String, u64)]) {
        let mut clients = self.clients.write().await;
        for (subscriber_id, conn_id) in slow {
            if clients.get(subscriber_id).is_some_and(|h| h.conn_id() == *conn_id) {
                let handle = clients.remove(subscriber_id);
                tracing::warn!(subscriber = %subscriber_id, "Disconnecting slow consumer");
                if let Some(handle) = handle {
                    handle.signal_close();
                }
            }
        }
    }

    /// Get all subscribers for a document.
    #[cfg(test)]
    pub(crate) async fn get_subscribers(&self, doc_id: &str) -> Vec<String> {
        self.subscriptions
            .read()
            .await
            .get(doc_id)
            .map(|members| members.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Check if a user is subscribed to a document (at any authorization state).
    pub async fn is_subscribed(&self, user_id: &str, doc_id: &str) -> bool {
        self.subscriptions.read().await.get(doc_id).is_some_and(|subs| subs.contains_key(user_id))
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

        router.register_client(alice_handle, true).await;
        router.register_client(bob_handle, true).await;

        assert!(router.subscribe("alice", "doc1", None).await);
        assert!(router.subscribe("bob", "doc1", None).await);

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

        router.register_client(alice_handle, true).await;
        router.register_client(bob_handle, true).await;
        router.register_client(eve_handle, true).await;

        assert!(router.subscribe("alice", "doc1", None).await);
        assert!(router.subscribe("bob", "doc1", None).await);

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
        router.register_client(alice_handle, true).await;
        assert!(router.subscribe("alice", "doc1", None).await);

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

        router.register_client(alice_handle, true).await;
        router.register_client(bob_handle, true).await;

        assert!(router.subscribe("alice", "doc1", None).await);
        assert!(router.subscribe("bob", "doc1", None).await);

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
        router.register_client(alice_handle, true).await;

        assert!(router.subscribe("alice", "doc1", None).await);
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

        router.register_client(alice_handle, true).await;
        router.register_client(bob_handle, true).await;
        router.register_client(charlie_handle, true).await;

        assert!(router.subscribe("alice", "doc1", None).await);
        assert!(router.subscribe("bob", "doc1", None).await);
        assert!(router.subscribe("alice", "doc2", None).await);
        assert!(router.subscribe("charlie", "doc2", None).await);

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
        router.register_client(alice_handle, true).await;
        router.register_client(bob_handle, true).await;

        assert!(router.subscribe("alice", "doc1", None).await);
        assert!(router.subscribe("bob", "doc1", None).await);

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
        router.register_client(old_handle, true).await;
        router.register_client(new_handle, true).await;
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
            anchors: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::new(),
            max_documents: 100,
            max_subscribers_per_doc: 2,
            content_gating: AtomicBool::new(false),
        };

        assert!(router.subscribe("a", "doc1", None).await);
        assert!(router.subscribe("b", "doc1", None).await);
        // Third distinct subscriber exceeds the per-document cap.
        assert!(!router.subscribe("c", "doc1", None).await);
        // Re-subscribing an existing member is still fine.
        assert!(router.subscribe("a", "doc1", None).await);
    }

    #[tokio::test]
    async fn test_offline_eviction_prunes_subscriptions() {
        // Offline queue tracks at most one user; the second enqueue evicts the
        // first, whose subscriptions must then be dropped.
        let router = MessageRouter {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            anchors: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::with_limits(10, 1),
            max_documents: 100,
            max_subscribers_per_doc: 100,
            content_gating: AtomicBool::new(false),
        };

        // Two offline subscribers (no live handles registered).
        assert!(router.subscribe("u1", "doc1", None).await);
        assert!(router.subscribe("u2", "doc1", None).await);

        // A sender routes an update; both offline subscribers get enqueued.
        // The offline queue (cap 1) evicts whichever was enqueued first.
        let msg = ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "sender".into(),
            encrypted: vec![1],
            epoch: 1,
        };
        router.route_message("doc1", "sender", msg).await;

        // Exactly one subscriber survives, and it is the one still tracked by the
        // offline queue. The evicted user is pruned from both. (Fan-out iterates a
        // HashSet, so which of u1/u2 is evicted is not fixed — assert the
        // invariant, not a specific identity.)
        let u1_sub = router.is_subscribed("u1", "doc1").await;
        let u2_sub = router.is_subscribed("u2", "doc1").await;
        assert_ne!(u1_sub, u2_sub, "exactly one subscription must survive eviction");
        assert_eq!(
            router.is_subscribed("u1", "doc1").await,
            router.has_offline_messages("u1").await,
            "u1's subscription must track its offline-queue retention"
        );
        assert_eq!(
            router.is_subscribed("u2", "doc1").await,
            router.has_offline_messages("u2").await,
            "u2's subscription must track its offline-queue retention"
        );
    }

    #[tokio::test]
    async fn test_register_refuses_takeover_when_disallowed() {
        // Defense-in-depth guard: with takeover disallowed, a duplicate Identify
        // for a live user id is refused and the existing session keeps its slot.
        // (Unreachable from the relay in auth mode — the token check rejects an
        // unauthenticated Identify before register_client — but the guard stays.)
        let router = MessageRouter::new();

        let (first_handle, _first_rx) = create_test_client("alice", 1);
        let (second_handle, _second_rx) = create_test_client("alice", 2);
        assert!(router.register_client(first_handle, false).await);

        // The duplicate is refused; the original (conn 1) is left untouched.
        assert!(!router.register_client(second_handle, false).await);
        assert_eq!(router.client_count().await, 1);
        assert_eq!(router.get_client("alice").await.map(|h| h.conn_id()), Some(1));
    }

    #[tokio::test]
    async fn test_disconnect_slow_is_conn_id_scoped() {
        let router = MessageRouter::new();

        // A newer session (conn 2) holds the user id.
        let (new_handle, _new_rx) = create_test_client("alice", 2);
        router.register_client(new_handle, true).await;
        assert_eq!(router.client_count().await, 1);

        // A stale slow-classification (conn 1) must NOT evict the newer session.
        router.disconnect_slow(&[("alice".to_string(), 1)]).await;
        assert_eq!(router.client_count().await, 1);
        assert_eq!(router.get_client("alice").await.map(|h| h.conn_id()), Some(2));

        // A matching slow-classification (conn 2) does evict it.
        router.disconnect_slow(&[("alice".to_string(), 2)]).await;
        assert_eq!(router.client_count().await, 0);
    }

    #[tokio::test]
    async fn test_anchor_tofu_then_monotonic_rotation() {
        // TOFU: the first registration for a doc wins.
        let router = MessageRouter::new();
        assert!(router.get_anchor("doc1").await.is_none());
        assert!(router.set_anchor("doc1", 1, [1u8; 32]).await);
        let a = router.get_anchor("doc1").await.unwrap();
        assert_eq!(a.epoch, 1);
        assert_eq!(a.verifying_key, [1u8; 32]);

        // A strictly-higher epoch rotates the anchor.
        assert!(router.set_anchor("doc1", 2, [2u8; 32]).await);
        let a = router.get_anchor("doc1").await.unwrap();
        assert_eq!(a.epoch, 2);
        assert_eq!(a.verifying_key, [2u8; 32]);

        // A stale (lower) or equal epoch is rejected; the anchor is untouched.
        assert!(!router.set_anchor("doc1", 2, [3u8; 32]).await);
        assert!(!router.set_anchor("doc1", 1, [4u8; 32]).await);
        let a = router.get_anchor("doc1").await.unwrap();
        assert_eq!(a.epoch, 2);
        assert_eq!(a.verifying_key, [2u8; 32], "stale rotation must not overwrite the key");
    }

    #[tokio::test]
    async fn test_anchor_bounded_by_document_cap() {
        // The anchor map must be bounded like the subscription map, or any
        // identified client could flood RegisterDocKey for distinct doc_ids and
        // OOM the relay (handle_register_doc_key runs regardless of the toggle).
        let router = MessageRouter {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            anchors: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::new(),
            max_documents: 2,
            max_subscribers_per_doc: 100,
            content_gating: AtomicBool::new(false),
        };

        // Two distinct first-anchors fill the map to the cap.
        assert!(router.set_anchor("doc1", 1, [1u8; 32]).await);
        assert!(router.set_anchor("doc2", 1, [2u8; 32]).await);
        // A third distinct doc's first anchor exceeds the document cap.
        assert!(!router.set_anchor("doc3", 1, [3u8; 32]).await);
        assert!(router.get_anchor("doc3").await.is_none());

        // Rotating an EXISTING anchor at the cap still succeeds (no new entry).
        assert!(router.set_anchor("doc1", 2, [9u8; 32]).await);
        let a = router.get_anchor("doc1").await.unwrap();
        assert_eq!(a.epoch, 2);
        assert_eq!(a.verifying_key, [9u8; 32]);
    }

    #[tokio::test]
    async fn test_anchor_first_registration_epoch_is_capped() {
        // A first-seen anchor claiming an implausibly large epoch is rejected:
        // this blunts the u64::MAX pre-emption lockout (an attacker who won the
        // TOFU race at u64::MAX would otherwise reject every real rotation).
        let router = MessageRouter::new();
        assert!(
            !router.set_anchor("doc1", u64::MAX, [1u8; 32]).await,
            "a first anchor at u64::MAX must be rejected"
        );
        assert!(
            !router
                .set_anchor("doc1", MessageRouter::MAX_INITIAL_ANCHOR_EPOCH + 1, [1u8; 32])
                .await,
            "a first anchor just past the cap must be rejected"
        );
        assert!(router.get_anchor("doc1").await.is_none());

        // A first anchor exactly at the cap is accepted, and monotonic rotation
        // beyond the cap is still allowed (the ceiling gates only first-seen).
        assert!(
            router.set_anchor("doc1", MessageRouter::MAX_INITIAL_ANCHOR_EPOCH, [2u8; 32]).await
        );
        assert!(
            router.set_anchor("doc1", MessageRouter::MAX_INITIAL_ANCHOR_EPOCH + 1, [3u8; 32]).await,
            "monotonic rotation past the cap is still allowed for an existing anchor"
        );
        let a = router.get_anchor("doc1").await.unwrap();
        assert_eq!(a.epoch, MessageRouter::MAX_INITIAL_ANCHOR_EPOCH + 1);
    }

    #[tokio::test]
    async fn test_subscribe_respects_document_cap() {
        let router = MessageRouter {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            anchors: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            offline: OfflineQueue::new(),
            max_documents: 2,
            max_subscribers_per_doc: 100,
            content_gating: AtomicBool::new(false),
        };

        assert!(router.subscribe("a", "doc1", None).await);
        assert!(router.subscribe("a", "doc2", None).await);
        // Third distinct document exceeds the global document cap.
        assert!(!router.subscribe("a", "doc3", None).await);
        // Subscribing to an existing document is still fine.
        assert!(router.subscribe("b", "doc1", None).await);
    }

    // ---- Content gating (issue #72) -----------------------------------------

    /// A `YrsUpdate` for `doc1` from `alice`.
    fn update_for_doc1(data: u8) -> ServerMessage {
        ServerMessage::YrsUpdate {
            doc_id: "doc1".into(),
            from: "alice".into(),
            encrypted: vec![data],
            epoch: 1,
        }
    }

    /// NEGATIVE (a): with content gating on, a subscriber that presented no
    /// capability is subscribed for the HANDSHAKE ONLY. It must receive MLS
    /// handshake traffic (or a joiner could never receive its `Welcome` and the
    /// join deadlocks) and must receive NO document content — neither delivered
    /// live nor accumulated in the offline queue.
    #[tokio::test]
    async fn test_content_gating_withholds_yrs_update_from_unauthorized() {
        let router = MessageRouter::new();
        router.set_content_gating(true);
        assert!(router.set_anchor("doc1", 1, [1u8; 32]).await);

        let (member_handle, mut member_rx) = create_test_client("member", 1);
        let (joiner_handle, mut joiner_rx) = create_test_client("joiner", 2);
        router.register_client(member_handle, true).await;
        router.register_client(joiner_handle, true).await;

        assert!(router.subscribe("member", "doc1", Some(1)).await);
        // The join bootstrap: subscribed, no capability presented yet.
        assert!(router.subscribe("joiner", "doc1", None).await);
        // Same, but with no live connection — the offline-queue path.
        assert!(router.subscribe("offline-joiner", "doc1", None).await);

        // Content reaches the content-authorized member only.
        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(1)).await, 1);
        assert!(member_rx.try_recv().is_ok(), "an authorized subscriber still receives content");
        assert!(
            joiner_rx.try_recv().is_err(),
            "an unauthorized subscriber must receive no content"
        );
        assert!(
            !router.has_offline_messages("offline-joiner").await,
            "content must be filtered BEFORE fan-out: an unauthorized subscriber must not \
             accumulate queued content either"
        );

        // Handshake traffic is NOT gated: it reaches every subscriber, which is
        // what makes the MLS join bootstrap work under authz.
        let handshake = ServerMessage::MlsHandshake {
            doc_id: "doc1".into(),
            from: "alice".into(),
            payload: vec![9],
            message_type: collab_proto::MlsMessageType::Welcome,
        };
        assert_eq!(router.route_message("doc1", "alice", handshake).await, 2);
        assert!(joiner_rx.try_recv().is_ok(), "a handshake-only subscriber must get the Welcome");
        assert!(member_rx.try_recv().is_ok());
        assert!(router.has_offline_messages("offline-joiner").await);
    }

    /// NEGATIVE (b): authorization is compared against the doc's CURRENT anchor
    /// epoch, so a rekey revokes it. A subscriber verified at epoch 1 keeps its
    /// subscription across the rotation to epoch 2 but stops receiving content —
    /// no extra bookkeeping, and no window where a removed member keeps reading.
    #[tokio::test]
    async fn test_content_gating_rotation_revokes_stale_authorization() {
        let router = MessageRouter::new();
        router.set_content_gating(true);
        assert!(router.set_anchor("doc1", 1, [1u8; 32]).await);

        let (removed_handle, mut removed_rx) = create_test_client("removed", 1);
        router.register_client(removed_handle, true).await;
        assert!(router.subscribe("removed", "doc1", Some(1)).await);

        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(1)).await, 1);
        assert!(removed_rx.try_recv().is_ok(), "epoch-1 authorization delivers at epoch 1");

        // The group rekeys and the anchor rotates forward.
        assert!(router.set_anchor("doc1", 2, [2u8; 32]).await);

        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(2)).await, 0);
        assert!(
            removed_rx.try_recv().is_err(),
            "a capability verified at the OLD epoch must not receive post-rotation content"
        );
        assert!(!router.has_offline_messages("removed").await);
        assert!(
            router.is_subscribed("removed", "doc1").await,
            "the subscription itself survives: only content is withheld"
        );
    }

    /// A document with NO anchor has nobody content-authorized, so gated content
    /// goes nowhere — fail closed.
    ///
    /// `None` is the ONLY authorization state reachable on an unanchored doc:
    /// `subscribe(.., Some(epoch))` is reached solely from `relay.rs` after
    /// `authorize_subscribe` found an anchor, and anchors are never removed. So
    /// this pins the handshake-only subscriber — the case the fail-closed `?` in
    /// [`MessageRouter::recipients`] actually guards. Both a live and an offline
    /// subscriber, so the offline queue is covered too.
    #[tokio::test]
    async fn test_content_gating_without_anchor_withholds_from_handshake_only() {
        let router = MessageRouter::new();
        router.set_content_gating(true);

        let (handle, mut rx) = create_test_client("eve", 1);
        router.register_client(handle, true).await;
        assert!(router.subscribe("eve", "doc1", None).await);
        assert!(router.subscribe("offline-eve", "doc1", None).await);

        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(1)).await, 0);
        assert!(rx.try_recv().is_err(), "no anchor means nobody is content-authorized");
        assert!(
            !router.has_offline_messages("offline-eve").await,
            "an unauthorized subscriber must not accumulate queued content either"
        );
    }

    /// The other direction of the same re-statement: a BARE re-subscribe
    /// DOWNGRADES an authorized subscriber back to handshake-only. This is what a
    /// reconnecting client does if it forgets to re-present its capability — it
    /// silently stops receiving content, so a client that reconnects must mint
    /// and re-present, not just re-subscribe.
    #[tokio::test]
    async fn test_bare_resubscribe_downgrades_authorization() {
        let router = MessageRouter::new();
        router.set_content_gating(true);
        assert!(router.set_anchor("doc1", 1, [1u8; 32]).await);

        let (handle, mut rx) = create_test_client("member", 1);
        router.register_client(handle, true).await;
        assert!(router.subscribe("member", "doc1", Some(1)).await);
        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(1)).await, 1);
        assert!(rx.try_recv().is_ok());

        // Reconnect, re-subscribe, present nothing.
        assert!(router.subscribe("member", "doc1", None).await);

        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(2)).await, 0);
        assert!(
            rx.try_recv().is_err(),
            "a bare re-subscribe drops content authorization back to handshake-only"
        );
    }

    /// Re-subscribing re-states authorization: this is how a joiner upgrades from
    /// handshake-only to content-authorized once it can mint a capability.
    #[tokio::test]
    async fn test_resubscribe_upgrades_authorization() {
        let router = MessageRouter::new();
        router.set_content_gating(true);
        assert!(router.set_anchor("doc1", 1, [1u8; 32]).await);

        let (handle, mut rx) = create_test_client("joiner", 1);
        router.register_client(handle, true).await;

        assert!(router.subscribe("joiner", "doc1", None).await);
        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(1)).await, 0);

        // Joined, minted a capability, re-subscribed with it.
        assert!(router.subscribe("joiner", "doc1", Some(1)).await);
        assert_eq!(router.route_message("doc1", "alice", update_for_doc1(2)).await, 1);
        assert!(rx.try_recv().is_ok());
    }
}
