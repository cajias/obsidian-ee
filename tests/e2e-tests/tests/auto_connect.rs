//! End-to-end tests for the auto-connect state machine feature.
//!
//! These tests verify that the [`ConnectionStateMachine`] integrates correctly
//! with a real relay server over WebSocket. They cover:
//!
//! - Auto-connect on startup with a live relay
//! - Retry behaviour when the relay is unreachable
//! - Retry exhaustion and permanent failure
//! - Reconnection after a transient disconnect
//! - Full Identify + Subscribe handshake driven by state machine actions
//! - Two independent clients auto-connecting in parallel
//! - Disabled auto-connect staying idle until explicitly connected
//!
//! All tests spin up an in-process relay via [`TestServer::start`] (no Docker
//! required) and use `tokio_tungstenite::connect_async` for real WebSocket I/O.

use std::time::Duration;

use collab_core::{
    ConnectionAction, ConnectionConfig, ConnectionState, ConnectionStateMachine, RetryPolicy,
};
use collab_proto::{ClientMessage, ServerMessage};
use e2e_tests::helpers::{TestServer, SHORT_TIMEOUT};
use futures::{SinkExt, StreamExt};
use pretty_assertions::assert_eq;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Default document ID used throughout these tests.
const DOC_ID: &str = "auto-connect-doc";

/// Timeout for operations that must complete quickly.
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Build a [`ConnectionConfig`] pointing at `relay_url` with auto-connect
/// enabled and the default retry policy.
fn auto_config(relay_url: &str) -> ConnectionConfig {
    ConnectionConfig::new(relay_url, "user-1", DOC_ID)
}

/// Build a [`ConnectionConfig`] with auto-connect disabled.
fn manual_config(relay_url: &str) -> ConnectionConfig {
    ConnectionConfig::new(relay_url, "user-1", DOC_ID).with_auto_connect(false)
}

/// Build a [`ConnectionConfig`] with a custom retry policy.
fn config_with_retries(relay_url: &str, max_retries: u32) -> ConnectionConfig {
    let policy = RetryPolicy::new(
        max_retries,
        Duration::from_millis(10),
        Duration::from_millis(100),
        2.0,
        0.0,
    );
    ConnectionConfig::new(relay_url, "user-1", DOC_ID).with_retry_policy(policy)
}

/// Assert the state machine is in the expected state and returns the expected
/// action.
fn assert_sm(
    sm: &ConnectionStateMachine,
    expected_state: &ConnectionState,
    expected_action: &ConnectionAction,
) {
    assert_eq!(sm.state(), expected_state);
    assert_eq!(&sm.next_action(), expected_action);
}

/// Drive a state machine from `Connecting` to `Connected` by actually opening
/// a WebSocket to the given URL and notifying the state machine.
async fn drive_to_connected(sm: &mut ConnectionStateMachine, url: &str) {
    let result = timeout(TEST_TIMEOUT, connect_async(url)).await;
    assert!(result.is_ok(), "WebSocket connect timed out");
    assert!(result.unwrap().is_ok(), "WebSocket connect failed");
    sm.on_connected();
}

/// Send `Identify` over a raw WebSocket and assert the `Identified` response.
async fn send_identify(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    user_id: &str,
) {
    let msg = ClientMessage::Identify { user_id: user_id.to_string() };
    let json = serde_json::to_string(&msg).unwrap();
    ws.send(Message::Text(json)).await.unwrap();

    let resp = timeout(SHORT_TIMEOUT, ws.next()).await.unwrap().unwrap().unwrap();
    let server_msg: ServerMessage = serde_json::from_str(resp.to_text().unwrap()).unwrap();
    assert!(
        matches!(server_msg, ServerMessage::Identified { user_id: ref uid } if uid == user_id),
        "Expected Identified, got {server_msg:?}"
    );
}

/// Send `Subscribe` over a raw WebSocket and assert the `Subscribed` response.
async fn send_subscribe(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    doc_id: &str,
) {
    let msg = ClientMessage::Subscribe { doc_id: doc_id.to_string() };
    let json = serde_json::to_string(&msg).unwrap();
    ws.send(Message::Text(json)).await.unwrap();

    let resp = timeout(SHORT_TIMEOUT, ws.next()).await.unwrap().unwrap().unwrap();
    let server_msg: ServerMessage = serde_json::from_str(resp.to_text().unwrap()).unwrap();
    assert!(
        matches!(server_msg, ServerMessage::Subscribed { doc_id: ref did } if did == doc_id),
        "Expected Subscribed, got {server_msg:?}"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Verify that a state machine with `auto_connect=true` starts in `Connecting`,
/// transitions to `Connected` after a real WebSocket connection, and then emits
/// `IdentifyAndSubscribe`.
#[tokio::test]
async fn test_auto_connect_to_real_relay() {
    let server = TestServer::start().await;
    let config = auto_config(server.url());
    let relay_url = server.url().to_owned();

    let mut sm = ConnectionStateMachine::new(config);

    assert_sm(
        &sm,
        &ConnectionState::Connecting,
        &ConnectionAction::Connect { relay_url: relay_url.clone() },
    );

    drive_to_connected(&mut sm, &relay_url).await;

    assert_sm(
        &sm,
        &ConnectionState::Connected,
        &ConnectionAction::IdentifyAndSubscribe { user_id: "user-1".into(), doc_id: DOC_ID.into() },
    );
}

/// Attempting to connect to a port that refuses connections should cause the
/// state machine to transition to `Reconnecting { attempt: 1 }` with a
/// `WaitAndRetry` action.
#[tokio::test]
async fn test_connect_to_nonexistent_relay_triggers_retry() {
    let dead_url = "ws://127.0.0.1:1";
    let config = config_with_retries(dead_url, 3);

    let mut sm = ConnectionStateMachine::new(config);
    assert_eq!(sm.state(), &ConnectionState::Connecting);

    // Attempt the real connection -- it will fail.
    let result = timeout(TEST_TIMEOUT, connect_async(dead_url)).await;
    let connect_failed = result.is_err() || result.unwrap().is_err();
    assert!(connect_failed, "Connection to port 1 should fail");

    sm.on_error("connection refused");

    assert_eq!(sm.state(), &ConnectionState::Reconnecting { attempt: 1 });
    let action = sm.next_action();
    assert!(
        matches!(action, ConnectionAction::WaitAndRetry { attempt: 1, .. }),
        "Expected WaitAndRetry, got {action:?}"
    );
}

/// Try connecting to `dead_url`, assert failure, then call `sm.on_error`.
/// If `expect_reconnecting` is `Some(attempt)`, asserts the state machine
/// transitions to `Reconnecting` and advances via `on_retry_tick`.
async fn fail_one_attempt(
    sm: &mut ConnectionStateMachine,
    dead_url: &str,
    expect_reconnecting: Option<u32>,
) {
    let result = timeout(TEST_TIMEOUT, connect_async(dead_url)).await;
    let failed = result.is_err() || result.unwrap().is_err();
    assert!(failed, "Connection to dead URL should fail");

    sm.on_error("connection refused");

    if let Some(attempt) = expect_reconnecting {
        assert_eq!(sm.state(), &ConnectionState::Reconnecting { attempt });
        sm.on_retry_tick();
        assert_eq!(sm.state(), &ConnectionState::Connecting);
    }
}

/// After exhausting all retries the state machine must transition to `Failed`
/// and emit `GiveUp`.
#[tokio::test]
async fn test_retry_exhaustion_gives_up() {
    let dead_url = "ws://127.0.0.1:1";
    let config = config_with_retries(dead_url, 2);
    let mut sm = ConnectionStateMachine::new(config);

    // Attempts 1 and 2: fail but can still retry
    fail_one_attempt(&mut sm, dead_url, Some(1)).await;
    fail_one_attempt(&mut sm, dead_url, Some(2)).await;

    // Attempt 3: exceeds max_retries -- should transition to Failed
    fail_one_attempt(&mut sm, dead_url, None).await;

    assert_eq!(sm.state(), &ConnectionState::Failed { reason: "connection refused".into() });
    assert_eq!(sm.next_action(), ConnectionAction::GiveUp { reason: "connection refused".into() });
}

/// After a transient disconnect the state machine should cycle through
/// `Reconnecting` -> `Connecting` -> `Connected` when the server is still up.
#[tokio::test]
async fn test_reconnect_after_server_restart() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();
    let config = auto_config(&url);
    let mut sm = ConnectionStateMachine::new(config);

    // Phase 1: initial connection
    drive_to_connected(&mut sm, &url).await;
    assert!(sm.is_connected());

    // Phase 2: simulate disconnect
    sm.on_disconnected();
    assert_eq!(sm.state(), &ConnectionState::Reconnecting { attempt: 1 });

    // Phase 3: retry tick moves back to Connecting
    sm.on_retry_tick();
    assert_eq!(sm.state(), &ConnectionState::Connecting);

    // Phase 4: reconnect to the still-running server
    drive_to_connected(&mut sm, &url).await;
    assert!(sm.is_connected());
}

/// Drive the state machine all the way through the Identify + Subscribe
/// handshake using a real WebSocket connection to the relay.
#[tokio::test]
async fn test_full_identify_subscribe_handshake() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();
    let config = auto_config(&url);
    let mut sm = ConnectionStateMachine::new(config);

    // Connect
    let (mut ws, _) = timeout(TEST_TIMEOUT, connect_async(&url)).await.unwrap().unwrap();
    sm.on_connected();

    // The state machine tells us to identify and subscribe.
    let action = sm.next_action();
    let (user_id, doc_id) = extract_identify_subscribe(&action);

    // Perform the handshake over the real connection.
    send_identify(&mut ws, &user_id).await;
    send_subscribe(&mut ws, &doc_id).await;

    // State machine is still Connected -- handshake is an application concern.
    assert!(sm.is_connected());
}

/// Extract `user_id` and `doc_id` from an `IdentifyAndSubscribe` action.
/// Panics if the action is not `IdentifyAndSubscribe`.
fn extract_identify_subscribe(action: &ConnectionAction) -> (String, String) {
    match action {
        ConnectionAction::IdentifyAndSubscribe { user_id, doc_id } => {
            (user_id.clone(), doc_id.clone())
        }
        other => panic!("Expected IdentifyAndSubscribe, got {other:?}"),
    }
}

/// Two independent state machines with different user IDs can auto-connect,
/// identify, and subscribe to the same relay without interference.
#[tokio::test]
async fn test_two_clients_auto_connect_independently() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();

    let config_a = ConnectionConfig::new(&url, "alice", DOC_ID);
    let config_b = ConnectionConfig::new(&url, "bob", DOC_ID);

    let mut sm_a = ConnectionStateMachine::new(config_a);
    let mut sm_b = ConnectionStateMachine::new(config_b);

    // Both connect in parallel.
    let (ws_a, ws_b) = connect_two_clients(&url).await;
    let mut ws_a = ws_a;
    let mut ws_b = ws_b;

    sm_a.on_connected();
    sm_b.on_connected();

    // Handshake Alice
    let (uid_a, did_a) = extract_identify_subscribe(&sm_a.next_action());
    send_identify(&mut ws_a, &uid_a).await;
    send_subscribe(&mut ws_a, &did_a).await;

    // Handshake Bob
    let (uid_b, did_b) = extract_identify_subscribe(&sm_b.next_action());
    send_identify(&mut ws_b, &uid_b).await;
    send_subscribe(&mut ws_b, &did_b).await;

    assert!(sm_a.is_connected());
    assert!(sm_b.is_connected());
}

/// Open two WebSocket connections to `url` concurrently.
async fn connect_two_clients(
    url: &str,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) {
    let (a, b) = tokio::join!(connect_async(url), connect_async(url));
    (a.unwrap().0, b.unwrap().0)
}

/// With `auto_connect=false` the state machine starts `Disconnected` and emits
/// `DoNothing`. Calling `connect()` transitions to `Connecting`.
#[tokio::test]
async fn test_auto_connect_disabled_stays_disconnected() {
    let server = TestServer::start().await;
    let config = manual_config(server.url());
    let relay_url = server.url().to_owned();

    let mut sm = ConnectionStateMachine::new(config);

    assert_sm(&sm, &ConnectionState::Disconnected, &ConnectionAction::DoNothing);

    sm.connect();

    assert_sm(&sm, &ConnectionState::Connecting, &ConnectionAction::Connect { relay_url });
}
