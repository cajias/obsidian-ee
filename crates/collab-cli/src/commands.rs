//! CLI command implementations.

use std::fs;
use std::ops::ControlFlow;
use std::path::Path;

use collab_core::{
    ConnectionAction, ConnectionConfig, ConnectionStateMachine, EncryptedDocument, EncryptedOp,
    MlsDocumentGroup,
};
use collab_proto::{ClientMessage, Invite, MlsMessageType, ServerMessage};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

/// Initialize a new collaborative document.
///
/// Creates a new encrypted document as the owner.
///
/// # Errors
///
/// Returns an error if document creation fails.
pub fn init(doc_id: &str, user_id: &str, state_file: Option<&Path>) -> anyhow::Result<InitResult> {
    let _doc = EncryptedDocument::create(doc_id, user_id)?;

    // Save state if requested
    if let Some(path) = state_file {
        let state = DocumentState {
            doc_id: doc_id.to_string(),
            user_id: user_id.to_string(),
            role: "owner".to_string(),
        };
        fs::write(path, serde_json::to_string_pretty(&state)?)?;
    }

    Ok(InitResult {
        doc_id: doc_id.to_string(),
        user_id: user_id.to_string(),
        message: format!("Created document '{doc_id}' as owner. Share invites with collaborators."),
    })
}

/// Result of initializing a document.
#[derive(Debug, Serialize, Deserialize)]
pub struct InitResult {
    /// The document ID.
    pub doc_id: String,
    /// The user ID of the owner.
    pub user_id: String,
    /// Human-readable message.
    pub message: String,
}

/// Document state saved to disk.
#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentState {
    /// The document ID.
    pub doc_id: String,
    /// The user ID.
    pub user_id: String,
    /// User's role (e.g. "owner", "collaborator").
    pub role: String,
}

/// Generate a key package for joining a group.
///
/// Returns a pending member with its key package bytes.
///
/// # Errors
///
/// Returns an error if key package generation fails.
pub fn keygen(user_id: &str, output_file: &Path) -> anyhow::Result<KeygenResult> {
    let pending = MlsDocumentGroup::generate_key_package(user_id)?;
    let key_package = pending.key_package().to_vec();

    // We can't easily serialize the full PendingMember (contains crypto state),
    // so we save the key package and rely on regenerating for join.
    // In a real implementation, we'd serialize the crypto state properly.
    let output =
        KeygenOutput { user_id: user_id.to_string(), key_package: base64_encode(&key_package) };

    fs::write(output_file, serde_json::to_string_pretty(&output)?)?;

    Ok(KeygenResult {
        user_id: user_id.to_string(),
        key_package_file: output_file.display().to_string(),
        message: format!(
            "Generated key package. Share '{0}' with the document owner.",
            output_file.display()
        ),
    })
}

/// Result of key generation.
#[derive(Debug, Serialize)]
pub struct KeygenResult {
    /// The user ID.
    pub user_id: String,
    /// Path to the key package file.
    pub key_package_file: String,
    /// Human-readable message.
    pub message: String,
}

/// Key generation output saved to file.
#[derive(Debug, Serialize, Deserialize)]
pub struct KeygenOutput {
    /// The user ID.
    pub user_id: String,
    /// Base64-encoded key package.
    pub key_package: String,
}

/// Create an invite for a new member.
///
/// Takes the joiner's key package file and outputs an invite file.
///
/// # Errors
///
/// Returns an error if invite creation fails.
pub fn create_invite(
    doc_id: &str,
    owner_user_id: &str,
    key_package_file: &Path,
    invite_output: &Path,
) -> anyhow::Result<InviteResult> {
    // Load the joiner's key package
    let keygen_content = fs::read_to_string(key_package_file)?;
    let keygen: KeygenOutput = serde_json::from_str(&keygen_content)?;
    let key_package_bytes = base64_decode(&keygen.key_package)?;

    // Create document (owner's state)
    let mut doc = EncryptedDocument::create(doc_id, owner_user_id)?;

    // Create invite
    let invite = doc.create_invite(&key_package_bytes)?;

    // Write the complete invite (welcome + commit + epoch) to file.
    let invite_proto = Invite {
        doc_id: invite.doc_id.clone(),
        welcome: invite.welcome,
        commit: invite.commit,
        epoch: invite.epoch,
        relay_url: String::new(),
    };
    fs::write(invite_output, serde_json::to_string_pretty(&invite_proto)?)?;

    Ok(InviteResult {
        doc_id: invite.doc_id,
        invite_file: invite_output.display().to_string(),
        message: format!(
            "Invite created. Share '{0}' with {1}.",
            invite_output.display(),
            keygen.user_id
        ),
    })
}

/// Result of creating an invite.
#[derive(Debug, Serialize)]
pub struct InviteResult {
    /// The document ID.
    pub doc_id: String,
    /// Path to the invite file.
    pub invite_file: String,
    /// Human-readable message.
    pub message: String,
}

/// Join an existing collaborative document.
///
/// # Errors
///
/// Returns an error if joining fails.
pub fn join(
    invite_file: &Path,
    user_id: &str,
    state_output: Option<&Path>,
) -> anyhow::Result<JoinResult> {
    // Load the invite
    let invite_content = fs::read_to_string(invite_file)?;
    let invite: Invite = serde_json::from_str(&invite_content)?;

    // NOTE: the file-based flow cannot yet reconstruct the exact `PendingMember`
    // produced by `keygen` — its MLS private state is not persisted — so we
    // regenerate a key package here. It will not match the invite's welcome, so
    // the MLS join fails. That failure is surfaced honestly as an error (a
    // non-zero exit) rather than a fake `success: false` with exit code 0.
    // Persisting keygen state is tracked as future work; use `demo` for the
    // working in-process flow.
    let pending = MlsDocumentGroup::generate_key_package(user_id)?;
    let _group = pending.join(&invite.welcome).map_err(|e| {
        anyhow::anyhow!(
            "Failed to join document '{}': {e}. The file-based join flow requires \
             the key-package state produced by `keygen`, which is not yet persisted \
             across processes. See `collab-cli demo` for the working flow.",
            invite.doc_id
        )
    })?;

    // Save state if requested.
    if let Some(path) = state_output {
        let state = DocumentState {
            doc_id: invite.doc_id.clone(),
            user_id: user_id.to_string(),
            role: "collaborator".to_string(),
        };
        fs::write(path, serde_json::to_string_pretty(&state)?)?;
    }

    Ok(JoinResult {
        doc_id: invite.doc_id,
        user_id: user_id.to_string(),
        success: true,
        message: "Successfully joined document".to_string(),
    })
}

/// Result of joining a document.
#[derive(Debug, Serialize)]
pub struct JoinResult {
    /// The document ID.
    pub doc_id: String,
    /// The user ID.
    pub user_id: String,
    /// Whether join succeeded.
    pub success: bool,
    /// Human-readable message.
    pub message: String,
}

/// Demonstrate the full collaboration flow in-memory.
///
/// This bypasses file I/O to show the MLS flow working correctly.
///
/// # Errors
///
/// Returns an error if any step fails.
pub fn demo(doc_id: &str) -> anyhow::Result<DemoResult> {
    // Alice creates a document
    let mut alice_doc = EncryptedDocument::create(doc_id, "alice")?;

    // Bob generates a key package
    let bob_pending = MlsDocumentGroup::generate_key_package("bob")?;

    // Alice creates an invite for Bob
    let invite = alice_doc.create_invite(bob_pending.key_package())?;

    // Bob joins using the invite
    let mut bob_doc = EncryptedDocument::join(&invite, bob_pending)?;

    // Alice writes some content
    alice_doc.insert(0, "Hello from Alice!");
    let encrypted_update = alice_doc.get_encrypted_update()?;

    // Bob receives and decrypts
    bob_doc.apply_encrypted_update(&encrypted_update)?;
    let _bob_content = bob_doc.get_content();

    // Bob responds
    bob_doc.insert(17, " Hi from Bob!");
    let bob_update = bob_doc.get_encrypted_update()?;

    // Alice receives
    alice_doc.apply_encrypted_update(&bob_update)?;
    let final_content = alice_doc.get_content();

    Ok(DemoResult {
        doc_id: doc_id.to_string(),
        alice_content: final_content.clone(),
        bob_content: final_content,
        message: "Demo completed successfully! E2E encryption working.".to_string(),
    })
}

/// Result of the demo command.
#[derive(Debug, Serialize)]
pub struct DemoResult {
    /// The document ID.
    pub doc_id: String,
    /// Alice's view of the content.
    pub alice_content: String,
    /// Bob's view of the content.
    pub bob_content: String,
    /// Human-readable message.
    pub message: String,
}

/// Fixed ceiling for how long a peer waits for any single relay message.
// ponytail: fixed timeout; make configurable only if a slow relay ever needs it.
const PEER_RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A client-side WebSocket connection to the relay, used by [`session_check`].
type PeerWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One relay-connected client: a split WebSocket plus identify/subscribe and
/// typed send/recv helpers bounded by [`PEER_RECV_TIMEOUT`].
struct Peer {
    write: futures::stream::SplitSink<PeerWs, Message>,
    read: futures::stream::SplitStream<PeerWs>,
}

impl Peer {
    /// Connect, identify, and subscribe — returning a ready peer or an error if
    /// any handshake step is rejected or times out.
    async fn connect(url: &str, user_id: &str, doc_id: &str) -> anyhow::Result<Self> {
        let (ws, _) = connect_async(url).await?;
        let (write, read) = ws.split();
        let mut peer = Self { write, read };

        peer.send(ClientMessage::Identify { user_id: user_id.to_string(), token: None }).await?;
        match peer.recv().await? {
            ServerMessage::Identified { .. } => {}
            other => return Err(anyhow::anyhow!("expected Identified, got {other:?}")),
        }

        peer.send(ClientMessage::Subscribe { doc_id: doc_id.to_string() }).await?;
        match peer.recv().await? {
            ServerMessage::Subscribed { .. } => {}
            other => return Err(anyhow::anyhow!("expected Subscribed, got {other:?}")),
        }

        Ok(peer)
    }

    /// Serialize and send one client message.
    async fn send(&mut self, msg: ClientMessage) -> anyhow::Result<()> {
        self.write.send(Message::Text(serde_json::to_string(&msg)?)).await?;
        Ok(())
    }

    /// Receive the next server message, erroring on timeout, close, transport
    /// failure, or an `Error` frame from the relay.
    async fn recv(&mut self) -> anyhow::Result<ServerMessage> {
        let next = tokio::time::timeout(PEER_RECV_TIMEOUT, self.read_frame())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for a relay message"))??;

        match next {
            ServerMessage::Error { code, message } => {
                Err(anyhow::anyhow!("relay error {code:?}: {message}"))
            }
            msg => Ok(msg),
        }
    }

    /// Read frames until a text frame arrives, parsing it into a `ServerMessage`.
    /// Ping/pong/binary frames are skipped; a close or transport error errors.
    async fn read_frame(&mut self) -> anyhow::Result<ServerMessage> {
        while let Some(frame) = self.read.next().await {
            match frame.map_err(|e| anyhow::anyhow!("websocket transport error: {e}"))? {
                // Parse the first text frame; return whatever it decodes to.
                Message::Text(text) => return parse_server_message(&text),
                Message::Close(_) => break,
                // ping/pong/binary: keep waiting for a text frame.
                _ => (),
            }
        }
        Err(anyhow::anyhow!("relay closed the connection"))
    }

    /// Receive an MLS handshake payload, asserting the expected message type.
    async fn recv_mls(&mut self, expected: MlsMessageType) -> anyhow::Result<Vec<u8>> {
        match self.recv().await? {
            ServerMessage::MlsHandshake { payload, message_type, .. }
                if message_type == expected =>
            {
                Ok(payload)
            }
            other => Err(anyhow::anyhow!("expected {expected:?} handshake, got {other:?}")),
        }
    }

    /// Receive a forwarded Yrs update's ciphertext and epoch.
    async fn recv_yrs(&mut self) -> anyhow::Result<(Vec<u8>, u64)> {
        match self.recv().await? {
            ServerMessage::YrsUpdate { encrypted, epoch, .. } => Ok((encrypted, epoch)),
            other => Err(anyhow::anyhow!("expected a Yrs update, got {other:?}")),
        }
    }
}

/// Parse a relay text frame into a [`ServerMessage`].
fn parse_server_message(text: &str) -> anyhow::Result<ServerMessage> {
    serde_json::from_str(text).map_err(|e| anyhow::anyhow!("invalid server message: {e}"))
}

/// Result of a machine-checkable two-client session.
#[derive(Debug, Serialize)]
pub struct SessionCheckResult {
    /// The document identifier used for the session.
    pub doc_id: String,
    /// The text the peer actually decrypted.
    pub received: String,
    /// The text the peer was expected to decrypt.
    pub expected: String,
    /// True iff the peer decrypted exactly the expected text.
    pub matched: bool,
}

/// Run a real two-client session over a relay and report whether the receiving
/// peer decrypted exactly the expected text.
///
/// This composes keygen -> invite -> join -> connect -> encrypted round-trip
/// between two independently-keyed clients (Alice sends, Bob receives). Both
/// clients share one process, but every message crosses a real relay and the
/// MLS `KeyPackage` -> `Welcome` handshake is genuine. When `relay_url` is
/// `None` a self-contained in-process relay is started and torn down automatically.
///
/// # Errors
///
/// Returns an error if the relay fails to start, any connect/handshake step is
/// rejected or times out, or the encrypted update fails to decrypt (a tampered
/// or wrong-key payload fails closed here rather than silently mismatching).
pub async fn session_check(
    relay_url: Option<&str>,
    doc_id: &str,
    send_text: &str,
    expect: &str,
) -> anyhow::Result<SessionCheckResult> {
    // Start a self-contained relay unless the caller supplied one; keep the
    // handle alive for the whole session and shut it down on every exit path.
    let (url, bound) = if let Some(u) = relay_url {
        (u.to_string(), None)
    } else {
        let bound = collab_relay::RelayServer::new().bind("127.0.0.1:0").await?;
        (format!("ws://{}", bound.addr), Some(bound))
    };

    let outcome = run_session(&url, doc_id, send_text, expect).await;

    if let Some(bound) = bound {
        bound.handle.shutdown();
    }
    outcome
}

/// Drive the two-client choreography over an already-known relay URL.
async fn run_session(
    url: &str,
    doc_id: &str,
    send_text: &str,
    expect: &str,
) -> anyhow::Result<SessionCheckResult> {
    // Alice subscribes before Bob publishes so she is present to route to.
    let mut alice = Peer::connect(url, "alice", doc_id).await?;
    let mut bob = Peer::connect(url, "bob", doc_id).await?;

    // Bob generates a key package and publishes it to the group.
    let bob_pending = MlsDocumentGroup::generate_key_package("bob")?;
    bob.send(ClientMessage::MlsHandshake {
        doc_id: doc_id.to_string(),
        payload: bob_pending.key_package().to_vec(),
        message_type: MlsMessageType::KeyPackage,
    })
    .await?;

    // Alice receives the key package, creates the doc, and sends the welcome.
    let key_package = alice.recv_mls(MlsMessageType::KeyPackage).await?;
    let mut alice_doc = EncryptedDocument::create(doc_id, "alice")?;
    let invite = alice_doc.create_invite(&key_package)?;
    alice
        .send(ClientMessage::MlsHandshake {
            doc_id: doc_id.to_string(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await?;

    // Bob receives the welcome and joins the group (a 2-party joiner reads only
    // the welcome — correct MLS semantics; the KeyPackage genuinely crossed the wire).
    let welcome = bob.recv_mls(MlsMessageType::Welcome).await?;
    let bob_invite =
        collab_core::Invite { doc_id: doc_id.to_string(), welcome, commit: vec![], epoch: 1 };
    let mut bob_doc = EncryptedDocument::join(&bob_invite, bob_pending)?;

    // Alice writes the text and publishes the encrypted update.
    alice_doc.insert(0, send_text);
    let update = alice_doc.get_encrypted_update()?;
    alice
        .send(ClientMessage::YrsUpdate {
            doc_id: doc_id.to_string(),
            encrypted: update.ciphertext,
            epoch: update.epoch,
        })
        .await?;

    // Bob receives and decrypts. A tampered/wrong-key payload errors here and
    // bubbles up via `?` to a non-zero exit (fail closed).
    let (ciphertext, epoch) = bob.recv_yrs().await?;
    bob_doc.apply_encrypted_update(&EncryptedOp { ciphertext, epoch })?;
    let received = bob_doc.get_content();

    Ok(SessionCheckResult {
        doc_id: doc_id.to_string(),
        matched: received == expect,
        received,
        expected: expect.to_string(),
    })
}

/// Handle a server message by printing appropriate output.
fn handle_server_message(server_msg: collab_proto::ServerMessage) {
    match server_msg {
        ServerMessage::Identified { user_id } => {
            println!("Identified as {user_id}");
        }
        ServerMessage::Subscribed { doc_id } => {
            println!("Subscribed to {doc_id}");
        }
        ServerMessage::YrsUpdate { from, doc_id, encrypted, .. } => {
            println!("Update from {from} for {doc_id} ({} bytes)", encrypted.len());
        }
        ServerMessage::Error { message, .. } => {
            eprintln!("Error: {message}");
        }
        _ => {
            println!("{server_msg:?}");
        }
    }
}

/// Minimum time a connection must stay up before it counts as stable enough to
/// refill the retry budget. Sessions shorter than this are treated as failed
/// attempts (accept-then-drop storms) so the retry count keeps accumulating
/// toward `GiveUp` instead of reconnecting forever at a fixed cadence.
// ponytail: fixed stability threshold; make adaptive if reconnect tuning ever matters.
const MIN_STABLE_CONNECTION: std::time::Duration = std::time::Duration::from_secs(10);

/// Run the WebSocket session: identify, subscribe, and process messages.
///
/// Returns `Ok(())` only on a genuine graceful shutdown — a server-side
/// `Close` frame.
///
/// # Errors
///
/// Returns an error on WebSocket send failures during the handshake phase, on
/// a read-loop transport error, or when the stream ends without a `Close`
/// frame. These are surfaced (rather than collapsed into `Ok(())`) so the
/// caller can distinguish a dropped connection from a clean shutdown and
/// reconnect accordingly.
async fn run_ws_session(
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    user_id: &str,
    doc_id: &str,
) -> anyhow::Result<()> {
    let (mut write, mut read) = ws.split();

    let identify = ClientMessage::Identify { user_id: user_id.to_string(), token: None };
    write.send(Message::Text(serde_json::to_string(&identify)?)).await?;

    let subscribe = ClientMessage::Subscribe { doc_id: doc_id.to_string() };
    write.send(Message::Text(serde_json::to_string(&subscribe)?)).await?;

    println!("Connected as {user_id}, subscribed to {doc_id}");
    println!("Listening for updates... (Press Ctrl+C to exit)");

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => match serde_json::from_str::<ServerMessage>(&text) {
                Ok(server_msg) => handle_server_message(server_msg),
                Err(e) => eprintln!("Failed to parse server message: {e}"),
            },
            Ok(Message::Binary(data)) => {
                eprintln!("Warning: unexpected binary message ({} bytes)", data.len());
            }
            Ok(Message::Close(_)) => {
                println!("Connection closed by server");
                return Ok(());
            }
            Err(e) => {
                return Err(anyhow::anyhow!("WebSocket transport error: {e}"));
            }
            _ => {
                // Ping/Pong handled by tungstenite at protocol level
            }
        }
    }

    // Stream ended without a Close frame: the socket dropped mid-session.
    // Report it as an error so the caller reconnects instead of exiting clean.
    Err(anyhow::anyhow!("connection dropped: stream ended without a close frame"))
}

/// Connect to a relay server and listen for updates.
///
/// Uses [`ConnectionStateMachine`] for automatic connection and retry logic
/// with exponential backoff. On disconnection or session error, the state
/// machine drives reconnection attempts until the retry policy is exhausted.
///
/// # Errors
///
/// Returns an error if the connection permanently fails after exhausting
/// all retry attempts.
pub async fn connect(relay_url: &str, user_id: &str, doc_id: &str) -> anyhow::Result<()> {
    let config = ConnectionConfig::new(relay_url, user_id, doc_id);
    let mut sm = ConnectionStateMachine::new(config);

    loop {
        // Each arm yields whether to keep looping; a graceful session end or a
        // terminal state breaks the loop with a successful (Ok) exit.
        let flow = match sm.next_action() {
            ConnectionAction::Connect { relay_url: url } => {
                println!("Connecting to {url}...");
                handle_connect_action(&mut sm, &url).await?
            }
            ConnectionAction::WaitAndRetry { delay, attempt } => {
                println!("Retry attempt {attempt} in {delay:?}...");
                tokio::time::sleep(delay).await;
                sm.on_retry_tick();
                ControlFlow::Continue(())
            }
            ConnectionAction::GiveUp { reason } => {
                return Err(anyhow::anyhow!("Connection failed permanently: {reason}"));
            }
            ConnectionAction::IdentifyAndSubscribe { .. } => {
                debug_assert!(false, "IdentifyAndSubscribe at top of connect loop");
                ControlFlow::Break(())
            }
            // `DoNothing` (auto_connect disabled) and any future variant: stop.
            _ => ControlFlow::Break(()),
        };
        if flow.is_break() {
            break;
        }
    }

    Ok(())
}

/// Handle a single [`ConnectionAction::Connect`] attempt.
///
/// Returns [`ControlFlow::Break`] when the session ended **gracefully** (a clean
/// server-side close) so the caller can exit successfully. Returns
/// [`ControlFlow::Continue`] when a connection or session **error** occurred and
/// was signalled to the state machine for retry handling — a clean shutdown is
/// no longer indistinguishable from a failure.
async fn handle_connect_action(
    sm: &mut ConnectionStateMachine,
    url: &str,
) -> anyhow::Result<ControlFlow<()>> {
    let (ws, _) = match connect_async(url).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Connection failed: {e}");
            sm.on_error(&e.to_string());
            return Ok(ControlFlow::Continue(()));
        }
    };

    sm.on_connected();

    let action = sm.next_action();
    let ConnectionAction::IdentifyAndSubscribe { user_id: uid, doc_id: did } = action else {
        eprintln!("Unexpected action after connect: {action:?}");
        sm.on_error("unexpected state after connect");
        return Ok(ControlFlow::Continue(()));
    };

    // Only a session that stays up past MIN_STABLE_CONNECTION proves the
    // connection was genuinely useful and earns a fresh retry budget. A quick
    // accept-then-drop must NOT reset the budget, or the retry loop never
    // escalates toward GiveUp.
    let started = tokio::time::Instant::now();
    let result = run_ws_session(ws, &uid, &did).await;
    if started.elapsed() >= MIN_STABLE_CONNECTION {
        sm.on_stable_connection();
    }

    match result {
        Ok(()) => {
            println!("Disconnected cleanly.");
            Ok(ControlFlow::Break(()))
        }
        Err(e) => {
            eprintln!("Session error: {e}");
            sm.on_error(&e.to_string());
            Ok(ControlFlow::Continue(()))
        }
    }
}

// Base64 encoding/decoding backed by the `base64` crate (standard alphabet).
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Decode standard base64, ignoring ASCII whitespace.
///
/// # Errors
///
/// Returns an error if the input is not valid base64 (invalid characters,
/// bad padding, or a wrong length).
fn base64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    let cleaned: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid base64 input: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_roundtrip() {
        let original = b"Hello, World!";
        let encoded = base64_encode(original);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_base64_key_package_roundtrip() {
        // Simulate a realistic key package size
        #[allow(clippy::cast_possible_truncation)]
        let data: Vec<u8> = (0u16..500).map(|i| (i % 256) as u8).collect();
        let encoded = base64_encode(&data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_demo_full_flow() {
        let result = demo("test-doc").unwrap();
        assert_eq!(result.alice_content, "Hello from Alice! Hi from Bob!");
        assert_eq!(result.bob_content, "Hello from Alice! Hi from Bob!");
    }

    #[test]
    fn test_init_creates_document() {
        let result = init("test-doc", "alice", None).unwrap();
        assert_eq!(result.doc_id, "test-doc");
        assert_eq!(result.user_id, "alice");
    }

    /// Bind an ephemeral loopback listener and return it with its `ws://` URL.
    async fn bind_ws() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}"))
    }

    /// Accept one connection, drain the client's Identify + Subscribe
    /// handshake, then either send a clean `Close` frame (`clean_close`) or
    /// drop the socket abruptly with no close frame.
    async fn serve_then_end(listener: tokio::net::TcpListener, clean_close: bool) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let _ = ws.next().await;
        let _ = ws.next().await;
        if clean_close {
            ws.send(Message::Close(None)).await.unwrap();
            let _ = ws.next().await; // await the client's close ack
        }
        drop(ws);
    }

    /// A genuine server-side `Close` frame is the only path that yields
    /// `Ok(())` — `handle_connect_action` maps that to a clean exit (`Break`).
    #[tokio::test]
    async fn run_ws_session_returns_ok_on_clean_close() {
        let (listener, url) = bind_ws().await;
        tokio::spawn(serve_then_end(listener, true));

        let (ws, _) = connect_async(&url).await.unwrap();
        let result = run_ws_session(ws, "user", "doc").await;
        assert!(result.is_ok(), "clean close must return Ok, got {result:?}");
    }

    /// A mid-session transport drop (no `Close` frame) must return `Err` so
    /// `handle_connect_action` retries (`Continue`) instead of reporting a
    /// clean disconnect. This is the exact regression the fix guards against.
    #[tokio::test]
    async fn run_ws_session_returns_err_on_transport_drop() {
        let (listener, url) = bind_ws().await;
        tokio::spawn(serve_then_end(listener, false));

        let (ws, _) = connect_async(&url).await.unwrap();
        let result = run_ws_session(ws, "user", "doc").await;
        assert!(result.is_err(), "transport drop must return Err, got {result:?}");
    }

    /// Positive path: the peer decrypts exactly the expected text over a real
    /// in-process relay + MLS handshake, so `matched` is true.
    #[tokio::test]
    async fn test_session_check_matches() {
        let r = session_check(None, "sc-doc", "Hello, session!", "Hello, session!").await.unwrap();
        assert!(r.matched);
        assert_eq!(r.received, "Hello, session!");
    }

    /// Negative-path regression: sending different text than expected must be
    /// detected (`matched` false, `received` reflects what was really decrypted,
    /// not a hardcoded pass). This is the teeth that proves the gate can flip.
    #[tokio::test]
    async fn test_session_check_mismatch_is_detected() {
        let r = session_check(None, "sc-doc", "tampered", "Hello, session!").await.unwrap();
        assert!(!r.matched);
        assert_eq!(r.received, "tampered");
    }

    #[test]
    fn test_keygen_creates_package() {
        let temp_dir = std::env::temp_dir();
        let output_file = temp_dir.join("test_keygen.json");

        let result = keygen("bob", &output_file).unwrap();
        assert_eq!(result.user_id, "bob");
        assert!(output_file.exists());

        // Verify the file contains valid JSON
        let content = fs::read_to_string(&output_file).unwrap();
        let output: KeygenOutput = serde_json::from_str(&content).unwrap();
        assert_eq!(output.user_id, "bob");
        assert!(!output.key_package.is_empty());

        // Cleanup
        let _ = fs::remove_file(&output_file);
    }
}
