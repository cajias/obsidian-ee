//! Test helper utilities for E2E testing.
//!
//! This module provides:
//! - `TestClient` for WebSocket communication with the relay
//! - `MlsTestGroup` for quick MLS group setup between users
//! - Timeout-aware operations with clear error messages

use std::time::Duration;

use collab_core::{EncryptedDocument, EncryptedOp, Invite, MlsDocumentGroup, PendingMember};
use collab_proto::{ClientMessage, DocumentId, MlsMessageType, ServerMessage};
use collab_relay::{RelayServer, ServerHandle};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// Default timeout for E2E operations.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Short timeout for operations that should be quick.
pub const SHORT_TIMEOUT: Duration = Duration::from_secs(5);

/// Relay server URL for tests.
pub const RELAY_URL: &str = "ws://localhost:8080/ws";

/// Test server wrapper for E2E tests.
pub struct TestServer {
    pub url: String,
    handle: ServerHandle,
}

impl TestServer {
    /// Start a test server on a random port.
    ///
    /// # Panics
    ///
    /// Panics if the server fails to bind to a free port.
    pub async fn start() -> Self {
        let server = RelayServer::new();
        let bound = server.bind("127.0.0.1:0").await.expect("Failed to bind test server");
        let url = format!("ws://{}", bound.addr);
        Self { url, handle: bound.handle }
    }

    /// Get the server URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Get the WebSocket URL (convenience clone).
    #[must_use]
    pub fn ws_url(&self) -> String {
        self.url.clone()
    }

    /// Shut down the server.
    pub fn shutdown(&self) {
        self.handle.shutdown();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.shutdown();
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

    /// Receive a message from the server with default timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed, stream ends, or timeout expires.
    pub async fn recv(&mut self) -> anyhow::Result<ServerMessage> {
        self.recv_timeout(SHORT_TIMEOUT).await
    }

    /// Receive a message from the server with custom timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed, stream ends, or timeout expires.
    pub async fn recv_timeout(&mut self, duration: Duration) -> anyhow::Result<ServerMessage> {
        let result = timeout(duration, self.ws.next())
            .await
            .map_err(|_| anyhow::anyhow!("Timeout after {duration:?} waiting for message"))?;
        match result {
            Some(Ok(Message::Text(text))) => Ok(serde_json::from_str(&text)?),
            Some(Ok(Message::Close(_))) => anyhow::bail!("Connection closed unexpectedly"),
            Some(Err(e)) => Err(e.into()),
            None => anyhow::bail!("WebSocket stream ended unexpectedly"),
            _ => anyhow::bail!("Unexpected WebSocket message type"),
        }
    }

    /// Try to receive a message, returning `Ok(None)` only on timeout.
    ///
    /// Unlike [`recv_timeout`](Self::recv_timeout), this method distinguishes
    /// timeouts from real errors structurally (via `tokio::time::timeout`)
    /// rather than by string matching.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails or message is invalid.
    /// Only returns `Ok(None)` on a legitimate `tokio::time::Elapsed` timeout.
    pub async fn try_recv(&mut self, duration: Duration) -> anyhow::Result<Option<ServerMessage>> {
        match timeout(duration, self.ws.next()).await {
            Err(_elapsed) => Ok(None),
            Ok(Some(Ok(Message::Text(text)))) => Ok(Some(serde_json::from_str(&text)?)),
            Ok(Some(Ok(Message::Close(_)))) => anyhow::bail!("Connection closed unexpectedly"),
            Ok(Some(Err(e))) => Err(e.into()),
            Ok(None) => anyhow::bail!("WebSocket stream ended unexpectedly"),
            Ok(Some(Ok(_))) => anyhow::bail!("Unexpected WebSocket message type"),
        }
    }

    /// Subscribe to a document and wait for confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error if subscription fails.
    pub async fn subscribe(&mut self, doc_id: &DocumentId) -> anyhow::Result<()> {
        self.send(&ClientMessage::Subscribe { doc_id: doc_id.clone() }).await?;
        let response = self.recv().await?;
        if !matches!(response, ServerMessage::Subscribed { .. }) {
            anyhow::bail!("Expected Subscribed response, got {response:?}");
        }
        Ok(())
    }

    /// Send an encrypted CRDT update.
    ///
    /// # Errors
    ///
    /// Returns an error if sending fails.
    pub async fn send_update(
        &mut self,
        doc_id: &DocumentId,
        op: &EncryptedOp,
    ) -> anyhow::Result<()> {
        self.send(&ClientMessage::YrsUpdate {
            doc_id: doc_id.clone(),
            encrypted: op.ciphertext.clone(),
            epoch: op.epoch,
        })
        .await
    }

    /// Receive and parse a `YrsUpdate` message.
    ///
    /// # Errors
    ///
    /// Returns an error if the message is not a `YrsUpdate`.
    pub async fn recv_update(&mut self) -> anyhow::Result<EncryptedOp> {
        match self.recv().await? {
            ServerMessage::YrsUpdate { encrypted, epoch, .. } => {
                Ok(EncryptedOp { ciphertext: encrypted, epoch })
            }
            other => anyhow::bail!("Expected YrsUpdate, got {other:?}"),
        }
    }
}

/// Helper for setting up MLS groups between test users.
///
/// This struct manages the complexity of MLS key exchange for tests.
pub struct MlsTestGroup {
    /// Document ID for this group.
    pub doc_id: DocumentId,
    /// Owner's encrypted document.
    pub owner_doc: EncryptedDocument,
    /// Owner's user ID.
    pub owner_id: String,
}

impl MlsTestGroup {
    /// Create a new MLS group with an owner.
    ///
    /// # Errors
    ///
    /// Returns an error if MLS group creation fails.
    pub fn create(doc_id: &str, owner_id: &str) -> anyhow::Result<Self> {
        let owner_doc = EncryptedDocument::create(doc_id, owner_id)?;
        Ok(Self { doc_id: doc_id.to_string(), owner_doc, owner_id: owner_id.to_string() })
    }

    /// Generate a pending member that can join this group.
    ///
    /// # Errors
    ///
    /// Returns an error if key package generation fails.
    pub fn generate_joiner(user_id: &str) -> anyhow::Result<PendingMember> {
        Ok(MlsDocumentGroup::generate_key_package(user_id)?)
    }

    /// Add a member to the group and get the invite.
    ///
    /// # Errors
    ///
    /// Returns an error if adding member fails.
    pub fn add_member(&mut self, pending: &PendingMember) -> anyhow::Result<Invite> {
        Ok(self.owner_doc.create_invite(pending.key_package())?)
    }

    /// Create an encrypted document for a joiner using an invite.
    ///
    /// # Errors
    ///
    /// Returns an error if joining fails.
    pub fn join(invite: &Invite, pending: PendingMember) -> anyhow::Result<EncryptedDocument> {
        Ok(EncryptedDocument::join(invite, pending)?)
    }
}

/// Set up a complete two-user MLS group via the relay.
///
/// This handles all the WebSocket handshaking needed to establish an MLS group
/// between two connected clients.
///
/// # Returns
///
/// A tuple of (`alice_doc`, `bob_doc`) both ready for encrypted communication.
///
/// # Errors
///
/// Returns an error if any step of the setup fails.
pub async fn setup_two_user_group(
    alice: &mut TestClient,
    bob: &mut TestClient,
    doc_id: &DocumentId,
) -> anyhow::Result<(EncryptedDocument, EncryptedDocument)> {
    // Both subscribe to the document
    alice.subscribe(doc_id).await?;
    bob.subscribe(doc_id).await?;

    // Alice creates the encrypted document (she's the owner)
    let mut alice_doc = EncryptedDocument::create(doc_id, &alice.user_id)?;

    // Bob generates his key package
    let bob_pending = MlsDocumentGroup::generate_key_package(&bob.user_id)?;

    // Alice creates an invite for Bob
    let invite = alice_doc.create_invite(bob_pending.key_package())?;

    // Alice sends the welcome message via the relay
    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await?;

    // Bob receives the welcome
    let welcome_payload = match bob.recv().await? {
        ServerMessage::MlsHandshake { payload, .. } => payload,
        other => anyhow::bail!("Expected MlsHandshake, got {other:?}"),
    };

    // Bob joins using the welcome
    // MLS Protocol Detail: The Welcome message contains all state Bob needs to join.
    // The Commit message (empty here) is only needed to update *other* existing members
    // about the new joiner. In this 2-user case, Alice (the inviter) already processed
    // the commit when creating the invite, and there are no other existing members to
    // notify. For 3+ user groups, the commit would contain updates that existing members
    // must process to learn about Bob.
    let bob_invite =
        Invite { doc_id: doc_id.clone(), welcome: welcome_payload, commit: vec![], epoch: 1 };
    let bob_doc = EncryptedDocument::join(&bob_invite, bob_pending)?;

    Ok((alice_doc, bob_doc))
}

/// Set up a three-user MLS group via the relay.
///
/// # Returns
///
/// A tuple of (`alice_doc`, `bob_doc`, `charlie_doc`) all ready for encrypted communication.
///
/// # Errors
///
/// Returns an error if any step of the setup fails.
///
/// # Panics
///
/// Panics if draining broadcast messages fails due to connection errors.
pub async fn setup_three_user_group(
    alice: &mut TestClient,
    bob: &mut TestClient,
    charlie: &mut TestClient,
    doc_id: &DocumentId,
) -> anyhow::Result<(EncryptedDocument, EncryptedDocument, EncryptedDocument)> {
    // All subscribe to the document
    alice.subscribe(doc_id).await?;
    bob.subscribe(doc_id).await?;
    charlie.subscribe(doc_id).await?;

    // Alice creates the encrypted document
    let mut alice_doc = EncryptedDocument::create(doc_id, &alice.user_id)?;

    // Bob generates his key package and joins
    let bob_pending = MlsDocumentGroup::generate_key_package(&bob.user_id)?;
    let bob_invite = alice_doc.create_invite(bob_pending.key_package())?;

    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: bob_invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await?;

    // Bob receives welcome (Charlie also receives it as a subscriber)
    let bob_welcome = match bob.recv().await? {
        ServerMessage::MlsHandshake { payload, .. } => payload,
        other => anyhow::bail!("Bob expected MlsHandshake, got {other:?}"),
    };

    // Charlie receives the handshake too (but can't use it - it's for Bob)
    // Drain broadcast message not meant for this client
    let _ = charlie
        .try_recv(SHORT_TIMEOUT)
        .await
        .expect("Connection error while draining broadcast message");

    let bob_doc = EncryptedDocument::join(
        &Invite { doc_id: doc_id.clone(), welcome: bob_welcome, commit: vec![], epoch: 1 },
        bob_pending,
    )?;

    // Charlie generates his key package and joins
    let charlie_pending = MlsDocumentGroup::generate_key_package(&charlie.user_id)?;
    let charlie_invite = alice_doc.create_invite(charlie_pending.key_package())?;

    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: charlie_invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await?;

    // Charlie receives his welcome
    let charlie_welcome = match charlie.recv().await? {
        ServerMessage::MlsHandshake { payload, .. } => payload,
        other => anyhow::bail!("Charlie expected MlsHandshake, got {other:?}"),
    };

    // Bob also receives the handshake (but can't use it)
    // Drain broadcast message not meant for this client
    let _ = bob
        .try_recv(SHORT_TIMEOUT)
        .await
        .expect("Connection error while draining broadcast message");

    let charlie_doc = EncryptedDocument::join(
        &Invite { doc_id: doc_id.clone(), welcome: charlie_welcome, commit: vec![], epoch: 2 },
        charlie_pending,
    )?;

    Ok((alice_doc, bob_doc, charlie_doc))
}
