//! WebSocket relay server implementation.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use collab_proto::{ClientMessage, ErrorCode, ServerMessage};
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;

/// The relay server managing WebSocket connections.
pub struct RelayServer {
    /// Connected clients by user ID.
    clients: Arc<RwLock<HashMap<String, ClientHandle>>>,
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
        let handle = ServerHandle {
            shutdown_tx: shutdown_tx.clone(),
        };

        let server = Arc::new(self);

        // Spawn the accept loop
        tokio::spawn(async move {
            let mut shutdown_rx = shutdown_tx.subscribe();

            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, peer_addr)) => {
                                tracing::debug!("New connection from {}", peer_addr);
                                let server = Arc::clone(&server);
                                tokio::spawn(async move {
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
        });

        Ok(BoundServer {
            addr: local_addr,
            handle,
        })
    }

    /// Handle a single WebSocket connection.
    async fn handle_connection(&self, stream: TcpStream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ws_stream = tokio_tungstenite::accept_async(stream).await?;
        let (write, mut read) = ws_stream.split();

        // Channel for sending messages to this client
        let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

        // Spawn task to forward messages from channel to WebSocket
        let write = Arc::new(tokio::sync::Mutex::new(write));
        let write_clone = Arc::clone(&write);
        let writer_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Ok(json) = serde_json::to_string(&msg) {
                    let mut writer = write_clone.lock().await;
                    if writer.send(Message::Text(json)).await.is_err() {
                        break;
                    }
                }
            }
        });

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
                                    let _ = tx.send(error);
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

        // Clean up: remove client from connected clients
        if let Some(uid) = user_id {
            self.clients.write().await.remove(&uid);
            tracing::debug!("Client {} disconnected", uid);
        }

        writer_task.abort();
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
                tracing::debug!("User identified: {}", uid);

                // Store the client handle
                let handle = ClientHandle::new(uid.clone(), tx.clone());
                self.clients.write().await.insert(uid.clone(), handle);

                *user_id = Some(uid.clone());

                let response = ServerMessage::Identified { user_id: uid };
                let _ = tx.send(response);
            }
            ClientMessage::Subscribe { doc_id } => {
                if user_id.is_none() {
                    let _ = tx.send(ServerMessage::Error {
                        code: ErrorCode::NotIdentified,
                        message: "Must identify before subscribing".to_string(),
                    });
                    return;
                }
                // Subscription handling will be implemented in T11
                let _ = tx.send(ServerMessage::Subscribed { doc_id });
            }
            ClientMessage::Unsubscribe { doc_id } => {
                if user_id.is_none() {
                    let _ = tx.send(ServerMessage::Error {
                        code: ErrorCode::NotIdentified,
                        message: "Must identify before unsubscribing".to_string(),
                    });
                    return;
                }
                // Unsubscription handling will be implemented in T11
                let _ = tx.send(ServerMessage::Unsubscribed { doc_id });
            }
            ClientMessage::YrsUpdate { .. } | ClientMessage::MlsHandshake { .. } => {
                if user_id.is_none() {
                    let _ = tx.send(ServerMessage::Error {
                        code: ErrorCode::NotIdentified,
                        message: "Must identify before sending updates".to_string(),
                    });
                }
                // Message routing will be implemented in T11
            }
        }
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
            Self {
                addr: bound.addr,
                handle: bound.handle,
            }
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
                if let Some(Ok(Message::Text(text))) = self.read.next().await {
                    return serde_json::from_str(&text).unwrap();
                }
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

        client
            .send(ClientMessage::Identify {
                user_id: "alice".into(),
            })
            .await;

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
        client
            .send(ClientMessage::Subscribe {
                doc_id: "doc1".into(),
            })
            .await;

        let response = client.recv().await;
        assert!(matches!(
            response,
            ServerMessage::Error { code: ErrorCode::NotIdentified, .. }
        ));
    }

    #[tokio::test]
    async fn test_subscribe_after_identify() {
        let server = TestServer::start().await;
        let mut client = TestClient::connect(&server).await;

        // Identify first
        client
            .send(ClientMessage::Identify {
                user_id: "alice".into(),
            })
            .await;
        let _ = client.recv().await; // Identified response

        // Now subscribe
        client
            .send(ClientMessage::Subscribe {
                doc_id: "doc1".into(),
            })
            .await;

        let response = client.recv().await;
        assert!(matches!(
            response,
            ServerMessage::Subscribed { doc_id } if doc_id == "doc1"
        ));
    }

    #[tokio::test]
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
