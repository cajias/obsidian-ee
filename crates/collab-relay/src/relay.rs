//! WebSocket relay server implementation.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use collab_proto::{ClientMessage, ErrorCode, MlsMessageType, ServerMessage};
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;

use crate::routing::MessageRouter;

/// The relay server managing WebSocket connections.
pub struct RelayServer {
    /// Connected clients by user ID.
    clients: Arc<RwLock<HashMap<String, ClientHandle>>>,
    /// Message router for subscriptions.
    router: Arc<MessageRouter>,
    /// Shutdown signal sender.
    shutdown_tx: broadcast::Sender<()>,
}

/// Handle to a connected client for sending messages.
#[derive(Clone)]
pub struct ClientHandle {
    /// User identifier.
    pub user_id: String,
    /// Channel to send messages to this client.
    tx: mpsc::UnboundedSender<ServerMessage>,
}

impl ClientHandle {
    /// Create a new client handle.
    #[must_use]
    pub const fn new(user_id: String, tx: mpsc::UnboundedSender<ServerMessage>) -> Self {
        Self { user_id, tx }
    }

    /// Send a message to this client.
    ///
    /// # Errors
    ///
    /// Returns an error if the channel is closed.
    pub fn send(&self, msg: ServerMessage) -> Result<(), mpsc::error::SendError<ServerMessage>> {
        self.tx.send(msg)
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
    /// Create a new relay server.
    #[must_use]
    pub fn new() -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            router: Arc::new(MessageRouter::new()),
            shutdown_tx,
        }
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

        // Spawn the accept loop
        tokio::spawn(run_accept_loop(server, listener, shutdown_tx));

        Ok(BoundServer { addr: local_addr, handle })
    }

    /// Handle a single WebSocket connection.
    #[allow(clippy::too_many_lines)]
    async fn handle_connection(
        &self,
        stream: TcpStream,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ws_stream = tokio_tungstenite::accept_async(stream).await?;
        let (write, mut read) = ws_stream.split();

        // Channel for sending messages to this client
        let (tx, rx) = mpsc::unbounded_channel::<ServerMessage>();

        // Spawn task to forward messages from channel to WebSocket
        let write = Arc::new(tokio::sync::Mutex::new(write));
        let write_clone = Arc::clone(&write);
        let writer_task = tokio::spawn(forward_messages_to_websocket(rx, write_clone));

        let mut user_id: Option<String> = None;
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<ClientMessage>(&text) {
                                Ok(client_msg) => {
                                    self.handle_message(client_msg, &tx, &mut user_id).await;
                                }
                                Err(e) => {
                                    tracing::warn!("Invalid message: {}", e);
                                    let error = ServerMessage::Error {
                                        code: ErrorCode::InvalidMessage,
                                        message: format!("Invalid message format: {e}"),
                                    };
                                    if let Err(e) = tx.send(error) {
                                        tracing::warn!(error = %e, "Failed to send error response to client");
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("WebSocket error: {}", e);
                            break;
                        }
                        _ => {}
                    }
                }
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }

        // Clean up: remove client from connected clients and router
        if let Some(uid) = user_id {
            self.clients.write().await.remove(&uid);
            self.router.unregister_client(&uid).await;
            tracing::debug!("Client {} disconnected", uid);
        }

        writer_task.abort();
        let _ = writer_task.await;
        Ok(())
    }

    /// Handle a client message.
    async fn handle_message(
        &self,
        msg: ClientMessage,
        tx: &mpsc::UnboundedSender<ServerMessage>,
        user_id: &mut Option<String>,
    ) {
        match msg {
            ClientMessage::Identify { user_id: uid } => {
                self.handle_identify(uid, tx, user_id).await;
            }
            ClientMessage::Subscribe { doc_id } => {
                self.handle_subscribe(user_id.as_ref(), tx, doc_id).await;
            }
            ClientMessage::Unsubscribe { doc_id } => {
                self.handle_unsubscribe(user_id.as_ref(), tx, doc_id).await;
            }
            ClientMessage::YrsUpdate { doc_id, encrypted, epoch, signature } => {
                self.handle_yrs_update(user_id.as_ref(), tx, doc_id, encrypted, epoch, signature)
                    .await;
            }
            ClientMessage::MlsHandshake { doc_id, payload, message_type } => {
                self.handle_mls_handshake(user_id.as_ref(), tx, doc_id, payload, message_type)
                    .await;
            }
        }
    }

    /// Handle the Identify message.
    async fn handle_identify(
        &self,
        uid: String,
        tx: &mpsc::UnboundedSender<ServerMessage>,
        user_id: &mut Option<String>,
    ) {
        tracing::debug!("User identified: {}", uid);

        // Store the client handle
        let handle = ClientHandle::new(uid.clone(), tx.clone());
        self.clients.write().await.insert(uid.clone(), handle.clone());

        // Register with router for message routing
        self.router.register_client(handle).await;

        *user_id = Some(uid.clone());

        let response = ServerMessage::Identified { user_id: uid };
        if let Err(e) = tx.send(response) {
            tracing::warn!(error = %e, "Failed to send Identified response to client");
        }
    }

    /// Handle the Subscribe message.
    async fn handle_subscribe(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::UnboundedSender<ServerMessage>,
        doc_id: String,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "subscribing");
            return;
        };

        self.router.subscribe(uid, &doc_id).await;
        if let Err(e) = tx.send(ServerMessage::Subscribed { doc_id }) {
            tracing::warn!(error = %e, "Failed to send Subscribed response to client");
        }
    }

    /// Handle the Unsubscribe message.
    async fn handle_unsubscribe(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::UnboundedSender<ServerMessage>,
        doc_id: String,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "unsubscribing");
            return;
        };

        self.router.unsubscribe(uid, &doc_id).await;
        if let Err(e) = tx.send(ServerMessage::Unsubscribed { doc_id }) {
            tracing::warn!(error = %e, "Failed to send Unsubscribed response to client");
        }
    }

    /// Handle `YrsUpdate` message - route to subscribers.
    async fn handle_yrs_update(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::UnboundedSender<ServerMessage>,
        doc_id: String,
        encrypted: Vec<u8>,
        epoch: u64,
        signature: Vec<u8>,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "sending updates");
            return;
        };

        let message = ServerMessage::YrsUpdate {
            doc_id: doc_id.clone(),
            from: uid.clone(),
            encrypted,
            epoch,
            signature,
        };

        self.router.route_message(&doc_id, uid, message).await;
    }

    /// Handle `MlsHandshake` message - route to subscribers.
    async fn handle_mls_handshake(
        &self,
        user_id: Option<&String>,
        tx: &mpsc::UnboundedSender<ServerMessage>,
        doc_id: String,
        payload: Vec<u8>,
        message_type: MlsMessageType,
    ) {
        let Some(uid) = user_id else {
            send_not_identified_error(tx, "sending MLS handshake");
            return;
        };

        let message = ServerMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            from: uid.clone(),
            payload,
            message_type,
        };

        self.router.route_message(&doc_id, uid, message).await;
    }

    /// Get a client handle by user ID (for testing).
    #[cfg(test)]
    pub async fn get_client(&self, user_id: &str) -> Option<ClientHandle> {
        self.clients.read().await.get(user_id).cloned()
    }

    /// Get the number of connected clients (for testing).
    #[cfg(test)]
    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
    }
}

impl Default for RelayServer {
    fn default() -> Self {
        Self::new()
    }
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
                handle_accept_result(result, &server);
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutting down server");
                break;
            }
        }
    }
}

/// Handle the result of accepting a new connection.
fn handle_accept_result(
    result: std::io::Result<(TcpStream, SocketAddr)>,
    server: &Arc<RelayServer>,
) {
    let Ok((stream, peer_addr)) = result else {
        if let Err(e) = &result {
            tracing::error!("Accept error: {}", e);
        }
        return;
    };
    tracing::debug!("New connection from {}", peer_addr);
    let server = Arc::clone(server);
    tokio::spawn(spawn_connection_handler(server, stream));
}

/// Spawn a connection handler task.
async fn spawn_connection_handler(server: Arc<RelayServer>, stream: TcpStream) {
    if let Err(e) = server.handle_connection(stream).await {
        tracing::error!("Connection error: {}", e);
    }
}

/// Send a "not identified" error message.
fn send_not_identified_error(tx: &mpsc::UnboundedSender<ServerMessage>, action: &str) {
    if let Err(e) = tx.send(ServerMessage::Error {
        code: ErrorCode::NotIdentified,
        message: format!("Must identify before {action}"),
    }) {
        tracing::warn!(error = %e, action = %action, "Failed to send NotIdentified error to client");
    }
}

/// Forward messages from a channel to a WebSocket writer.
async fn forward_messages_to_websocket<W>(
    mut rx: mpsc::UnboundedReceiver<ServerMessage>,
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
            let server = RelayServer::new();
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
        // Connection succeeded
        drop(ws);
    }

    #[tokio::test]
    async fn test_identify_user() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        client.send(ClientMessage::Identify { user_id: "alice".into() }).await;

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

        // Try to subscribe without identifying first
        client.send(ClientMessage::Subscribe { doc_id: "doc1".into() }).await;

        let response = client.recv().await;
        assert!(matches!(response, ServerMessage::Error { code: ErrorCode::NotIdentified, .. }));
    }

    #[tokio::test]
    async fn test_subscribe_after_identify() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        // Identify first
        client.send(ClientMessage::Identify { user_id: "alice".into() }).await;
        let _ = client.recv().await; // Identified response

        // Now subscribe
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

        // Send invalid JSON
        write.send(Message::Text("not json".into())).await.unwrap();

        // Should receive error
        if let Some(Ok(Message::Text(text))) = read.next().await {
            let response: ServerMessage = serde_json::from_str(&text).unwrap();
            assert!(matches!(
                response,
                ServerMessage::Error { code: ErrorCode::InvalidMessage, .. }
            ));
        }
    }
}
