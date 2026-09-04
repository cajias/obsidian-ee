//! WebSocket relay server implementation.
//!
//! # Security posture
//!
//! The relay is a zero-knowledge router: it never sees plaintext. Even so, it
//! must protect itself and its clients from abuse. This module enforces:
//!
//! - **Authenticated identity (optional).** When configured with an auth token,
//!   an [`ClientMessage::Identify`] must carry a matching bearer token.
//! - **Connection-id-scoped sessions.** Each connection has a unique id, so a
//!   stale connection's teardown can never evict a newer session for the same
//!   user, and a duplicate `Identify` explicitly and deterministically takes
//!   over the prior session instead of silently corrupting routing state.
//! - **Resource bounds.** Bounded per-client channels (slow consumers are
//!   disconnected), a capped WebSocket frame size, a global connection cap, and
//!   per-document / document-count subscription caps (in [`MessageRouter`]).

use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use collab_proto::{ClientMessage, ErrorCode, MlsMessageType, ServerMessage, SubscribeCapability};
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Notify};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::routing::MessageRouter;

/// Capacity of each client's outbound message channel. A consumer that lets its
/// channel fill past this is treated as too slow and disconnected, bounding the
/// memory a single slow reader can pin.
const CHANNEL_CAPACITY: usize = 1024;

/// Maximum WebSocket message/frame size accepted from a client (1 MiB). This
/// caps the amplification of a single frame fanned out to N subscribers.
const MAX_MESSAGE_SIZE: usize = 1 << 20;

/// Maximum accepted length of a `doc_id` or `user_id` string.
const MAX_ID_LEN: usize = 256;

/// Default maximum number of concurrent connections.
const DEFAULT_MAX_CONNECTIONS: usize = 10_000;

/// The relay server managing WebSocket connections.
pub struct RelayServer {
    /// Message router: owns client sessions, subscriptions, and the offline queue.
    router: Arc<MessageRouter>,
    /// Shutdown signal sender.
    shutdown_tx: broadcast::Sender<()>,
    /// Optional bearer token required in `Identify`. `None` disables auth.
    auth_token: Option<String>,
    /// When true, receiving document content requires a current-epoch membership
    /// capability verified against the doc's registered anchor (issue #29).
    /// A capability-less `Subscribe` still succeeds, but for the MLS handshake
    /// only — that is what keeps the join bootstrap working under authz (issue
    /// #72). Default off pending the client-side work to mint and present
    /// capabilities on the normal subscribe path.
    require_subscribe_authz: bool,
    /// Maximum number of concurrent connections.
    max_connections: usize,
    /// Current number of active connections.
    active_connections: Arc<AtomicUsize>,
    /// Monotonic source of per-connection ids.
    conn_counter: Arc<AtomicU64>,
}

/// Handle to a connected client for sending messages.
#[derive(Clone)]
pub struct ClientHandle {
    /// User identifier.
    pub user_id: String,
    /// Unique id of the connection backing this handle.
    conn_id: u64,
    /// Bounded channel to send messages to this client.
    tx: mpsc::Sender<ServerMessage>,
    /// Signal used to force this connection to close (takeover / slow consumer).
    close: Arc<Notify>,
}

impl ClientHandle {
    /// Create a new client handle.
    #[must_use]
    pub fn new(user_id: String, conn_id: u64, tx: mpsc::Sender<ServerMessage>) -> Self {
        Self { user_id, conn_id, tx, close: Arc::new(Notify::new()) }
    }

    /// The unique id of the connection backing this handle.
    #[must_use]
    pub const fn conn_id(&self) -> u64 {
        self.conn_id
    }

    /// Try to send a message to this client without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error if the channel is closed or full (the consumer is too
    /// slow); callers treat either case as a delivery failure.
    pub fn send(&self, msg: ServerMessage) -> Result<(), mpsc::error::TrySendError<ServerMessage>> {
        self.tx.try_send(msg)
    }

    /// Signal the connection backing this handle to close.
    pub fn signal_close(&self) {
        self.close.notify_one();
    }

    /// Obtain a clone of the close signal, for the owning connection to await.
    #[must_use]
    pub fn close_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.close)
    }
}

/// Result of binding the server to an address.
pub struct BoundServer {
    /// The address the server is bound to.
    pub addr: SocketAddr,
    /// Handle to stop the server.
    pub handle: ServerHandle,
}

/// Handle to control the running server.
pub struct ServerHandle {
    shutdown_tx: broadcast::Sender<()>,
}

impl ServerHandle {
    /// Signal the server to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

impl RelayServer {
    /// Create a new relay server with default configuration and no auth token.
    #[must_use]
    pub fn new() -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            router: Arc::new(MessageRouter::new()),
            shutdown_tx,
            auth_token: None,
            require_subscribe_authz: false,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            active_connections: Arc::new(AtomicUsize::new(0)),
            conn_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Require clients to present a matching bearer token in `Identify`.
    ///
    /// Passing `None` (or never calling this) leaves authentication disabled.
    #[must_use]
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token.filter(|t| !t.is_empty());
        self
    }

    /// Enable per-document subscribe authorization (issues #29, #72).
    ///
    /// When enabled, a `Subscribe` carrying a [`SubscribeCapability`] is
    /// rejected with [`ErrorCode::Unauthorized`] unless the capability verifies
    /// against the doc's registered anchor (current epoch + verifying key) — no
    /// anchor, or a capability that fails to verify, means rejected. A
    /// `Subscribe` carrying NO capability succeeds but authorizes no content:
    /// the router then delivers handshake traffic to it and withholds
    /// `YrsUpdate`.
    #[must_use]
    pub fn with_subscribe_authz(mut self, enabled: bool) -> Self {
        self.require_subscribe_authz = enabled;
        // The router applies the content half of the gate on fan-out (#72).
        self.router.set_content_gating(enabled);
        self
    }

    /// Set the maximum number of concurrent connections.
    #[must_use]
    pub const fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    /// Bind and start the relay server on the given address.
    ///
    /// Returns a `BoundServer` with the actual address and a handle to stop the server.
    ///
    /// # Errors
    ///
    /// Returns an error if binding fails.
    pub async fn bind(self, addr: &str) -> Result<BoundServer, std::io::Error> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;

        let shutdown_tx = self.shutdown_tx.clone();
        let handle = ServerHandle { shutdown_tx: shutdown_tx.clone() };

        let server = Arc::new(self);

        tokio::spawn(run_accept_loop(server, listener, shutdown_tx));

        Ok(BoundServer { addr: local_addr, handle })
    }

    /// Handle a single WebSocket connection.
    async fn handle_connection(
        &self,
        stream: TcpStream,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let config = WebSocketConfig {
            max_message_size: Some(MAX_MESSAGE_SIZE),
            max_frame_size: Some(MAX_MESSAGE_SIZE),
            ..Default::default()
        };
        let ws_stream = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await?;
        let (write, mut read) = ws_stream.split();

        let conn_id = self.conn_counter.fetch_add(1, Ordering::Relaxed);

        // Bounded channel for sending messages to this client.
        let (tx, rx) = mpsc::channel::<ServerMessage>(CHANNEL_CAPACITY);

        let write = Arc::new(tokio::sync::Mutex::new(write));
        let writer_task = tokio::spawn(forward_messages_to_websocket(rx, Arc::clone(&write)));

        let mut user_id: Option<String> = None;
        let mut session_close: Option<Arc<Notify>> = None;
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            // Own (not borrow) the close signal so the read arm can mutate
            // `session_close` without a borrow conflict.
            let close_signal = session_close.clone();
            tokio::select! {
                msg = read.next() => {
                    let flow = self
                        .handle_ws_frame(msg, &tx, conn_id, &mut user_id, &mut session_close)
                        .await;
                    if flow.is_break() {
                        break;
                    }
                }
                _ = shutdown_rx.recv() => break,
                () = wait_for_close(close_signal) => {
                    tracing::debug!(conn_id, "Connection closed by takeover or resource limit");
                    break;
                }
            }
        }

        // Clean up: compare-and-remove only our own session. Subscriptions are
        // retained so this now-offline user's updates are queued for reconnect.
        if let Some(uid) = user_id {
            self.router.unregister_client(&uid, conn_id).await;
            tracing::debug!(conn_id, "Client {} disconnected", uid);
        }

        writer_task.abort();
        let _ = writer_task.await;
        Ok(())
    }

    /// Process one inbound WebSocket frame, returning whether to keep the
    /// connection open.
    async fn handle_ws_frame(
        &self,
        msg: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
        tx: &mpsc::Sender<ServerMessage>,
        conn_id: u64,
        user_id: &mut Option<String>,
        session_close: &mut Option<Arc<Notify>>,
    ) -> ControlFlow<()> {
        match msg {
            Some(Ok(Message::Text(text))) => {
                self.dispatch_text(&text, tx, conn_id, user_id, session_close).await;
                ControlFlow::Continue(())
            }
            Some(Ok(Message::Close(_))) | None => ControlFlow::Break(()),
            Some(Err(e)) => {
                tracing::error!("WebSocket error: {}", e);
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        }
    }

    /// Parse and dispatch a text frame, replying with an error on bad JSON.
    async fn dispatch_text(
        &self,
        text: &str,
        tx: &mpsc::Sender<ServerMessage>,
        conn_id: u64,
        user_id: &mut Option<String>,
        session_close: &mut Option<Arc<Notify>>,
    ) {
        match serde_json::from_str::<ClientMessage>(text) {
            Ok(client_msg) => {
                self.handle_message(client_msg, tx, conn_id, user_id, session_close).await;
            }
            Err(e) => {
                tracing::warn!("Invalid message: {}", e);
                send_msg(
                    tx,
                    ServerMessage::Error {
                        code: ErrorCode::InvalidMessage,
                        message: format!("Invalid message format: {e}"),
                    },
                )
                .await;
            }
        }
    }

    /// Handle a client message.
    async fn handle_message(
        &self,
        msg: ClientMessage,
        tx: &mpsc::Sender<ServerMessage>,
        conn_id: u64,
        user_id: &mut Option<String>,
        session_close: &mut Option<Arc<Notify>>,
    ) {
        match msg {
            ClientMessage::Identify { user_id: uid, token } => {
                self.handle_identify(uid, token, tx, conn_id, user_id, session_close).await;
            }
            ClientMessage::Subscribe { doc_id, capability } => {
                self.handle_subscribe(user_id.as_ref(), tx, doc_id, capability).await;
            }
            ClientMessage::RegisterDocKey { doc_id, epoch, public_key, proof, rotation_proof } => {
                self.handle_register_doc_key(
                    user_id.as_ref(),
                    tx,
                    doc_id,
                    epoch,
                    public_key,
                    proof,
                    rotation_proof,
                )
                .await;
            }
            ClientMessage::Unsubscribe { doc_id } => {
                self.handle_unsubscribe(user_id.as_ref(), tx, doc_id).await;
            }
            ClientMessage::YrsUpdate { doc_id, encrypted, epoch } => {
                self.handle_yrs_update(user_id.as_ref(), tx, doc_id, encrypted, epoch).await;
            }
            ClientMessage::MlsHandshake { doc_id, payload, message_type } => {
                self.handle_mls_handshake(user_id.as_ref(), tx, doc_id, payload, message_type)
                    .await;
            }
        }
    }

    /// Handle the Identify message: authenticate, register (taking over any
    /// prior session), then deliver any queued offline messages.
    async fn handle_identify(
        &self,
        uid: String,
        token: Option<String>,
        tx: &mpsc::Sender<ServerMessage>,
        conn_id: u64,
        user_id: &mut Option<String>,
        session_close: &mut Option<Arc<Notify>>,
    ) {
        let unauthorized =
            self.auth_token.as_deref().is_some_and(|expected| token.as_deref() != Some(expected));
        if unauthorized {
            tracing::warn!(user = %uid, "Rejected Identify: invalid or missing auth token");
            send_msg(
                tx,
                ServerMessage::Error {
                    code: ErrorCode::Unauthorized,
                    message: "Invalid or missing authentication token".to_string(),
                },
            )
            .await;
            return;
        }

        if uid.len() > MAX_ID_LEN {
            send_msg(
                tx,
                ServerMessage::Error {
                    code: ErrorCode::LimitExceeded,
                    message: format!("user_id exceeds maximum length of {MAX_ID_LEN}"),
                },
            )
            .await;
            return;
        }

        tracing::debug!(conn_id, "User identified: {}", uid);

        let handle = ClientHandle::new(uid.clone(), conn_id, tx.clone());
        let close_signal = handle.close_signal();
        // Permit self-takeover on reconnect when EITHER auth is disabled OR the
        // Identify is authenticated. In no-auth mode a duplicate Identify is just
        // the same user reconnecting (there is no identity to protect); rejecting
        // it would lock them out of their own user_id after an unclean disconnect
        // until TCP reaps the half-open socket. `unauthorized` is always false
        // here — the gate above returns early otherwise — so this stays true for
        // authenticated Identify and is defense-in-depth should that early return
        // ever be refactored: it goes false only for an unauthenticated duplicate
        // Identify while auth is enabled.
        let allow_takeover = !unauthorized;
        if !self.router.register_client(handle, allow_takeover).await {
            tracing::warn!(user = %uid, "Rejected Identify: user already has an active session");
            send_msg(
                tx,
                ServerMessage::Error {
                    code: ErrorCode::Unauthorized,
                    message: "user_id already has an active session".to_string(),
                },
            )
            .await;
            return;
        }
        *session_close = Some(close_signal);
        *user_id = Some(uid.clone());

        send_msg(tx, ServerMessage::Identified { user_id: uid.clone() }).await;

        // Deliver anything queued while this user was offline.
        for queued in self.router.drain_offline(&uid).await {
            send_msg(tx, queued).await;
        }
    }

    /// Handle the Subscribe message.
    ///
    /// When subscribe authorization is enabled (issue #29), a `capability` proves
    /// current-epoch membership: the doc must have a registered anchor and the
    /// capability must verify against it. The relay binds the LOCAL `doc_id` (the
    /// subscribe target) and the LOCALLY-stored `anchor.epoch` /
    /// `anchor.verifying_key` as the expected values — never `cap.epoch` or any
    /// other inbound frame field.
    ///
    /// Subscribing and receiving document content are separate (issue #72). A
    /// subscribe with NO capability succeeds for the MLS handshake only: it is
    /// the join bootstrap, where a joiner must be subscribed to receive the
    /// `Welcome` that makes it a member — only then can it mint a capability.
    /// Such a subscription carries no content authorization, so
    /// [`MessageRouter::route_message`] withholds `YrsUpdate` from it. A
    /// capability that is PRESENT but fails to verify still fails closed:
    /// `Unauthorized`, no subscription.
    async fn handle_subscribe(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::Sender<ServerMessage>,
        doc_id: String,
        capability: Option<SubscribeCapability>,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "subscribing").await;
            return;
        };
        if !validate_doc_id(tx, &doc_id).await {
            return;
        }

        let authorized_epoch = if self.require_subscribe_authz {
            match self.authorize_subscribe(tx, uid, &doc_id, capability).await {
                Ok(epoch) => epoch,
                Err(()) => return,
            }
        } else {
            // Authz off means content gating is off too, so this is never read.
            None
        };

        if self.router.subscribe(uid, &doc_id, authorized_epoch).await {
            send_msg(tx, ServerMessage::Subscribed { doc_id }).await;
        } else {
            send_msg(
                tx,
                ServerMessage::Error {
                    code: ErrorCode::LimitExceeded,
                    message: "Subscription limit reached".to_string(),
                },
            )
            .await;
        }
    }

    /// Verify a subscribe capability against the doc's anchor. Only called when
    /// `require_subscribe_authz` is on.
    ///
    /// Returns the subscription's content-authorization state: `Ok(Some(epoch))`
    /// when a capability verified against the anchor at that epoch, `Ok(None)`
    /// for a capability-less handshake-only subscribe (issue #72). Returns
    /// `Err(())` when the client is rejected, having already been sent
    /// `Unauthorized`.
    async fn authorize_subscribe(
        &self,
        tx: &mpsc::Sender<ServerMessage>,
        uid: &str,
        doc_id: &str,
        capability: Option<SubscribeCapability>,
    ) -> Result<Option<u64>, ()> {
        let Some(cap) = capability else {
            // No capability: subscribe for the MLS handshake only. A joiner has
            // no way to mint one before it has consumed its Welcome, and the
            // relay cannot tell that joiner from any other capability-less
            // client — so it authorizes no content and the router withholds
            // `YrsUpdate` (issue #72).
            return Ok(None);
        };
        // A capability WAS presented: from here it must check out, or the client
        // is rejected. No anchor → membership cannot be proven → fail closed.
        let Some(anchor) = self.router.get_anchor(doc_id).await else {
            send_unauthorized(tx, "no subscribe anchor registered for this document").await;
            return Err(());
        };
        // Bind the LOCALLY-trusted values as expected: the CONNECTION's identified
        // `uid`, the subscribe-target `doc_id`, and the LOCALLY-stored anchor
        // epoch/key — NEVER `cap.user_id`/`cap.epoch` or any inbound frame field
        // (CLAUDE.md). Passing the connection's uid means a capability minted for
        // Alice cannot be presented by Eve's connection (UserIdMismatch).
        if let Err(e) = collab_proto::verify_subscribe_capability(
            &cap,
            &anchor.verifying_key,
            uid,
            doc_id,
            anchor.epoch,
            now_unix(),
        ) {
            tracing::warn!(doc_id = %doc_id, error = %e, "Rejecting subscribe: capability failed");
            send_unauthorized(tx, "subscribe capability verification failed").await;
            return Err(());
        }
        // Bind the LOCALLY-stored anchor epoch as the authorized epoch, never
        // `cap.epoch`: a rotation past it then revokes this subscription.
        Ok(Some(anchor.epoch))
    }

    /// Handle the `RegisterDocKey` message: (re)anchor a doc's subscribe
    /// verification key (issue #29).
    ///
    /// The `proof` must be an `Ed25519` self-signature over `(doc_id || epoch)`
    /// verifiable under `public_key` — proving the registrant holds the private
    /// half of the key being registered. It does NOT prove group membership: the
    /// relay is a zero-knowledge router with no group state and no identity
    /// system, so it cannot. Anchor trust is TOFU (first registrant wins), the
    /// same model as first-Identify-wins for `user_id`.
    ///
    /// A **rotation** (an anchor already exists for `doc_id`) additionally requires
    /// `rotation_proof`: an `Ed25519` signature over `(doc_id || epoch ||
    /// public_key)` verifiable under the CURRENT stored anchor key. This ties the
    /// rotation to possession of the current anchor key, so an identified client
    /// cannot overwrite an existing anchor merely by picking an arbitrary key at a
    /// higher epoch (metadata escalation / subscribe-authz `DoS`). It RAISES the bar
    /// but is not full membership proof: a member holding `key_N` can still forge a
    /// rotation to N+1 until the group rekeys past their knowledge (PR #73 review).
    /// A FIRST (TOFU) registration ignores `rotation_proof`.
    ///
    /// Removal (#31) bites on the *subscribe* path, not here: after a rekey a
    /// removed member's stale-epoch capability no longer matches the rotated
    /// anchor. This never runs MLS: it stores a public key and does `Ed25519`
    /// verifies, then enforces monotonic rotation and the anchor resource bounds.
    #[allow(clippy::too_many_arguments)] // one field per RegisterDocKey wire field
    async fn handle_register_doc_key(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::Sender<ServerMessage>,
        doc_id: String,
        epoch: u64,
        public_key: Vec<u8>,
        proof: Vec<u8>,
        rotation_proof: Vec<u8>,
    ) {
        let Some(_uid) = user_id else {
            send_not_identified_error(tx, "registering a document key").await;
            return;
        };
        if !validate_doc_id(tx, &doc_id).await {
            return;
        }

        let Ok(key_bytes): Result<[u8; 32], _> = public_key.try_into() else {
            send_unauthorized(tx, "public_key must be 32 bytes").await;
            return;
        };

        // Verify the registrant holds the private half of the key being
        // registered. This proves key possession, NOT group membership (the relay
        // is zero-knowledge and cannot check membership); anchor trust is TOFU.
        if let Err(e) = collab_proto::verify_doc_key_proof(&doc_id, epoch, &key_bytes, &proof) {
            tracing::warn!(doc_id = %doc_id, error = %e, "Rejecting RegisterDocKey: bad proof");
            send_unauthorized(tx, "doc key proof verification failed").await;
            return;
        }

        // Rotation continuity: if an anchor already exists, the new registration
        // must be authorized by the CURRENT anchor key, not just a higher epoch.
        // Without this any identified client could overwrite an anchor with an
        // arbitrary key at a higher epoch (metadata escalation / subscribe DoS).
        let rotation_check = self.router.get_anchor(&doc_id).await.map(|current| {
            collab_proto::verify_anchor_rotation(
                &doc_id,
                epoch,
                &key_bytes,
                &current.verifying_key,
                &rotation_proof,
            )
        });
        if let Some(Err(e)) = rotation_check {
            tracing::warn!(doc_id = %doc_id, error = %e, "Rejecting RegisterDocKey: bad rotation continuity proof");
            send_unauthorized(tx, "anchor rotation continuity proof verification failed").await;
            return;
        }

        // Monotonic anchor: TOFU or strictly-higher epoch. A stale/equal epoch is
        // rejected so a captured old-epoch registration cannot roll the anchor back.
        if !self.router.set_anchor(&doc_id, epoch, key_bytes).await {
            tracing::warn!(doc_id = %doc_id, epoch, "Rejecting RegisterDocKey: stale epoch");
            send_unauthorized(tx, "stale or equal epoch for document anchor").await;
        }
        // On success: silent (no new ServerMessage variant needed).
    }

    /// Handle the Unsubscribe message.
    async fn handle_unsubscribe(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::Sender<ServerMessage>,
        doc_id: String,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "unsubscribing").await;
            return;
        };

        self.router.unsubscribe(uid, &doc_id).await;
        send_msg(tx, ServerMessage::Unsubscribed { doc_id }).await;
    }

    /// Handle `YrsUpdate` message - route to subscribers.
    async fn handle_yrs_update(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::Sender<ServerMessage>,
        doc_id: String,
        encrypted: Vec<u8>,
        epoch: u64,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "sending updates").await;
            return;
        };
        if !validate_doc_id(tx, &doc_id).await {
            return;
        }

        let message = ServerMessage::YrsUpdate {
            doc_id: doc_id.clone(),
            from: uid.clone(),
            encrypted,
            epoch,
        };

        self.router.route_message(&doc_id, uid, message).await;
    }

    /// Handle `MlsHandshake` message - route to subscribers.
    async fn handle_mls_handshake(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::Sender<ServerMessage>,
        doc_id: String,
        payload: Vec<u8>,
        message_type: MlsMessageType,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "sending MLS handshake").await;
            return;
        };
        if !validate_doc_id(tx, &doc_id).await {
            return;
        }

        let message = ServerMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            from: uid.clone(),
            payload,
            message_type,
        };

        self.router.route_message(&doc_id, uid, message).await;
    }
}

impl Default for RelayServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Await a connection-close signal, or never resolve if there is none yet.
async fn wait_for_close(signal: Option<Arc<Notify>>) {
    match signal {
        Some(notify) => notify.notified().await,
        None => std::future::pending::<()>().await,
    }
}

/// Validate a `doc_id`'s length, sending a `LimitExceeded` error if too long.
///
/// Returns `true` if the id is acceptable.
async fn validate_doc_id(tx: &mpsc::Sender<ServerMessage>, doc_id: &str) -> bool {
    if doc_id.len() > MAX_ID_LEN {
        send_msg(
            tx,
            ServerMessage::Error {
                code: ErrorCode::LimitExceeded,
                message: format!("doc_id exceeds maximum length of {MAX_ID_LEN}"),
            },
        )
        .await;
        return false;
    }
    true
}

/// Run the server accept loop, handling incoming connections.
async fn run_accept_loop(
    server: Arc<RelayServer>,
    listener: TcpListener,
    shutdown_tx: broadcast::Sender<()>,
) {
    let mut shutdown_rx = shutdown_tx.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        // Enforce the global connection cap.
                        let count = server.active_connections.fetch_add(1, Ordering::Relaxed) + 1;
                        if count > server.max_connections {
                            server.active_connections.fetch_sub(1, Ordering::Relaxed);
                            tracing::warn!(%peer_addr, "Connection cap reached; rejecting");
                            drop(stream);
                            continue;
                        }

                        tracing::debug!("New connection from {}", peer_addr);
                        let server = Arc::clone(&server);
                        tokio::spawn(async move {
                            let _guard = ConnectionGuard(Arc::clone(&server.active_connections));
                            if let Err(e) = server.handle_connection(stream).await {
                                tracing::error!("Connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Accept error: {}", e);
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutting down server");
                break;
            }
        }
    }
}

/// Decrements the active-connection counter when a connection task ends.
struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Current wall-clock time in whole seconds since the Unix epoch.
///
/// Used only for capability expiry checks. Fail CLOSED on a broken clock: a
/// pre-1970 `SystemTime` maps to `u64::MAX` ("far future"), which makes every
/// capability compare as expired and be rejected — never accepted. Mapping to 0
/// would be fail-OPEN (`now > expiry` could never fire → expired capabilities
/// accepted), a trust-boundary hole.
fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(u64::MAX, |d| d.as_secs())
}

/// Send an [`ErrorCode::Unauthorized`] error with a human-readable message.
async fn send_unauthorized(tx: &mpsc::Sender<ServerMessage>, message: &str) {
    send_msg(
        tx,
        ServerMessage::Error { code: ErrorCode::Unauthorized, message: message.to_string() },
    )
    .await;
}

/// Send a "not identified" error message.
async fn send_not_identified_error(tx: &mpsc::Sender<ServerMessage>, action: &str) {
    send_msg(
        tx,
        ServerMessage::Error {
            code: ErrorCode::NotIdentified,
            message: format!("Must identify before {action}"),
        },
    )
    .await;
}

/// Send a message to a client's channel, logging on failure.
async fn send_msg(tx: &mpsc::Sender<ServerMessage>, msg: ServerMessage) {
    if let Err(e) = tx.send(msg).await {
        tracing::warn!(error = %e, "Failed to enqueue message to client channel");
    }
}

/// Forward messages from a channel to a WebSocket writer.
async fn forward_messages_to_websocket<W>(
    mut rx: mpsc::Receiver<ServerMessage>,
    write: Arc<tokio::sync::Mutex<W>>,
) where
    W: SinkExt<Message> + Unpin,
    W::Error: std::fmt::Debug,
{
    while let Some(msg) = rx.recv().await {
        let json = match serde_json::to_string(&msg) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize ServerMessage to JSON");
                continue;
            }
        };
        let mut writer = write.lock().await;
        if let Err(e) = writer.send(Message::Text(json)).await {
            tracing::warn!(error = ?e, "Failed to send message over WebSocket");
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{SplitSink, SplitStream};
    use futures::SinkExt;
    use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

    /// Test helper for starting a server on a random port.
    struct TestServer {
        addr: SocketAddr,
        handle: ServerHandle,
    }

    impl TestServer {
        async fn start() -> Self {
            Self::start_with(RelayServer::new()).await
        }

        async fn start_with(server: RelayServer) -> Self {
            let bound = server.bind("127.0.0.1:0").await.unwrap();
            Self { addr: bound.addr, handle: bound.handle }
        }

        fn url(&self) -> String {
            format!("ws://{}", self.addr)
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.handle.shutdown();
        }
    }

    /// Test client helper.
    struct TestClient {
        write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
        read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    }

    #[allow(clippy::excessive_nesting)]
    impl TestClient {
        async fn connect(server: &TestServer) -> Self {
            let (ws, _) = connect_async(&server.url()).await.unwrap();
            let (write, read) = ws.split();
            Self { write, read }
        }

        async fn send(&mut self, msg: ClientMessage) {
            let json = serde_json::to_string(&msg).unwrap();
            self.write.send(Message::Text(json)).await.unwrap();
        }

        async fn recv(&mut self) -> ServerMessage {
            loop {
                let Some(Ok(Message::Text(text))) = self.read.next().await else {
                    continue;
                };
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn test_client_connects() {
        let server = TestServer::start().await;
        let (ws, _) = connect_async(&server.url()).await.unwrap();
        drop(ws);
    }

    #[tokio::test]
    async fn test_identify_user() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        client.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;

        let response = client.recv().await;
        assert!(matches!(
            response,
            ServerMessage::Identified { user_id } if user_id == "alice"
        ));
    }

    #[tokio::test]
    async fn test_subscribe_requires_identify() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        client.send(ClientMessage::Subscribe { doc_id: "doc1".into(), capability: None }).await;

        let response = client.recv().await;
        assert!(matches!(response, ServerMessage::Error { code: ErrorCode::NotIdentified, .. }));
    }

    #[tokio::test]
    async fn test_subscribe_after_identify() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        client.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        let _ = client.recv().await; // Identified response

        client.send(ClientMessage::Subscribe { doc_id: "doc1".into(), capability: None }).await;

        let response = client.recv().await;
        assert!(matches!(
            response,
            ServerMessage::Subscribed { doc_id } if doc_id == "doc1"
        ));
    }

    #[tokio::test]
    #[allow(clippy::excessive_nesting)]
    async fn test_invalid_message() {
        let server = TestServer::start().await;
        let (ws, _) = connect_async(&server.url()).await.unwrap();
        let (mut write, mut read) = ws.split();

        write.send(Message::Text("not json".into())).await.unwrap();

        if let Some(Ok(Message::Text(text))) = read.next().await {
            let response: ServerMessage = serde_json::from_str(&text).unwrap();
            assert!(matches!(
                response,
                ServerMessage::Error { code: ErrorCode::InvalidMessage, .. }
            ));
        }
    }

    #[tokio::test]
    async fn test_auth_token_rejects_missing_and_wrong_token() {
        let server =
            TestServer::start_with(RelayServer::new().with_auth_token(Some("s3cret".into()))).await;

        // Missing token.
        let mut client = TestClient::connect(&server).await;
        client.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        assert!(matches!(
            client.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));

        // Wrong token.
        let mut client2 = TestClient::connect(&server).await;
        client2
            .send(ClientMessage::Identify { user_id: "alice".into(), token: Some("nope".into()) })
            .await;
        assert!(matches!(
            client2.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));
    }

    #[tokio::test]
    async fn test_auth_token_accepts_correct_token() {
        let server =
            TestServer::start_with(RelayServer::new().with_auth_token(Some("s3cret".into()))).await;

        let mut client = TestClient::connect(&server).await;
        client
            .send(ClientMessage::Identify { user_id: "alice".into(), token: Some("s3cret".into()) })
            .await;
        assert!(matches!(client.recv().await, ServerMessage::Identified { .. }));
    }

    #[tokio::test]
    async fn test_authenticated_duplicate_identify_takes_over_and_closes_old() {
        // Takeover is only permitted for authenticated Identify.
        let server =
            TestServer::start_with(RelayServer::new().with_auth_token(Some("s3cret".into()))).await;

        let mut first = TestClient::connect(&server).await;
        first
            .send(ClientMessage::Identify { user_id: "alice".into(), token: Some("s3cret".into()) })
            .await;
        assert!(matches!(first.recv().await, ServerMessage::Identified { .. }));

        // A second authenticated connection identifying as the same user takes over.
        let mut second = TestClient::connect(&server).await;
        second
            .send(ClientMessage::Identify { user_id: "alice".into(), token: Some("s3cret".into()) })
            .await;
        assert!(matches!(second.recv().await, ServerMessage::Identified { .. }));

        // The first connection is told it was replaced.
        assert!(matches!(
            first.recv().await,
            ServerMessage::Error { code: ErrorCode::SessionReplaced, .. }
        ));
        // (Session-count invariants are covered by the router unit tests.)
    }

    #[tokio::test]
    async fn test_no_auth_duplicate_identify_takes_over() {
        // With auth disabled there is no identity to protect, so a duplicate
        // Identify is treated as the same user reconnecting: it takes over the
        // prior session (which is signalled it was replaced). This is what makes
        // reconnect work after an unclean disconnect leaves a stale half-open
        // session lingering in `clients` until TCP reaps it.
        let server = TestServer::start().await;

        let mut first = TestClient::connect(&server).await;
        first.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        assert!(matches!(first.recv().await, ServerMessage::Identified { .. }));

        let mut second = TestClient::connect(&server).await;
        second.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        // The reconnecting session is accepted.
        assert!(matches!(second.recv().await, ServerMessage::Identified { .. }));

        // The stale session is told it was replaced.
        assert!(matches!(
            first.recv().await,
            ServerMessage::Error { code: ErrorCode::SessionReplaced, .. }
        ));
    }

    #[tokio::test]
    async fn test_long_doc_id_is_rejected() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        client.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        let _ = client.recv().await;

        let long_doc = "d".repeat(MAX_ID_LEN + 1);
        client.send(ClientMessage::Subscribe { doc_id: long_doc, capability: None }).await;
        assert!(matches!(
            client.recv().await,
            ServerMessage::Error { code: ErrorCode::LimitExceeded, .. }
        ));
    }

    // ---- Subscribe authorization (issue #29) --------------------------------

    use collab_proto::{
        sign_anchor_rotation, sign_doc_key_proof, sign_subscribe_capability, SubscribeCapability,
    };
    use ed25519_dalek::SigningKey;

    const AUTHZ_DOC: &str = "authz-doc";
    const AUTHZ_EPOCH: u64 = 1;

    /// A member's signing key (deterministic seed) standing in for the group's
    /// per-epoch exporter-derived key.
    fn member_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    /// Identify `client` and register `signer`'s verifying key as `AUTHZ_DOC`'s
    /// anchor at `AUTHZ_EPOCH` with a valid self-proof. First (TOFU) registration,
    /// so `rotation_proof` is empty.
    async fn register_anchor(client: &mut TestClient, signer: &SigningKey) {
        let public_key = signer.verifying_key().to_bytes().to_vec();
        let proof = sign_doc_key_proof(signer, AUTHZ_DOC, AUTHZ_EPOCH);
        client
            .send(ClientMessage::RegisterDocKey {
                doc_id: AUTHZ_DOC.into(),
                epoch: AUTHZ_EPOCH,
                public_key,
                proof,
                rotation_proof: Vec::new(),
            })
            .await;
    }

    /// A capability naming `user_id` for `AUTHZ_DOC` at `AUTHZ_EPOCH` signed by
    /// `signer`, expiring far in the future.
    fn cap_for(signer: &SigningKey, user_id: &str) -> SubscribeCapability {
        sign_subscribe_capability(signer, user_id, AUTHZ_DOC, AUTHZ_EPOCH, u64::MAX)
    }

    /// A member (holding the anchor key) can register then subscribe with a
    /// valid capability.
    #[tokio::test]
    async fn test_authz_member_with_valid_capability_subscribes() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut member, &signer).await;

        member
            .send(ClientMessage::Subscribe {
                doc_id: AUTHZ_DOC.into(),
                capability: Some(cap_for(&signer, "member")),
            })
            .await;
        assert!(matches!(
            member.recv().await,
            ServerMessage::Subscribed { doc_id } if doc_id == AUTHZ_DOC
        ));
    }

    /// A capability minted naming one member, presented by a DIFFERENT
    /// connection's identity, is rejected `UserIdMismatch` — even though it is
    /// signed by the anchor key (a same-group replay-as-someone-else). The relay
    /// binds the presenting connection's LOCAL uid as `expected_user_id`.
    #[tokio::test]
    async fn test_authz_rejects_capability_replayed_as_other_user() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        // Bob (the minter) registers the anchor.
        let mut bob = TestClient::connect(&server).await;
        bob.send(ClientMessage::Identify { user_id: "bob".into(), token: None }).await;
        assert!(matches!(bob.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut bob, &signer).await;

        // Alice presents a capability minted for BOB (same valid anchor key), but
        // her connection is identified as "alice" → UserIdMismatch, rejected.
        let mut alice = TestClient::connect(&server).await;
        alice.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        assert!(matches!(alice.recv().await, ServerMessage::Identified { .. }));
        alice
            .send(ClientMessage::Subscribe {
                doc_id: AUTHZ_DOC.into(),
                capability: Some(cap_for(&signer, "bob")),
            })
            .await;
        assert!(matches!(
            alice.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));
    }

    /// With authz on, a subscribe carrying NO capability SUCCEEDS as
    /// handshake-only (issue #72). This is the join bootstrap: a joiner must be
    /// subscribed to receive the `Welcome` that makes it a member, and only then
    /// can it mint a capability. It is not content-authorized by this subscribe —
    /// the router withholds `YrsUpdate` from it (see the routing.rs content-gating
    /// tests and the `subscribe_authz` wire test).
    #[tokio::test]
    async fn test_authz_missing_capability_subscribes_handshake_only() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        // Register the anchor (via a member) first: the doc IS anchored and the
        // client still presents nothing — the strict handshake-only case, not a
        // subscribe that slipped through because the doc was unanchored.
        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut member, &signer).await;

        let mut joiner = TestClient::connect(&server).await;
        joiner.send(ClientMessage::Identify { user_id: "joiner".into(), token: None }).await;
        assert!(matches!(joiner.recv().await, ServerMessage::Identified { .. }));
        joiner.send(ClientMessage::Subscribe { doc_id: AUTHZ_DOC.into(), capability: None }).await;
        assert!(matches!(
            joiner.recv().await,
            ServerMessage::Subscribed { doc_id } if doc_id == AUTHZ_DOC
        ));
    }

    /// With authz on, a subscribe carrying a capability that FAILS verification
    /// is still rejected — the #72 bootstrap allowance is for an ABSENT
    /// capability only. An explicitly bad one is an attack signal, and rejecting
    /// it cannot reintroduce the deadlock: a joiner presents none at all.
    #[tokio::test]
    async fn test_authz_still_rejects_invalid_capability() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut member, &signer).await;

        // A capability at the WRONG epoch, signed by the real anchor key.
        let stale =
            sign_subscribe_capability(&signer, "member", AUTHZ_DOC, AUTHZ_EPOCH + 1, u64::MAX);
        member
            .send(ClientMessage::Subscribe { doc_id: AUTHZ_DOC.into(), capability: Some(stale) })
            .await;
        assert!(matches!(
            member.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));
    }

    /// A capability signed by a NON-anchor key (a non-member) is rejected.
    #[tokio::test]
    async fn test_authz_rejects_non_member_capability() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut member, &signer).await;

        // Eve mints a capability with her OWN (non-anchor) key.
        let eve_key = SigningKey::from_bytes(&[9u8; 32]);
        let mut eve = TestClient::connect(&server).await;
        eve.send(ClientMessage::Identify { user_id: "eve".into(), token: None }).await;
        assert!(matches!(eve.recv().await, ServerMessage::Identified { .. }));
        eve.send(ClientMessage::Subscribe {
            doc_id: AUTHZ_DOC.into(),
            capability: Some(cap_for(&eve_key, "eve")),
        })
        .await;
        assert!(matches!(
            eve.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));
    }

    /// A capability minted for a DIFFERENT doc is rejected on this doc (the
    /// capability's own doc_id does not match the subscribe target).
    #[tokio::test]
    async fn test_authz_rejects_capability_for_other_doc() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut member, &signer).await;

        // Capability for "other-doc" signed by the anchor key — wrong target.
        let other_cap =
            sign_subscribe_capability(&signer, "member", "other-doc", AUTHZ_EPOCH, u64::MAX);
        member
            .send(ClientMessage::Subscribe {
                doc_id: AUTHZ_DOC.into(),
                capability: Some(other_cap),
            })
            .await;
        assert!(matches!(
            member.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));
    }

    /// With authz on but NO anchor registered, even a valid-looking capability is
    /// rejected: membership cannot be proven, so subscribe fails closed.
    #[tokio::test]
    async fn test_authz_rejects_when_no_anchor() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        let mut client = TestClient::connect(&server).await;
        client.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(client.recv().await, ServerMessage::Identified { .. }));
        client
            .send(ClientMessage::Subscribe {
                doc_id: AUTHZ_DOC.into(),
                capability: Some(cap_for(&signer, "member")),
            })
            .await;
        assert!(matches!(
            client.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));
    }

    /// A `RegisterDocKey` whose proof does not verify under `public_key` is
    /// rejected, so no anchor is set.
    #[tokio::test]
    async fn test_register_doc_key_rejects_bad_proof() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let signer = member_key();

        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));

        // Proof signed by a DIFFERENT key than public_key → verification fails.
        let wrong_signer = SigningKey::from_bytes(&[3u8; 32]);
        member
            .send(ClientMessage::RegisterDocKey {
                doc_id: AUTHZ_DOC.into(),
                epoch: AUTHZ_EPOCH,
                public_key: signer.verifying_key().to_bytes().to_vec(),
                proof: sign_doc_key_proof(&wrong_signer, AUTHZ_DOC, AUTHZ_EPOCH),
                rotation_proof: Vec::new(),
            })
            .await;
        assert!(matches!(
            member.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));

        // And the anchor was never set: a later valid capability still fails
        // closed (no anchor).
        member
            .send(ClientMessage::Subscribe {
                doc_id: AUTHZ_DOC.into(),
                capability: Some(cap_for(&signer, "member")),
            })
            .await;
        assert!(matches!(
            member.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));
    }

    /// Authz OFF (the default) preserves the legacy un-gated subscribe so the
    /// MLS-handshake bootstrap keeps working.
    #[tokio::test]
    async fn test_authz_off_allows_ungated_subscribe() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;
        client.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        assert!(matches!(client.recv().await, ServerMessage::Identified { .. }));
        client.send(ClientMessage::Subscribe { doc_id: "doc1".into(), capability: None }).await;
        assert!(matches!(
            client.recv().await,
            ServerMessage::Subscribed { doc_id } if doc_id == "doc1"
        ));
    }

    /// Rotation continuity NEGATIVE (PR #73 review): once an anchor exists, an
    /// attacker who does NOT hold the current anchor key cannot overwrite it —
    /// even with a valid self-proof under a NEW key at a strictly-higher epoch —
    /// because the continuity proof does not verify under the stored anchor key.
    /// The anchor is left unchanged.
    #[tokio::test]
    async fn test_register_doc_key_rejects_rotation_without_current_key() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let member_signer = member_key();

        // A member registers the first anchor (TOFU) at epoch 1.
        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut member, &member_signer).await;

        // An attacker holding their OWN key tries to rotate to epoch 2. The
        // self-proof is valid (under the attacker's new key), and the epoch is
        // strictly higher, but the continuity proof is signed by the ATTACKER's
        // key, not the current anchor key → rejected.
        let attacker_signer = SigningKey::from_bytes(&[11u8; 32]);
        let attacker_key = attacker_signer.verifying_key().to_bytes();
        let mut attacker = TestClient::connect(&server).await;
        attacker.send(ClientMessage::Identify { user_id: "attacker".into(), token: None }).await;
        assert!(matches!(attacker.recv().await, ServerMessage::Identified { .. }));
        attacker
            .send(ClientMessage::RegisterDocKey {
                doc_id: AUTHZ_DOC.into(),
                epoch: AUTHZ_EPOCH + 1,
                public_key: attacker_key.to_vec(),
                proof: sign_doc_key_proof(&attacker_signer, AUTHZ_DOC, AUTHZ_EPOCH + 1),
                // Continuity proof signed by the attacker's own key, not the
                // current anchor key: the realistic forgery attempt.
                rotation_proof: sign_anchor_rotation(
                    &attacker_signer,
                    AUTHZ_DOC,
                    AUTHZ_EPOCH + 1,
                    &attacker_key,
                ),
            })
            .await;
        assert!(matches!(
            attacker.recv().await,
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ));

        // The anchor is unchanged: still the ORIGINAL member key at epoch 1, so a
        // capability under the original key at epoch 1 still subscribes.
        member
            .send(ClientMessage::Subscribe {
                doc_id: AUTHZ_DOC.into(),
                capability: Some(cap_for(&member_signer, "member")),
            })
            .await;
        assert!(matches!(member.recv().await, ServerMessage::Subscribed { .. }));
    }

    /// Rotation continuity POSITIVE (PR #73 review): a rotation authorized by the
    /// CURRENT anchor key succeeds. The new anchor (new key + higher epoch) then
    /// governs subscribe.
    #[tokio::test]
    async fn test_register_doc_key_accepts_rotation_signed_by_current_key() {
        let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
        let member_signer = member_key();

        let mut member = TestClient::connect(&server).await;
        member.send(ClientMessage::Identify { user_id: "member".into(), token: None }).await;
        assert!(matches!(member.recv().await, ServerMessage::Identified { .. }));
        register_anchor(&mut member, &member_signer).await;

        // Rekey: the member now holds the epoch-2 exporter key and rotates to it,
        // authorizing the rotation with a continuity proof under the CURRENT
        // (epoch-1) anchor key. Silent on success.
        let next_signer = SigningKey::from_bytes(&[12u8; 32]);
        let next_key = next_signer.verifying_key().to_bytes();
        member
            .send(ClientMessage::RegisterDocKey {
                doc_id: AUTHZ_DOC.into(),
                epoch: AUTHZ_EPOCH + 1,
                public_key: next_key.to_vec(),
                proof: sign_doc_key_proof(&next_signer, AUTHZ_DOC, AUTHZ_EPOCH + 1),
                rotation_proof: sign_anchor_rotation(
                    &member_signer,
                    AUTHZ_DOC,
                    AUTHZ_EPOCH + 1,
                    &next_key,
                ),
            })
            .await;

        // The anchor now governs epoch 2 under the new key: a capability under the
        // new key at epoch 2 subscribes.
        let new_epoch_cap =
            sign_subscribe_capability(&next_signer, "member", AUTHZ_DOC, AUTHZ_EPOCH + 1, u64::MAX);
        member
            .send(ClientMessage::Subscribe {
                doc_id: AUTHZ_DOC.into(),
                capability: Some(new_epoch_cap),
            })
            .await;
        assert!(matches!(member.recv().await, ServerMessage::Subscribed { .. }));
    }
}
