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

use collab_proto::{ClientMessage, ErrorCode, MlsMessageType, ServerMessage};
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
            ClientMessage::Subscribe { doc_id } => {
                self.handle_subscribe(user_id.as_ref(), tx, doc_id).await;
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
        *session_close = Some(handle.close_signal());
        self.router.register_client(handle).await;
        *user_id = Some(uid.clone());

        send_msg(tx, ServerMessage::Identified { user_id: uid.clone() }).await;

        // Deliver anything queued while this user was offline.
        for queued in self.router.drain_offline(&uid).await {
            send_msg(tx, queued).await;
        }
    }

    /// Handle the Subscribe message.
    async fn handle_subscribe(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::Sender<ServerMessage>,
        doc_id: String,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "subscribing").await;
            return;
        };
        if !validate_doc_id(tx, &doc_id).await {
            return;
        }

        if self.router.subscribe(uid, &doc_id).await {
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

        client.send(ClientMessage::Subscribe { doc_id: "doc1".into() }).await;

        let response = client.recv().await;
        assert!(matches!(response, ServerMessage::Error { code: ErrorCode::NotIdentified, .. }));
    }

    #[tokio::test]
    async fn test_subscribe_after_identify() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        client.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        let _ = client.recv().await; // Identified response

        client.send(ClientMessage::Subscribe { doc_id: "doc1".into() }).await;

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
    async fn test_duplicate_identify_takes_over_and_closes_old() {
        let server = TestServer::start().await;

        let mut first = TestClient::connect(&server).await;
        first.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        assert!(matches!(first.recv().await, ServerMessage::Identified { .. }));

        // A second connection identifying as the same user takes over.
        let mut second = TestClient::connect(&server).await;
        second.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        assert!(matches!(second.recv().await, ServerMessage::Identified { .. }));

        // The first connection is told it was replaced.
        assert!(matches!(
            first.recv().await,
            ServerMessage::Error { code: ErrorCode::SessionReplaced, .. }
        ));
        // (Session-count invariants are covered by the router unit tests.)
    }

    #[tokio::test]
    async fn test_long_doc_id_is_rejected() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        client.send(ClientMessage::Identify { user_id: "alice".into(), token: None }).await;
        let _ = client.recv().await;

        let long_doc = "d".repeat(MAX_ID_LEN + 1);
        client.send(ClientMessage::Subscribe { doc_id: long_doc }).await;
        assert!(matches!(
            client.recv().await,
            ServerMessage::Error { code: ErrorCode::LimitExceeded, .. }
        ));
    }
}
