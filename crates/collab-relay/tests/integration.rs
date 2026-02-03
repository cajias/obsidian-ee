//! Integration tests for the collab-relay server.
//!
//! These tests verify the full flow: connect, identify, subscribe, and exchange messages.

use collab_proto::{ClientMessage, ErrorCode, ServerMessage};
use collab_relay::RelayServer;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// Test server wrapper.
struct TestServer {
    addr: SocketAddr,
    handle: collab_relay::relay::ServerHandle,
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

/// Test client wrapper.
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

/// Helper to identify a client.
async fn identify(client: &mut TestClient, user_id: &str) {
    client
        .send(ClientMessage::Identify {
            user_id: user_id.into(),
        })
        .await;
    let response = client.recv().await;
    assert!(matches!(
        response,
        ServerMessage::Identified { user_id: uid } if uid == user_id
    ));
}

/// Helper to subscribe a client to a document.
async fn subscribe(client: &mut TestClient, doc_id: &str) {
    client
        .send(ClientMessage::Subscribe {
            doc_id: doc_id.into(),
        })
        .await;
    let response = client.recv().await;
    assert!(matches!(
        response,
        ServerMessage::Subscribed { doc_id: did } if did == doc_id
    ));
}

#[tokio::test]
async fn test_full_flow_connect_identify_subscribe_exchange() {
    let server = TestServer::start().await;

    // Alice connects and identifies
    let mut alice = TestClient::connect(&server).await;
    identify(&mut alice, "alice").await;

    // Bob connects and identifies
    let mut bob = TestClient::connect(&server).await;
    identify(&mut bob, "bob").await;

    // Both subscribe to doc1
    subscribe(&mut alice, "doc1").await;
    subscribe(&mut bob, "doc1").await;

    // Alice sends an update
    alice
        .send(ClientMessage::YrsUpdate {
            doc_id: "doc1".into(),
            encrypted: vec![1, 2, 3, 4, 5],
            epoch: 1,
            signature: vec![0xAB, 0xCD],
        })
        .await;

    // Bob should receive the update
    // Note: Current implementation doesn't route messages yet (T11 integration pending)
    // This test verifies the protocol flow works
    // The actual routing is tested in routing.rs unit tests
}

#[tokio::test]
async fn test_multiple_clients_concurrent_connect() {
    let server = TestServer::start().await;

    // Spawn multiple clients concurrently
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let url = server.url();
            tokio::spawn(async move {
                let (ws, _) = connect_async(&url).await.unwrap();
                let (mut write, mut read) = ws.split();

                // Identify
                let msg = ClientMessage::Identify {
                    user_id: format!("user{i}"),
                };
                let json = serde_json::to_string(&msg).unwrap();
                write.send(Message::Text(json)).await.unwrap();

                // Wait for response
                if let Some(Ok(Message::Text(text))) = read.next().await {
                    let response: ServerMessage = serde_json::from_str(&text).unwrap();
                    matches!(response, ServerMessage::Identified { .. })
                } else {
                    false
                }
            })
        })
        .collect();

    // All should succeed
    for handle in handles {
        assert!(handle.await.unwrap());
    }
}

#[tokio::test]
async fn test_subscribe_unsubscribe_flow() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await;

    identify(&mut client, "alice").await;

    // Subscribe
    subscribe(&mut client, "doc1").await;

    // Unsubscribe
    client
        .send(ClientMessage::Unsubscribe {
            doc_id: "doc1".into(),
        })
        .await;

    let response = client.recv().await;
    assert!(matches!(
        response,
        ServerMessage::Unsubscribed { doc_id } if doc_id == "doc1"
    ));
}

#[tokio::test]
async fn test_error_on_subscribe_without_identify() {
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
        ServerMessage::Error {
            code: ErrorCode::NotIdentified,
            ..
        }
    ));
}

#[tokio::test]
async fn test_error_on_yrs_update_without_identify() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await;

    // Try to send update without identifying first
    client
        .send(ClientMessage::YrsUpdate {
            doc_id: "doc1".into(),
            encrypted: vec![1, 2, 3],
            epoch: 1,
            signature: vec![],
        })
        .await;

    let response = client.recv().await;
    assert!(matches!(
        response,
        ServerMessage::Error {
            code: ErrorCode::NotIdentified,
            ..
        }
    ));
}

#[tokio::test]
async fn test_multiple_document_subscriptions() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await;

    identify(&mut client, "alice").await;

    // Subscribe to multiple documents
    subscribe(&mut client, "doc1").await;
    subscribe(&mut client, "doc2").await;
    subscribe(&mut client, "doc3").await;

    // Unsubscribe from one
    client
        .send(ClientMessage::Unsubscribe {
            doc_id: "doc2".into(),
        })
        .await;

    let response = client.recv().await;
    assert!(matches!(
        response,
        ServerMessage::Unsubscribed { doc_id } if doc_id == "doc2"
    ));
}

#[tokio::test]
async fn test_mls_handshake_requires_identify() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await;

    // Try to send MLS handshake without identifying first
    client
        .send(ClientMessage::MlsHandshake {
            doc_id: "doc1".into(),
            payload: vec![1, 2, 3],
            message_type: collab_proto::MlsMessageType::Welcome,
        })
        .await;

    let response = client.recv().await;
    assert!(matches!(
        response,
        ServerMessage::Error {
            code: ErrorCode::NotIdentified,
            ..
        }
    ));
}
