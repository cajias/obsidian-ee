//! Test helper utilities.

use std::time::Duration;

use collab_proto::{ClientMessage, ServerMessage};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// Default timeout for E2E operations.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Test server wrapper for E2E tests.
pub struct TestServer {
    pub url: String,
}

impl TestServer {
    /// Start a test server.
    #[allow(clippy::unused_async)] // Will have async implementation in T16
    pub async fn start() -> Self {
        // TODO: Implement in T16
        Self { url: "ws://localhost:8080".to_string() }
    }

    /// Get the server URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Test client for E2E testing.
pub struct TestClient {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// The user ID of this client.
    pub user_id: String,
}

impl TestClient {
    /// Connect to the relay server.
    ///
    /// # Errors
    ///
    /// Returns an error if the WebSocket connection fails.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let (ws, _) = connect_async(url).await?;
        Ok(Self { ws, user_id: String::new() })
    }

    /// Connect and identify as a user.
    ///
    /// # Errors
    ///
    /// Returns an error if connection or identification fails.
    pub async fn connect_as(url: &str, user_id: &str) -> anyhow::Result<Self> {
        let mut client = Self::connect(url).await?;
        client.user_id = user_id.to_string();
        let msg = ClientMessage::Identify { user_id: user_id.to_string() };
        client.send(&msg).await?;
        let response = client.recv().await?;
        if !matches!(response, ServerMessage::Identified { .. }) {
            anyhow::bail!("Expected Identified response");
        }
        Ok(client)
    }

    /// Send a message to the server.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or sending fails.
    pub async fn send(&mut self, msg: &ClientMessage) -> anyhow::Result<()> {
        let json = serde_json::to_string(msg)?;
        self.ws.send(Message::Text(json)).await?;
        Ok(())
    }

    /// Receive a message from the server with timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed, stream ends, or timeout expires.
    pub async fn recv(&mut self) -> anyhow::Result<ServerMessage> {
        let result = timeout(Duration::from_secs(10), self.ws.next()).await?;
        match result {
            Some(Ok(Message::Text(text))) => Ok(serde_json::from_str(&text)?),
            Some(Ok(Message::Close(_))) => anyhow::bail!("Connection closed"),
            Some(Err(e)) => Err(e.into()),
            None => anyhow::bail!("Stream ended"),
            _ => anyhow::bail!("Unexpected message type"),
        }
    }
}
