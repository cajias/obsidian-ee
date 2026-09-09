//! Storage for offline message queuing.
//!
//! Messages destined for a subscribed-but-disconnected user are buffered here
//! and drained when the user reconnects, so briefly-offline peers do not miss
//! updates and handshakes while away.
//!
//! This is an in-memory implementation. Memory is bounded on three axes to keep
//! the zero-knowledge relay safe from a client that queues without ever
//! reconnecting:
//! - `max_per_user` caps the messages retained for a single user (oldest first).
//! - `max_users` caps the number of distinct users tracked; when full, the
//!   least-recently-inserted user's queue is evicted.
//! - `ByteLimits` caps the retained payload bytes. Without a byte ceiling the
//!   per-message and per-user count caps still allow
//!   `max_users * max_per_user * MAX_MESSAGE_SIZE` (~1 TiB) of retained memory,
//!   because subscriptions survive disconnect: an attacker can amass many
//!   offline-but-subscribed user ids on a document and push max-size frames to
//!   each. The byte budget is the ceiling that actually prevents OOM.
//!
//! The byte ceiling is split two ways, because a single first-come-first-served
//! budget starves whoever asks second. NEITHER kind is safely droppable — see
//! `Kind` — so the split is not about which loss is tolerable:
//! - **By kind.** `YrsUpdate` content and `MlsHandshake` traffic get separate
//!   budgets so that a flood of one cannot consume the budget the other needs.
//!   Handshake traffic is ungated — any identified client can broadcast it to
//!   any document (`Router::recipients`) — so on a shared budget that flood
//!   evicts queued content.
//! - **By user.** Each user has its own per-kind cap, and admitting a message
//!   over budget evicts the OLDEST message of that kind from whichever user
//!   holds the MOST of it, rather than refusing the newcomer. A modest queue is
//!   therefore never the victim while a bigger consumer exists.
//!
//! Largest-queue eviction also recovers better than refusing the newcomer did.
//! A `YrsUpdate` carries the FULL cumulative document state (`Document::
//! encode_state` encodes against a default `StateVector`), so a dropped content
//! frame is healed by the next frame from any peer holding the change — as long
//! as that frame is itself admitted. Under refuse-the-newcomer the byte cap was
//! a global saturation condition: the pressure that dropped frame N persisted
//! into N+1 and blocked the very frame that would have closed the gap. Now the
//! hog is trimmed and the healing frame gets in.
//!
//! A `DynamoDB`-backed implementation can be introduced later behind a Cargo
//! feature (see `Cargo.toml`).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use collab_proto::{ServerMessage, UserId};
use tokio::sync::RwLock;

/// Which byte budget a message is charged against.
///
/// Both budgeted kinds carry drops that hurt, so neither budget may be spent on
/// the other's traffic:
/// - A dropped `YrsUpdate` is healed by the NEXT content frame, because each one
///   is a full cumulative snapshot — but only once some peer edits again.
/// - A dropped `MlsHandshake` recovers only for a `KeyPackage`, which the join
///   timer re-sends. A lost `Welcome` strands the joiner until an owner re-invites
///   it (it has already freed the key package holding its leaf private key —
///   `plugins/obsidian-ee/src/collab-client.ts`), and a lost `Commit` leaves
///   members behind the epoch advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `YrsUpdate` — document content.
    Content,
    /// `MlsHandshake` — group handshake traffic.
    Handshake,
    /// Fixed-size control frames: negligible, never budgeted.
    Unbudgeted,
}

const fn message_kind(message: &ServerMessage) -> Kind {
    match message {
        ServerMessage::YrsUpdate { .. } => Kind::Content,
        ServerMessage::MlsHandshake { .. } => Kind::Handshake,
        _ => Kind::Unbudgeted,
    }
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

/// One user's queue plus its per-kind byte counters.
///
/// The counters live next to the deque they describe, rather than in a parallel
/// map, so they cannot drift out of sync with it.
#[derive(Default)]
struct UserQueue {
    /// A SINGLE deque across both kinds, so [`OfflineQueue::drain`] stays FIFO
    /// in wire order and no client ordering assumption changes.
    messages: VecDeque<ServerMessage>,
    content_bytes: usize,
    handshake_bytes: usize,
}

impl UserQueue {
    const fn bytes(&self, kind: Kind) -> usize {
        match kind {
            Kind::Content => self.content_bytes,
            Kind::Handshake => self.handshake_bytes,
            Kind::Unbudgeted => 0,
        }
    }

    const fn charge(&mut self, kind: Kind, bytes: usize) {
        match kind {
            Kind::Content => self.content_bytes += bytes,
            Kind::Handshake => self.handshake_bytes += bytes,
            Kind::Unbudgeted => {}
        }
    }

    const fn credit(&mut self, kind: Kind, bytes: usize) {
        match kind {
            Kind::Content => self.content_bytes -= bytes,
            Kind::Handshake => self.handshake_bytes -= bytes,
            Kind::Unbudgeted => {}
        }
    }
}

/// Internal, lock-guarded state for [`OfflineQueue`].
#[derive(Default)]
struct Inner {
    /// Queued messages per user, with that user's byte counters.
    queues: HashMap<UserId, UserQueue>,
    /// Insertion order of the currently-tracked users, used to evict the
    /// least-recently-inserted user when `max_users` is exceeded. Kept in sync
    /// with `queues`: every key in `queues` appears exactly once here.
    order: VecDeque<UserId>,
    /// Running sums of [`message_bytes`] per kind over every queued message.
    /// Maintained incrementally: charged on enqueue, credited back on every
    /// removal (drain, count-cap drop, budget eviction, user eviction). Never
    /// recomputed by scanning, so charge/credit stays O(1).
    content_bytes: usize,
    handshake_bytes: usize,
}

impl Inner {
    const fn bytes(&self, kind: Kind) -> usize {
        match kind {
            Kind::Content => self.content_bytes,
            Kind::Handshake => self.handshake_bytes,
            Kind::Unbudgeted => 0,
        }
    }

    const fn charge(&mut self, kind: Kind, bytes: usize) {
        match kind {
            Kind::Content => self.content_bytes += bytes,
            Kind::Handshake => self.handshake_bytes += bytes,
            Kind::Unbudgeted => {}
        }
    }

    const fn credit(&mut self, kind: Kind, bytes: usize) {
        match kind {
            Kind::Content => self.content_bytes -= bytes,
            Kind::Handshake => self.handshake_bytes -= bytes,
            Kind::Unbudgeted => {}
        }
    }

    /// Remove the message at `idx` of `user`'s queue, crediting BOTH the
    /// per-user and aggregate counters. The single removal path is what keeps
    /// the two from drifting apart.
    fn remove_at(&mut self, user: &str, idx: usize) -> bool {
        let Some(queue) = self.queues.get_mut(user) else { return false };
        let Some(message) = queue.messages.remove(idx) else { return false };
        let kind = message_kind(&message);
        let bytes = message_bytes(&message);
        queue.credit(kind, bytes);
        self.credit(kind, bytes);
        true
    }

    /// Drop `user`'s oldest message of `kind`. Returns false when it holds none.
    // ponytail: O(queue) walk to find the oldest message of one kind, because a
    // single deque is what keeps drain FIFO across kinds. Only runs under budget
    // pressure. Upgrade: a per-kind index of deque positions if eviction stops
    // being rare.
    fn drop_oldest_of(&mut self, user: &str, kind: Kind) -> bool {
        let idx = self
            .queues
            .get(user)
            .and_then(|q| q.messages.iter().position(|m| message_kind(m) == kind));
        idx.is_some_and(|i| self.remove_at(user, i))
    }

    /// The user holding the most `kind` bytes, if any holds more than zero.
    // ponytail: O(users) scan per evicted message. Only runs under budget
    // pressure; the happy path stays O(1). Upgrade: a per-kind max-heap keyed by
    // bytes if the relay routinely sits at its budget.
    fn largest_holder(&self, kind: Kind) -> Option<UserId> {
        self.queues
            .iter()
            .filter(|(_, q)| q.bytes(kind) > 0)
            .max_by_key(|(_, q)| q.bytes(kind))
            .map(|(user, _)| user.clone())
    }
}

/// Drop `user`'s oldest messages until the queue holds at most `max_per_user`,
/// returning how many were dropped.
fn trim_to_cap(inner: &mut Inner, user: &str, max_per_user: usize) -> usize {
    let mut dropped = 0;
    while inner.queues.get(user).is_some_and(|q| q.messages.len() > max_per_user) {
        if !inner.remove_at(user, 0) {
            break;
        }
        dropped += 1;
    }
    dropped
}

/// Drop `user`'s oldest `kind` messages until `needed` more bytes fit under the
/// per-user cap, returning how many were dropped.
fn trim_user_budget(inner: &mut Inner, user: &str, kind: Kind, needed: usize, cap: usize) -> usize {
    let held = |inner: &Inner| inner.queues.get(user).map_or(0, |q| q.bytes(kind));
    let mut dropped = 0;
    while held(inner) + needed > cap {
        if !inner.drop_oldest_of(user, kind) {
            break;
        }
        dropped += 1;
    }
    dropped
}

/// Free space in the `kind` budget until `needed` more bytes fit, by repeatedly
/// dropping the OLDEST message of that kind from whichever user holds the MOST
/// of it. Returns how many messages were dropped.
///
/// The caller guarantees `needed <= budget`, so this always makes room: the
/// aggregate counter is the sum of the per-user ones, hence it reaches zero
/// before [`Inner::largest_holder`] runs out of victims. A user may be left with
/// an empty queue; that is harmless — `drain` and the `max_users` eviction both
/// clean the entry up, and `has_messages` already reports it as empty.
fn make_room(inner: &mut Inner, kind: Kind, needed: usize, budget: usize) -> usize {
    let mut dropped = 0;
    while inner.bytes(kind) + needed > budget {
        let Some(victim) = inner.largest_holder(kind) else { break };
        if !inner.drop_oldest_of(&victim, kind) {
            break;
        }
        dropped += 1;
    }
    dropped
}

/// Evict the least-recently-inserted user's queue while at or above capacity.
///
/// Returns the evicted user id, if any, so the caller can prune that user's
/// subscriptions — offline-queue retention is what keeps a subscription alive.
fn evict_if_full(inner: &mut Inner, max_users: usize) -> Option<UserId> {
    while inner.queues.len() >= max_users {
        let oldest = inner.order.pop_front()?;
        if let Some(queue) = inner.queues.remove(&oldest) {
            inner.content_bytes -= queue.content_bytes;
            inner.handshake_bytes -= queue.handshake_bytes;
            tracing::warn!(evicted = %oldest, "Offline queue at capacity; evicted oldest user");
            return Some(oldest);
        }
    }
    None
}

/// What an [`OfflineQueue::enqueue`] did, so callers can log the loss.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct EnqueueOutcome {
    /// Whole queue evicted at `max_users` capacity; caller must prune that
    /// user's subscriptions.
    pub(crate) evicted_user: Option<UserId>,
    /// The message could not be queued at all.
    pub(crate) refused: bool,
    /// Previously-queued messages dropped under BYTE-budget pressure (per-user
    /// or aggregate). Abnormal: it means the relay is at a resource limit.
    pub(crate) displaced: usize,
    /// Previously-queued messages dropped by the `max_per_user` COUNT ring.
    /// Routine: any long-offline user hits this on every further message once
    /// their queue is full, so it must never be conflated with `displaced` —
    /// otherwise a healthy steady state looks identical to real contention.
    /// Still real loss (see [`Kind`]), so it is still reported here.
    pub(crate) trimmed: usize,
}

/// Byte budgets for [`OfflineQueue`], split by message kind.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ByteLimits {
    /// Aggregate `YrsUpdate` payload bytes retained across all users.
    pub(crate) content: usize,
    /// Aggregate `MlsHandshake` payload bytes retained across all users.
    pub(crate) handshake: usize,
    /// `YrsUpdate` payload bytes retained for any single user.
    pub(crate) user_content: usize,
    /// `MlsHandshake` payload bytes retained for any single user.
    pub(crate) user_handshake: usize,
}

impl ByteLimits {
    /// Aggregate budget for `kind`. `Unbudgeted` messages are never charged, so
    /// their budget is irrelevant; `usize::MAX` keeps every check trivially true.
    const fn budget(&self, kind: Kind) -> usize {
        match kind {
            Kind::Content => self.content,
            Kind::Handshake => self.handshake,
            Kind::Unbudgeted => usize::MAX,
        }
    }

    /// Per-user cap for `kind`.
    const fn per_user(&self, kind: Kind) -> usize {
        match kind {
            Kind::Content => self.user_content,
            Kind::Handshake => self.user_handshake,
            Kind::Unbudgeted => usize::MAX,
        }
    }
}

impl Default for ByteLimits {
    fn default() -> Self {
        Self {
            content: OfflineQueue::DEFAULT_MAX_CONTENT_BYTES,
            handshake: OfflineQueue::DEFAULT_MAX_HANDSHAKE_BYTES,
            user_content: OfflineQueue::DEFAULT_MAX_USER_CONTENT_BYTES,
            user_handshake: OfflineQueue::DEFAULT_MAX_USER_HANDSHAKE_BYTES,
        }
    }
}

/// Stores messages for offline clients.
pub struct OfflineQueue {
    inner: Arc<RwLock<Inner>>,
    /// Maximum messages to store per user (prevents unbounded per-user growth).
    max_per_user: usize,
    /// Maximum number of distinct users tracked (prevents unbounded key growth).
    max_users: usize,
    /// Payload byte budgets, split per kind and per user (prevents OOM and
    /// starvation; see the module docs for what this closes).
    bytes: ByteLimits,
}

impl OfflineQueue {
    /// Default maximum messages per user.
    pub const DEFAULT_MAX_PER_USER: usize = 1000;

    /// Default maximum number of distinct users tracked.
    pub const DEFAULT_MAX_USERS: usize = 10_000;

    /// Total payload byte ceiling: 128 MiB, split between
    /// `DEFAULT_MAX_CONTENT_BYTES` and
    /// `DEFAULT_MAX_HANDSHAKE_BYTES`, which are derived from it so the
    /// sum stays exact.
    ///
    /// Sized to comfortably hold a realistic burst — several hundred briefly-
    /// offline users each holding a max-size frame — yet orders of magnitude
    /// below what the count caps alone would permit, so a single relay process
    /// cannot be driven to OOM by retained offline messages. Note the budget
    /// charges DECODED `payload.len()`, not the wire size: frames are JSON text
    /// and a `Vec<u8>` payload serializes as a number array, so a 1 MiB
    /// `MAX_MESSAGE_SIZE` frame carries only ~300 KiB of payload.
    pub const DEFAULT_MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;

    /// Default `MlsHandshake` byte budget: 16 MiB.
    ///
    /// Handshake traffic is ungated — any identified client can broadcast it to
    /// any document (`Router::recipients`) — so it is the flood risk, and gets
    /// the smaller slice of [`Self::DEFAULT_MAX_TOTAL_BYTES`]. Sized for real
    /// handshake volume, which is a few frames per join, not per keystroke.
    pub(crate) const DEFAULT_MAX_HANDSHAKE_BYTES: usize = 16 * 1024 * 1024;

    /// Default `YrsUpdate` byte budget: the rest of the total, 112 MiB.
    ///
    /// Content is the high-volume kind — one frame per edit burst per document,
    /// each a full state snapshot — so it takes the larger slice. Being its own
    /// budget is what stops a handshake flood from evicting it.
    pub(crate) const DEFAULT_MAX_CONTENT_BYTES: usize =
        Self::DEFAULT_MAX_TOTAL_BYTES - Self::DEFAULT_MAX_HANDSHAKE_BYTES;

    /// Default per-user `YrsUpdate` byte cap: 8 MiB.
    pub(crate) const DEFAULT_MAX_USER_CONTENT_BYTES: usize = 8 * 1024 * 1024;

    /// Default per-user `MlsHandshake` byte cap: 1 MiB.
    pub(crate) const DEFAULT_MAX_USER_HANDSHAKE_BYTES: usize = 1024 * 1024;

    /// Create a new offline queue with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            max_per_user: Self::DEFAULT_MAX_PER_USER,
            max_users: Self::DEFAULT_MAX_USERS,
            bytes: ByteLimits::default(),
        }
    }

    /// Create a new offline queue with custom per-user and user-count caps.
    ///
    /// The byte budgets stay at their `ByteLimits` defaults.
    #[must_use]
    pub fn with_limits(max_per_user: usize, max_users: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            max_per_user,
            max_users,
            bytes: ByteLimits::default(),
        }
    }

    /// Create a new offline queue with custom per-user, user-count, and
    /// byte caps.
    #[cfg(test)]
    pub(crate) fn with_byte_limits(
        max_per_user: usize,
        max_users: usize,
        bytes: ByteLimits,
    ) -> Self {
        Self { inner: Arc::new(RwLock::new(Inner::default())), max_per_user, max_users, bytes }
    }

    /// Queue a message for an offline user.
    ///
    /// The message is admitted unless it alone exceeds a budget for its kind
    /// (see [`ByteLimits`]); making room for it costs OTHER messages of the same
    /// kind, taken oldest-first from whichever user holds the most of that kind.
    /// A user's queue is also trimmed to `max_per_user` messages, and tracking a
    /// new user at `max_users` capacity evicts the least-recently-inserted one.
    ///
    /// EVERY drop is reported in the returned [`EnqueueOutcome`] — byte-budget
    /// evictions (`displaced`) and `max_per_user` count-cap drops (`trimmed`)
    /// alike — because none of them is safe to lose silently (see [`Kind`]) and
    /// the protocol has no retransmit request: recovery depends entirely on a
    /// later frame arriving. The two are kept apart because they mean different
    /// things: `displaced` signals resource contention, `trimmed` is routine for
    /// any long-offline user. Callers are expected to log both, at different
    /// severities. `evicted_user` additionally requires the router to prune
    /// that user's subscriptions, so a never-reconnecting user cannot pin
    /// subscription slots forever.
    pub(crate) async fn enqueue(&self, user_id: &str, message: ServerMessage) -> EnqueueOutcome {
        let kind = message_kind(&message);
        let msg_bytes = message_bytes(&message);
        let mut inner = self.inner.write().await;

        // Refuse only what can never fit: a message over its own kind's per-user
        // cap or aggregate budget. Both are checked before anything is mutated,
        // so a refusal costs no other user data or subscriptions — and because
        // the message does fit past this point, the eviction loops below are
        // guaranteed to make room for it.
        if msg_bytes > self.bytes.per_user(kind) || msg_bytes > self.bytes.budget(kind) {
            drop(inner);
            return EnqueueOutcome { refused: true, ..EnqueueOutcome::default() };
        }

        let mut displaced =
            trim_user_budget(&mut inner, user_id, kind, msg_bytes, self.bytes.per_user(kind));
        displaced += make_room(&mut inner, kind, msg_bytes, self.bytes.budget(kind));

        let evicted = if inner.queues.contains_key(user_id) {
            None
        } else {
            let evicted = evict_if_full(&mut inner, self.max_users);
            inner.order.push_back(user_id.to_string());
            evicted
        };

        inner.charge(kind, msg_bytes);
        let queue = inner.queues.entry(user_id.to_string()).or_default();
        queue.charge(kind, msg_bytes);
        queue.messages.push_back(message);
        let trimmed = trim_to_cap(&mut inner, user_id, self.max_per_user);
        drop(inner);
        EnqueueOutcome { evicted_user: evicted, refused: false, displaced, trimmed }
    }

    /// Retrieve and clear all queued messages for a user.
    ///
    /// Returns messages in the order they were queued (FIFO).
    pub async fn drain(&self, user_id: &str) -> Vec<ServerMessage> {
        let mut inner = self.inner.write().await;
        let drained = inner.queues.remove(user_id);
        if let Some(queue) = drained.as_ref() {
            inner.content_bytes -= queue.content_bytes;
            inner.handshake_bytes -= queue.handshake_bytes;
            inner.order.retain(|u| u != user_id);
        }
        drop(inner);
        drained.map_or_else(Vec::new, |q| q.messages.into())
    }

    /// Check if there are queued messages for a user.
    pub async fn has_messages(&self, user_id: &str) -> bool {
        let inner = self.inner.read().await;
        inner.queues.get(user_id).is_some_and(|q| !q.messages.is_empty())
    }

    /// Get the number of queued messages for a user.
    #[cfg(test)]
    pub async fn message_count(&self, user_id: &str) -> usize {
        let inner = self.inner.read().await;
        inner.queues.get(user_id).map_or(0, |q| q.messages.len())
    }

    /// Get the number of distinct users currently tracked.
    #[cfg(test)]
    pub async fn tracked_users(&self) -> usize {
        let inner = self.inner.read().await;
        inner.queues.len()
    }

    /// Get the running aggregate payload-byte total across both budgets.
    #[cfg(test)]
    pub async fn total_bytes(&self) -> usize {
        let inner = self.inner.read().await;
        inner.content_bytes + inner.handshake_bytes
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

    /// An `MlsHandshake` whose payload is exactly `bytes` long.
    fn make_handshake(doc_id: &str, from: &str, bytes: usize) -> ServerMessage {
        ServerMessage::MlsHandshake {
            doc_id: doc_id.into(),
            from: from.into(),
            payload: vec![0u8; bytes],
            message_type: collab_proto::MlsMessageType::Commit,
        }
    }

    /// Byte limits with every budget set to `n`, so a test's numbers stay small
    /// and only the axis it exercises can bite.
    const fn flat_limits(n: usize) -> ByteLimits {
        ByteLimits { content: n, handshake: n, user_content: n, user_handshake: n }
    }

    fn count_kinds(messages: &[ServerMessage]) -> (usize, usize) {
        let content =
            messages.iter().filter(|m| matches!(m, ServerMessage::YrsUpdate { .. })).count();
        let handshake =
            messages.iter().filter(|m| matches!(m, ServerMessage::MlsHandshake { .. })).count();
        (content, handshake)
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

        // Queue 5 messages for bob (exceeds limit of 3). The count cap is a
        // silent drop path unless it is reported, so collect what it reports.
        let mut trimmed = Vec::new();
        for i in 1..=5 {
            let outcome = queue.enqueue("bob", make_update("doc1", "alice", i)).await;
            assert_eq!(
                outcome.displaced, 0,
                "a count-cap drop must not be reported as byte pressure"
            );
            trimmed.push(outcome.trimmed);
        }
        assert_eq!(trimmed, vec![0, 0, 0, 1, 1], "count-cap drops must be reported too");

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
    async fn test_byte_budget_evicts_largest_queue() {
        // Budget for 3 messages of 100 bytes; plenty of per-user/user headroom
        // so only the byte cap can bite.
        let queue = OfflineQueue::with_byte_limits(1000, 1000, flat_limits(300));

        // Three 100-byte messages across two users exactly fill the budget.
        queue.enqueue("alice", make_sized("doc1", "x", 100)).await;
        queue.enqueue("alice", make_sized("doc1", "x", 100)).await;
        queue.enqueue("bob", make_sized("doc1", "x", 100)).await;
        assert_eq!(queue.total_bytes().await, 300);

        // The fourth is admitted by evicting the OLDEST message of the LARGEST
        // queue (alice, 200 bytes) — the newcomer is not the victim.
        let outcome = queue.enqueue("carol", make_sized("doc1", "x", 100)).await;
        assert!(!outcome.refused, "a modest newcomer must not be refused");
        assert_eq!(outcome.displaced, 1);
        assert!(queue.has_messages("carol").await, "newcomer must be queued");
        assert_eq!(queue.message_count("alice").await, 1, "largest queue gives up its oldest");
        assert!(queue.has_messages("bob").await, "a smaller queue is never the victim");
        assert_eq!(queue.total_bytes().await, 300, "byte budget must hold");

        // A message that alone exceeds the budget is refused, and mutates nothing.
        let too_big = queue.enqueue("dave", make_sized("doc1", "x", 301)).await;
        assert!(too_big.refused);
        assert_eq!(too_big.displaced, 0, "a refusal must not displace anything");
        assert_eq!(queue.total_bytes().await, 300);
        assert!(!queue.has_messages("dave").await, "refused message must not be stored");
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_byte_accounting_survives_count_cap_and_eviction() {
        // Per-user cap of 2 messages, 2 users max, generous byte budget.
        let queue = OfflineQueue::with_byte_limits(2, 2, flat_limits(1_000_000));

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
        assert_eq!(evicted.evicted_user, Some("alice".to_string()));
        // alice's 200 bytes credited on eviction; bob(100) + carol(100) remain.
        assert_eq!(queue.total_bytes().await, 200, "user eviction must credit bytes");
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_one_user_cannot_starve_another() {
        let queue = OfflineQueue::with_byte_limits(1000, 1000, flat_limits(300));

        // Alice alone fills the whole content budget.
        for _ in 0..3 {
            queue.enqueue("alice", make_sized("doc1", "x", 100)).await;
        }
        assert_eq!(queue.total_bytes().await, 300);

        // Bob's first, modest message must still be accepted — his ACCEPTANCE
        // is the discriminator, not alice's trimming.
        let outcome = queue.enqueue("bob", make_sized("doc1", "x", 100)).await;
        assert!(!outcome.refused, "a full budget must not starve a new recipient");
        assert!(queue.has_messages("bob").await, "bob must be queued");
        // And alice keeps most of hers: the budget is shared, not handed over.
        assert!(queue.has_messages("alice").await, "alice must not be wholly evicted");
        assert_eq!(queue.message_count("alice").await, 2);
        assert_eq!(queue.total_bytes().await, 300, "byte budget must hold");
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_handshake_flood_cannot_evict_content() {
        let queue = OfflineQueue::with_byte_limits(1000, 1000, flat_limits(300));

        // A handshake flood saturates the handshake budget...
        for _ in 0..10 {
            queue.enqueue("bob", make_handshake("doc1", "x", 100)).await;
        }
        // ...which must leave the content budget untouched.
        let outcome = queue.enqueue("bob", make_sized("doc1", "x", 100)).await;
        assert!(!outcome.refused, "a handshake flood must not refuse content");
        // Flooding again must not reach the content already queued.
        for _ in 0..10 {
            queue.enqueue("bob", make_handshake("doc1", "x", 100)).await;
        }

        let (content, handshakes) = count_kinds(&queue.drain("bob").await);
        assert_eq!(content, 1, "content must survive a handshake flood");
        // Positive half: the flood demonstrably happened and stayed bounded, so
        // the surviving update is evidence of a split budget, not an empty queue.
        assert_eq!(handshakes, 3, "handshakes are retained up to their own budget");
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_handshake_budget_is_separate_from_content() {
        // Asymmetric aggregate budgets, and per-user caps set well above what
        // any single flooding user sends — so only the AGGREGATE handshake
        // budget, not a per-user cap or the content budget, can be the
        // limiter below.
        let limits = ByteLimits { handshake: 300, user_handshake: 1000, ..flat_limits(100_000) };
        let queue = OfflineQueue::with_byte_limits(1000, 1000, limits);

        // A content message queued before the flood must survive it untouched.
        queue.enqueue("carl", make_sized("doc1", "x", 100)).await;
        assert_eq!(queue.total_bytes().await, 100);

        // Flood from 5 distinct users, 3 messages of 100 bytes each: 1500
        // bytes sent, far over the 300-byte handshake budget, while each
        // user's own 300 bytes stays well under its 1000-byte cap — no
        // single per-user cap can be what bounds this.
        for user in ["hs0", "hs1", "hs2", "hs3", "hs4"] {
            for _ in 0..3 {
                queue.enqueue(user, make_handshake("doc1", "x", 100)).await;
            }
        }

        // Aggregate handshake bytes must settle at the HANDSHAKE budget
        // (300), not the much larger content budget: subtracting the
        // untouched content bytes isolates the handshake side.
        assert_eq!(
            queue.total_bytes().await - 100,
            300,
            "handshake retention must be bounded by its own aggregate budget, not content's"
        );
        // Positive half: content survived the flood, so the bound above is
        // evidence of a real eviction on the handshake side, not an emptied
        // queue.
        assert!(queue.has_messages("carl").await, "content must survive the handshake flood");
        assert_eq!(queue.message_count("carl").await, 1);
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_enqueue_reports_refusal_and_displacement() {
        let limits = ByteLimits { user_content: 200, ..flat_limits(300) };
        let queue = OfflineQueue::with_byte_limits(1000, 1000, limits);

        // Success path: nothing refused, nothing displaced, nobody evicted.
        let ok = queue.enqueue("alice", make_sized("doc1", "x", 100)).await;
        assert_eq!(ok, EnqueueOutcome::default());

        // A message larger than the per-user cap is refused outright.
        let refused = queue.enqueue("alice", make_sized("doc1", "x", 201)).await;
        assert!(refused.refused, "a message over the per-user cap must be refused");
        assert_eq!(refused.displaced, 0);
        assert_eq!(queue.total_bytes().await, 100, "a refusal must mutate nothing");

        // Fill the budget (alice 200, bob 100), then a newcomer displaces.
        queue.enqueue("alice", make_sized("doc1", "x", 100)).await;
        queue.enqueue("bob", make_sized("doc1", "x", 100)).await;
        let displaced = queue.enqueue("carol", make_sized("doc1", "x", 100)).await;
        assert!(!displaced.refused);
        assert_eq!(displaced.displaced, 1, "one message dropped to make room");
        assert_eq!(queue.message_count("alice").await, 1, "the largest queue pays");
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_count_ring_trim_is_not_reported_as_byte_pressure() {
        // Byte budget huge (only the count ring can bite); max_per_user tiny.
        let queue = OfflineQueue::with_byte_limits(2, 1000, flat_limits(1_000_000));

        queue.enqueue("alice", make_sized("doc1", "x", 10)).await;
        queue.enqueue("alice", make_sized("doc1", "x", 10)).await;

        // Third message overflows the count ring only; nowhere near the byte cap.
        let outcome = queue.enqueue("alice", make_sized("doc1", "x", 10)).await;
        assert_eq!(outcome.trimmed, 1, "count-ring overflow must be reported as trimmed");
        assert_eq!(outcome.displaced, 0, "a count-ring drop must not be reported as byte pressure");

        // Positive half: genuine byte pressure elsewhere still reports displaced.
        let tight = OfflineQueue::with_byte_limits(1000, 1000, flat_limits(20));
        tight.enqueue("bob", make_sized("doc1", "x", 10)).await;
        tight.enqueue("bob", make_sized("doc1", "x", 10)).await;
        let byte_pressure = tight.enqueue("carol", make_sized("doc1", "x", 10)).await;
        assert!(byte_pressure.displaced > 0, "genuine byte pressure must still report displaced");
        assert_eq!(byte_pressure.trimmed, 0, "byte pressure must not be reported as a count trim");
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_per_user_content_cap_bounds_one_user() {
        // Room for 10 messages globally, but only 2 for any single user.
        let limits = ByteLimits { user_content: 250, ..flat_limits(10_000) };
        let queue = OfflineQueue::with_byte_limits(1000, 1000, limits);

        for _ in 0..20 {
            queue.enqueue("alice", make_sized("doc1", "x", 100)).await;
        }

        assert_eq!(queue.message_count("alice").await, 2, "per-user cap must bound one user");
        assert_eq!(queue.total_bytes().await, 200);
    }
}
