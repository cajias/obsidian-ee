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
    assert!(result.is_ok(), "WebSocket connect timed out after {TEST_TIMEOUT:?}");
    let ws_result = result.unwrap();
    assert!(ws_result.is_ok(), "WebSocket connect failed: {}", ws_result.unwrap_err());
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
    let json = serde_json::to_string(&msg)
        .unwrap_or_else(|e| panic!("Failed to serialize Identify message for {user_id}: {e}"));

    ws.send(Message::Text(json))
        .await
        .unwrap_or_else(|e| panic!("Failed to send Identify message: {e}"));

    let resp =
        timeout(SHORT_TIMEOUT, ws.next()).await.expect("Timeout waiting for Identified response");

    let msg = resp
        .expect("WebSocket stream closed before Identified response")
        .expect("WebSocket error during Identified handshake");

    let text = msg.to_text().expect("Received non-text WebSocket message during Identify");

    let server_msg: ServerMessage = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("Failed to parse Identified response: {e}. Got: {text}"));

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
    let json = serde_json::to_string(&msg)
        .unwrap_or_else(|e| panic!("Failed to serialize Subscribe message for {doc_id}: {e}"));

    ws.send(Message::Text(json))
        .await
        .unwrap_or_else(|e| panic!("Failed to send Subscribe message: {e}"));

    let resp =
        timeout(SHORT_TIMEOUT, ws.next()).await.expect("Timeout waiting for Subscribed response");

    let msg = resp
        .expect("WebSocket stream closed before Subscribed response")
        .expect("WebSocket error during Subscribed handshake");

    let text = msg.to_text().expect("Received non-text WebSocket message during Subscribe");

    let server_msg: ServerMessage = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("Failed to parse Subscribed response: {e}. Got: {text}"));

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
    let error_msg = match result {
        Err(_) => "connection timeout".to_string(),
        Ok(Err(e)) => e.to_string(),
        Ok(Ok(_)) => panic!("Connection to port 1 should fail"),
    };

    sm.on_error(&error_msg);

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
    let error_msg = match result {
        Err(_) => "connection timeout".to_string(),
        Ok(Err(e)) => e.to_string(),
        Ok(Ok(_)) => panic!("Connection to dead URL should fail"),
    };

    sm.on_error(&error_msg);

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

    // Verify we're in Failed state with an error about connection refused
    match sm.state() {
        ConnectionState::Failed { reason } => {
            assert!(
                reason.to_lowercase().contains("connection refused"),
                "Expected 'connection refused' in error, got: {reason}"
            );
        }
        other => panic!("Expected Failed state, got {other:?}"),
    }

    // Verify GiveUp action with error about connection refused
    match sm.next_action() {
        ConnectionAction::GiveUp { reason } => {
            assert!(
                reason.to_lowercase().contains("connection refused"),
                "Expected 'connection refused' in GiveUp reason, got: {reason}"
            );
        }
        other => panic!("Expected GiveUp action, got {other:?}"),
    }
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

/// Test that the state machine transitions to Reconnecting when the
/// application layer signals an error after a successful TCP connection.
///
/// This verifies the path where `on_connected()` has been called (TCP
/// handshake succeeded), the Identify exchange completes over the wire, but
/// the application layer calls `on_error()` — for example because the
/// server returned an `Error` response or business logic rejected the
/// session. The current relay always accepts `Identify`, so this test
/// simulates the rejection via `on_error()` rather than relying on a real
/// server rejection path.
#[tokio::test]
async fn test_app_layer_error_after_identify_triggers_retry() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();

    // Create config with 3 retries so we can observe retry behavior
    let config = config_with_retries(&url, 3);
    let mut sm = ConnectionStateMachine::new(config);

    // Drive to Connecting state
    assert_eq!(*sm.state(), ConnectionState::Connecting);

    // Actually connect (TCP handshake succeeds)
    let (mut ws, _) = timeout(TEST_TIMEOUT, connect_async(&url))
        .await
        .expect("WebSocket connect timed out")
        .unwrap_or_else(|e| panic!("WebSocket connect failed: {e}"));

    // Notify state machine of successful connection
    sm.on_connected();
    assert_eq!(*sm.state(), ConnectionState::Connected);

    // Get the IdentifyAndSubscribe action
    let action = sm.next_action();
    let (user_id, _doc_id) = extract_identify_subscribe(&action);

    // Send Identify message
    let msg = ClientMessage::Identify { user_id: user_id.clone() };
    let json = serde_json::to_string(&msg)
        .unwrap_or_else(|e| panic!("Failed to serialize Identify for {user_id}: {e}"));
    ws.send(Message::Text(json)).await.unwrap_or_else(|e| panic!("Failed to send Identify: {e}"));

    // Receive response - could be Identified or Error
    let resp =
        timeout(SHORT_TIMEOUT, ws.next()).await.expect("Timeout waiting for Identify response");

    let msg_result =
        resp.expect("WebSocket closed during Identify").expect("WebSocket error during Identify");

    let text = msg_result.to_text().expect("Non-text message during Identify");

    let server_msg: ServerMessage = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("Failed to parse response: {e}. Got: {text}"));

    // The current relay always responds with Identified (no rejection path).
    // We verify the server response, then simulate an application-layer error
    // to test the state machine's retry behavior after on_error().
    match server_msg {
        ServerMessage::Error { code, message } => {
            // Server rejected (future path) — feed the real error into the SM
            sm.on_error(&format!("Identification failed: {code:?} - {message}"));
        }
        ServerMessage::Identified { .. } => {
            // Normal path — simulate an app-layer error (e.g., version mismatch,
            // auth failure detected client-side, etc.)
            sm.on_error("simulated app-layer rejection after Identify");
        }
        other => {
            panic!("Expected Identified or Error response, got {other:?}");
        }
    }

    // Regardless of which branch, the state machine should retry
    assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });

    let retry_action = sm.next_action();
    assert!(
        matches!(retry_action, ConnectionAction::WaitAndRetry { attempt: 1, .. }),
        "Expected WaitAndRetry action after app-layer error, got {retry_action:?}"
    );
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
    let alice = a.unwrap_or_else(|e| panic!("Alice failed to connect: {e}")).0;
    let bob = b.unwrap_or_else(|e| panic!("Bob failed to connect: {e}")).0;
    (alice, bob)
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

/// Test that disconnection after `on_connected()` but before handshake triggers retry.
///
/// This tests the critical window between TCP handshake success and
/// application handshake completion. Network issues in this window
/// (NAT timeout, server restart, etc.) should trigger reconnection.
#[tokio::test]
async fn test_disconnect_before_handshake_retries() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();
    let config = auto_config(&url);
    let mut sm = ConnectionStateMachine::new(config);

    // Drive to Connecting state
    assert_eq!(*sm.state(), ConnectionState::Connecting);

    // Actually connect (TCP handshake succeeds)
    let result = timeout(TEST_TIMEOUT, connect_async(&url)).await;
    let ws_result = result.expect("WebSocket connect timed out");
    let ws = ws_result.unwrap_or_else(|e| panic!("WebSocket connect failed: {e}"));

    // Notify state machine of successful connection
    sm.on_connected();
    assert_eq!(*sm.state(), ConnectionState::Connected);

    // Connection drops immediately - before any handshake messages
    // In production this could be:
    // - NAT timeout
    // - Server restart
    // - Network partition
    // - Load balancer reconfiguration
    drop(ws);

    // Application detects the disconnect and notifies state machine
    sm.on_disconnected();

    // Should transition to Reconnecting, not Failed
    assert_eq!(
        *sm.state(),
        ConnectionState::Reconnecting { attempt: 1 },
        "State machine should retry after disconnect before handshake"
    );

    // Verify it will retry (not give up)
    let action = sm.next_action();
    assert!(
        matches!(action, ConnectionAction::WaitAndRetry { attempt: 1, .. }),
        "Expected WaitAndRetry action, got {action:?}"
    );

    // Verify the state machine can recover: advance through retry
    sm.on_retry_tick();
    assert_eq!(
        *sm.state(),
        ConnectionState::Connecting,
        "After retry tick, should be back in Connecting state"
    );

    // And successfully reconnect
    let result2 = timeout(TEST_TIMEOUT, connect_async(&url)).await;
    let ws_result2 = result2.expect("Second connect timed out");
    let _ws2 = ws_result2.unwrap_or_else(|e| panic!("Second connect failed: {e}"));

    sm.on_connected();
    assert_eq!(
        *sm.state(),
        ConnectionState::Connected,
        "State machine should reach Connected after successful reconnect"
    );
}

/// Test that the state machine handles race conditions gracefully.
///
/// This verifies:
/// 1. Calling `on_error()` while already in `Reconnecting` increments the attempt counter
/// 2. Calling `on_retry_tick()` multiple times is handled safely
///
/// These scenarios can occur in production when:
/// - Multiple concurrent connection attempts fail simultaneously
/// - Timer events and error events race each other
/// - Network flaps cause rapid error/retry cycles
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_concurrent_error_handling() {
    let dead_url = "ws://127.0.0.1:1";
    // Use max_retries=5 to allow room for testing race conditions
    let config = config_with_retries(dead_url, 5);
    let mut sm = ConnectionStateMachine::new(config);

    // Start in Connecting state
    assert_eq!(*sm.state(), ConnectionState::Connecting);

    // First connection fails
    let result = timeout(TEST_TIMEOUT, connect_async(dead_url)).await;
    let error_msg = match result {
        Err(_) => "connection timeout".to_string(),
        Ok(Err(e)) => e.to_string(),
        Ok(Ok(_)) => panic!("Connection should fail"),
    };

    sm.on_error(&error_msg);
    assert_eq!(
        *sm.state(),
        ConnectionState::Reconnecting { attempt: 1 },
        "First error should trigger reconnect"
    );

    // RACE CONDITION 1: Another error arrives while already reconnecting
    // This can happen if multiple concurrent operations fail
    sm.on_error("second concurrent error");

    // State machine should handle concurrent error gracefully - increments attempt
    // This is correct behavior: if you get another error while waiting to retry,
    // it counts as another failed attempt
    assert_eq!(
        *sm.state(),
        ConnectionState::Reconnecting { attempt: 2 },
        "Concurrent error should increment attempt counter"
    );

    // Advance through retry tick
    sm.on_retry_tick();
    assert_eq!(*sm.state(), ConnectionState::Connecting, "Retry tick should advance to Connecting");

    // Second connection attempt fails
    let result2 = timeout(TEST_TIMEOUT, connect_async(dead_url)).await;
    let error_msg2 = match result2 {
        Err(_) => "connection timeout".to_string(),
        Ok(Err(e)) => e.to_string(),
        Ok(Ok(_)) => panic!("Second connection should fail"),
    };

    sm.on_error(&error_msg2);
    assert_eq!(
        *sm.state(),
        ConnectionState::Reconnecting { attempt: 3 },
        "Third error (after concurrent error) should be attempt 3"
    );

    // RACE CONDITION 2: Multiple on_retry_tick() calls
    // This can happen if timer events fire rapidly or race with error handling
    sm.on_retry_tick();
    let state_after_first = sm.state().clone();

    // Call it again immediately
    sm.on_retry_tick();
    let state_after_second = sm.state().clone();

    // Both should transition to Connecting (idempotent behavior)
    assert_eq!(
        state_after_first,
        ConnectionState::Connecting,
        "First retry tick should transition to Connecting"
    );
    assert_eq!(
        state_after_second,
        ConnectionState::Connecting,
        "Second retry tick should remain in Connecting (idempotent)"
    );

    // Verify the state machine is still functional after race conditions
    let result3 = timeout(TEST_TIMEOUT, connect_async(dead_url)).await;
    let error_msg3 = match result3 {
        Err(_) => "connection timeout".to_string(),
        Ok(Err(e)) => e.to_string(),
        Ok(Ok(_)) => panic!("Third connection should fail"),
    };

    sm.on_error(&error_msg3);
    // We've had: initial error (1), concurrent error (2), second connection (3), third connection (4)
    assert_eq!(
        *sm.state(),
        ConnectionState::Reconnecting { attempt: 4 },
        "State machine should continue functioning correctly after race conditions"
    );
}

/// Test that jitter ranges are correctly computed alongside the state machine's
/// retry lifecycle in an e2e context.
///
/// The `ConnectionStateMachine` emits deterministic base delays in `WaitAndRetry`
/// actions. Callers are expected to use `RetryPolicy::delay_range_for_attempt()`
/// to obtain a jittered range and pick a random value within it. This test
/// verifies:
///
/// 1. The state machine emits the correct base delay for each retry attempt
/// 2. `delay_range_for_attempt()` produces a narrower range (min < base)
/// 3. Jitter ranges grow with exponential backoff but stay within bounds
/// 4. The jittered range always contains the base delay as its upper bound
///
/// This is exercised against a real relay server to confirm end-to-end behavior.
#[tokio::test]
async fn test_jitter_ranges_during_retry_lifecycle() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();

    // Policy: 50ms initial, 2x backoff, 500ms cap, 25% jitter, 4 retries
    let policy =
        RetryPolicy::new(4, Duration::from_millis(50), Duration::from_millis(500), 2.0, 0.25);
    let config =
        ConnectionConfig::new(&url, "jitter-user", DOC_ID).with_retry_policy(policy.clone());

    let mut sm = ConnectionStateMachine::new(config);

    // Connect successfully first, then disconnect to trigger retries
    drive_to_connected(&mut sm, &url).await;
    assert!(sm.is_connected());

    // Simulate disconnect to enter retry cycle
    sm.on_disconnected();
    assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });

    // Verify retry attempts with jitter ranges
    let expected_base_delays = [
        Duration::from_millis(50),  // attempt 0: 50ms * 2^0
        Duration::from_millis(100), // attempt 1: 50ms * 2^1
        Duration::from_millis(200), // attempt 2: 50ms * 2^2
        Duration::from_millis(400), // attempt 3: 50ms * 2^3
    ];

    for (i, expected_base) in expected_base_delays.iter().enumerate() {
        let attempt = (i + 1) as u32;

        // State machine should be in Reconnecting
        assert_eq!(
            *sm.state(),
            ConnectionState::Reconnecting { attempt },
            "Expected Reconnecting at attempt {attempt}"
        );

        // Verify the WaitAndRetry action has the deterministic base delay
        let action = sm.next_action();
        match &action {
            ConnectionAction::WaitAndRetry { delay, attempt: a } => {
                assert_eq!(delay, expected_base, "Base delay mismatch at attempt {a}");
                assert_eq!(*a, attempt);
            }
            other => panic!("Expected WaitAndRetry at attempt {attempt}, got {other:?}"),
        }

        // Verify jitter range from the policy (attempt is 1-based, index is 0-based)
        let (jitter_min, jitter_max) = policy
            .delay_range_for_attempt(attempt - 1)
            .expect("Should have delay for this attempt");

        // Jitter max should equal the base delay
        assert_eq!(
            jitter_max, *expected_base,
            "Jitter max should equal base delay at attempt {attempt}"
        );

        // Jitter min should be base - 25% = 75% of base
        let expected_min = expected_base.mul_f64(0.75);
        assert_eq!(
            jitter_min, expected_min,
            "Jitter min should be 75% of base at attempt {attempt}"
        );

        // The jitter range must be non-empty
        assert!(jitter_min <= jitter_max, "Jitter range must be non-empty");

        // Advance to next attempt (unless this is the last one)
        if i < expected_base_delays.len() - 1 {
            sm.on_retry_tick();
            sm.on_error("simulated failure for jitter test");
        }
    }

    // After exhausting all 4 retries, the next error should transition to Failed
    sm.on_retry_tick();
    sm.on_error("final failure");
    assert!(
        matches!(sm.state(), ConnectionState::Failed { .. }),
        "Should be Failed after exhausting retries, got {:?}",
        sm.state()
    );

    // Verify policy returns None for out-of-range attempts
    assert!(
        policy.delay_range_for_attempt(4).is_none(),
        "Should return None for attempt beyond max_retries"
    );
}

/// Test that the state machine reconnects after WebSocket close frames.
///
/// This verifies that when the application layer detects a WebSocket close
/// (of any kind) and calls `on_disconnected()`, the state machine transitions
/// to `Reconnecting` rather than `Failed`. Three scenarios are exercised:
///
/// - **Normal close (1000)**: Clean shutdown — still reconnects for resilience.
/// - **Going Away (1001)**: Server restarting — reconnects to pick up new instance.
/// - **Abnormal close**: Connection dropped without a close frame (network issue).
///
/// Note: In a real deployment the *server* initiates the close, but in this
/// test the close frames are sent from the client-side WebSocket handle
/// (we don't have a hook into the relay to force server-side closes). The
/// important thing being tested is the state machine's reaction to
/// `on_disconnected()` after each scenario, not who initiates the close.
#[tokio::test]
#[allow(clippy::too_many_lines, clippy::excessive_nesting)]
async fn test_close_frame_handling_triggers_reconnect() {
    let server = TestServer::start().await;
    let url = server.url().to_owned();
    let config = auto_config(&url);

    // Test Case 1: Normal close (1000)
    {
        let mut sm = ConnectionStateMachine::new(config.clone());
        assert_eq!(*sm.state(), ConnectionState::Connecting);

        // Actually connect
        let result = timeout(TEST_TIMEOUT, connect_async(&url)).await;
        let ws_result = result.expect("WebSocket connect timed out");
        let (mut ws, _response) =
            ws_result.unwrap_or_else(|e| panic!("WebSocket connect failed: {e}"));

        sm.on_connected();
        assert_eq!(*sm.state(), ConnectionState::Connected);

        // Send normal close frame (simulates server-initiated close)
        ws.send(Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: std::borrow::Cow::Borrowed("server shutdown"),
        })))
        .await
        .expect("Failed to send close frame");

        // Read the close frame echo (WebSocket close handshake)
        // Various outcomes are acceptable: close ack, stream end, timeout, or error
        if let Ok(Some(Ok(msg))) = timeout(SHORT_TIMEOUT, ws.next()).await {
            assert!(
                matches!(msg, Message::Close(_)),
                "Expected Close frame or connection end, got {msg:?}"
            );
        }

        // Application layer detects the close and notifies state machine
        sm.on_disconnected();

        // Should transition to Reconnecting (resilience despite clean close)
        assert_eq!(
            *sm.state(),
            ConnectionState::Reconnecting { attempt: 1 },
            "Normal close (1000) should trigger reconnection for resilience"
        );

        // Verify retry is possible
        let action = sm.next_action();
        assert!(
            matches!(action, ConnectionAction::WaitAndRetry { attempt: 1, .. }),
            "Expected WaitAndRetry action after normal close, got {action:?}"
        );
    }

    // Test Case 2: Going Away close (1001)
    {
        let mut sm = ConnectionStateMachine::new(config.clone());
        assert_eq!(*sm.state(), ConnectionState::Connecting);

        let result = timeout(TEST_TIMEOUT, connect_async(&url)).await;
        let ws_result = result.expect("WebSocket connect timed out");
        let (mut ws, _response) =
            ws_result.unwrap_or_else(|e| panic!("WebSocket connect failed: {e}"));

        sm.on_connected();
        assert_eq!(*sm.state(), ConnectionState::Connected);

        // Send "going away" close frame (simulates server restart/maintenance)
        ws.send(Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Away,
            reason: std::borrow::Cow::Borrowed("server restarting"),
        })))
        .await
        .expect("Failed to send close frame");

        // Drain the close frame echo (close handshake)
        let _ = timeout(SHORT_TIMEOUT, ws.next()).await;

        // Application detects close
        sm.on_disconnected();

        // Should trigger reconnection (server might come back)
        assert_eq!(
            *sm.state(),
            ConnectionState::Reconnecting { attempt: 1 },
            "Going Away (1001) should trigger reconnection"
        );
    }

    // Test Case 3: Abnormal close (connection drop without close frame)
    // This is simulated by dropping the connection without sending a close frame
    {
        let mut sm = ConnectionStateMachine::new(config);
        assert_eq!(*sm.state(), ConnectionState::Connecting);

        let result = timeout(TEST_TIMEOUT, connect_async(&url)).await;
        let ws_result = result.expect("WebSocket connect timed out");
        let (ws, _response) = ws_result.unwrap_or_else(|e| panic!("WebSocket connect failed: {e}"));

        sm.on_connected();
        assert_eq!(*sm.state(), ConnectionState::Connected);

        // Drop connection without proper close handshake (simulates network issue)
        drop(ws);

        // Application detects abnormal close
        sm.on_disconnected();

        // Should trigger reconnection (transient network issue)
        assert_eq!(
            *sm.state(),
            ConnectionState::Reconnecting { attempt: 1 },
            "Abnormal close (1006) should trigger reconnection"
        );

        // Verify state machine can recover
        sm.on_retry_tick();
        assert_eq!(
            *sm.state(),
            ConnectionState::Connecting,
            "After abnormal close, should be able to retry"
        );
    }
}
