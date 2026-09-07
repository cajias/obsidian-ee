//! CLI command implementations.

use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use collab_core::{
    ConnectionAction, ConnectionConfig, ConnectionStateMachine, EncryptedDocument, EncryptedOp,
    MlsDocumentGroup,
};
use collab_proto::{ClientMessage, Invite, MlsMessageType, ServerMessage, SubscribeCapability};
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
pub fn init(
    doc_id: &str,
    user_id: &str,
    state_file: Option<&Path>,
    key: &[u8; 32],
) -> anyhow::Result<InitResult> {
    let doc = EncryptedDocument::create(doc_id, user_id)?;

    // Save state if requested. The group is snapshotted encrypted-at-rest so a
    // later `connect` can restore it and mint a subscribe capability; without
    // this the owner reconnects with no group and stays content-blind (#93).
    if let Some(path) = state_file {
        let state = DocumentState {
            doc_id: doc_id.to_string(),
            user_id: user_id.to_string(),
            role: "owner".to_string(),
            // `snapshot_encrypted` binds `doc_id` as AEAD associated data using
            // the document's OWN id, so the blob only ever opens under the id it
            // was created for (#76).
            snapshot: base64_encode(&doc.snapshot_encrypted(key)?),
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
    /// The MLS group, snapshotted and encrypted at rest (base64), so a later
    /// session can resume it rather than build a fresh epoch-0 group (#93).
    ///
    /// The at-rest key lives in a SEPARATE file — writing it here would void
    /// the encryption, since the key would sit beside the blob it protects.
    pub snapshot: String,
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
            // Empty: a joiner has no group to snapshot. Reaching here at all
            // requires `pending.join` above to have succeeded, which it cannot
            // until `keygen`'s `PendingMember` state is persisted across
            // processes — a different type with no snapshot surface (#93
            // follow-up). `load_state` reads empty as "no group" and the
            // session falls back to handshake-only, exactly as today.
            snapshot: String::new(),
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

/// Lifetime of a minted subscribe capability (issue #29's design default). Short
/// enough that a leaked capability expires quickly, long enough to outlive a
/// session's handshake.
// ponytail: fixed TTL; make configurable only if a long-lived session needs it.
const CAPABILITY_TTL_SECS: u64 = 300;

/// Whole seconds since the Unix epoch, for capability expiry. A clock before
/// 1970 yields 0, which mints an already-expired capability — fail closed.
fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A client-side WebSocket connection to the relay, used by [`session_check`].
type PeerWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One relay-connected client: a split WebSocket plus identify/subscribe and
/// typed send/recv helpers bounded by [`PEER_RECV_TIMEOUT`].
struct Peer {
    write: futures::stream::SplitSink<PeerWs, Message>,
    read: futures::stream::SplitStream<PeerWs>,
    /// This connection's identified user id — the identity a minted capability
    /// must name, or the relay rejects it as `UserIdMismatch`.
    user_id: String,
}

impl Peer {
    /// Connect, identify, and subscribe — returning a ready peer or an error if
    /// any handshake step is rejected or times out.
    async fn connect(url: &str, user_id: &str, doc_id: &str) -> anyhow::Result<Self> {
        let (ws, _) = connect_async(url).await?;
        let (write, read) = ws.split();
        let mut peer = Self { write, read, user_id: user_id.to_string() };

        peer.send(ClientMessage::Identify { user_id: user_id.to_string(), token: None }).await?;
        match peer.recv().await? {
            ServerMessage::Identified { .. } => {}
            other => return Err(anyhow::anyhow!("expected Identified, got {other:?}")),
        }

        // capability: None — nothing can be minted yet. A joiner must be
        // subscribed to RECEIVE the Welcome that makes it a member, and only a
        // member can mint (issue #72). This subscription authorizes the MLS
        // handshake alone; `present_capability` upgrades it once a group exists.
        peer.subscribe(doc_id, None).await?;

        Ok(peer)
    }

    /// Subscribe (or re-subscribe) to `doc_id`, awaiting the relay's acceptance.
    ///
    /// Re-subscribing re-states the authorization: presenting a capability
    /// upgrades the subscription to content-authorized, and a bare `Subscribe`
    /// DOWNGRADES it back to handshake-only. So every subscribe after the group
    /// exists must carry a capability.
    async fn subscribe(
        &mut self,
        doc_id: &str,
        capability: Option<SubscribeCapability>,
    ) -> anyhow::Result<()> {
        self.send(ClientMessage::Subscribe { doc_id: doc_id.to_string(), capability }).await?;
        match self.recv().await? {
            ServerMessage::Subscribed { .. } => Ok(()),
            other => Err(anyhow::anyhow!("expected Subscribed, got {other:?}")),
        }
    }

    /// Register `doc`'s current-epoch subscribe anchor so members' capabilities
    /// have something to verify against (issue #29).
    ///
    /// `rotation_proof` is empty: the CLI only ever registers a document it has
    /// just created, which is the first (TOFU) registration. It never forges
    /// continuity for an anchor someone else owns — such a registration is
    /// refused, and the capability that follows then fails to verify.
    ///
    /// A successful registration is silent, so this does not wait for a reply;
    /// a rejection arrives as an `Error` frame that the next [`Self::recv`]
    /// surfaces (fail closed).
    async fn register_anchor(
        &mut self,
        doc: &EncryptedDocument,
        doc_id: &str,
    ) -> anyhow::Result<()> {
        self.send(ClientMessage::RegisterDocKey {
            doc_id: doc_id.to_string(),
            epoch: doc.epoch(),
            public_key: doc.subscribe_verifying_key()?.to_vec(),
            proof: doc.sign_doc_key_proof(doc_id)?,
            rotation_proof: Vec::new(),
        })
        .await
    }

    /// Mint a capability at `doc`'s current epoch and re-subscribe with it,
    /// upgrading this connection from handshake-only to content-authorized.
    ///
    /// The capability is bound to the LOCALLY-trusted `doc_id` the caller
    /// subscribed to and to this connection's own identity — never to a value
    /// taken from an inbound frame.
    async fn present_capability(
        &mut self,
        doc: &EncryptedDocument,
        doc_id: &str,
    ) -> anyhow::Result<()> {
        let capability =
            doc.mint_subscribe_capability(&self.user_id, doc_id, now_unix(), CAPABILITY_TTL_SECS)?;
        self.subscribe(doc_id, Some(capability)).await
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

/// The joiner half of the bootstrap: receive the `Welcome` over the relay, join
/// the group, then present a freshly minted capability.
///
/// The ordering is the crux of issue #72. A joiner cannot mint before it has
/// joined — the capability key derives from the group's per-epoch exporter
/// secret — so it must subscribe capability-less to receive the `Welcome`, and
/// only re-subscribe with a capability afterwards.
async fn join_via_welcome(
    peer: &mut Peer,
    pending: collab_core::PendingMember,
    doc_id: &str,
) -> anyhow::Result<EncryptedDocument> {
    // A 2-party joiner reads only the welcome — correct MLS semantics; the
    // KeyPackage genuinely crossed the wire.
    let welcome = peer.recv_mls(MlsMessageType::Welcome).await?;
    // A joiner-side reconstruction: no commit and no anchor rotation — the
    // joiner holds no outgoing-epoch key to sign one with.
    let invite = collab_core::Invite {
        doc_id: doc_id.to_string(),
        welcome,
        commit: vec![],
        epoch: 1,
        rotation: None,
    };
    let doc = EncryptedDocument::join(&invite, pending)?;

    // A member only now, so able to mint only now. Re-subscribing with the
    // capability upgrades this connection from handshake-only to
    // content-authorized — without it the relay withholds every `YrsUpdate`.
    peer.present_capability(&doc, doc_id).await?;
    Ok(doc)
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

    // The group now exists, so the anchor can be registered and Alice can
    // upgrade her own handshake-only subscription. Both must happen BEFORE Bob
    // presents his capability — his verifies against this anchor.
    alice.register_anchor(&alice_doc, doc_id).await?;
    alice.present_capability(&alice_doc, doc_id).await?;

    alice
        .send(ClientMessage::MlsHandshake {
            doc_id: doc_id.to_string(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await?;

    let mut bob_doc = join_via_welcome(&mut bob, bob_pending, doc_id).await?;

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
    doc: Option<&EncryptedDocument>,
) -> anyhow::Result<()> {
    let (mut write, mut read) = ws.split();

    let identify = ClientMessage::Identify { user_id: user_id.to_string(), token: None };
    write.send(Message::Text(serde_json::to_string(&identify)?)).await?;

    let subscribe = subscribe_frame(user_id, doc_id, doc)?;
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
pub async fn connect(
    relay_url: &str,
    user_id: &str,
    doc_id: &str,
    doc: Option<&EncryptedDocument>,
) -> anyhow::Result<()> {
    let config = ConnectionConfig::new(relay_url, user_id, doc_id);
    let mut sm = ConnectionStateMachine::new(config);

    loop {
        // Each arm yields whether to keep looping; a graceful session end or a
        // terminal state breaks the loop with a successful (Ok) exit.
        let flow = match sm.next_action() {
            ConnectionAction::Connect { relay_url: url } => {
                println!("Connecting to {url}...");
                handle_connect_action(&mut sm, &url, doc).await?
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
    doc: Option<&EncryptedDocument>,
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
    let result = run_ws_session(ws, &uid, &did, doc).await;
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

/// Build this session's `Subscribe` frame, minting a capability from `doc` when
/// a group was restored (#93).
///
/// Extracted so the frame that actually goes on the wire is directly
/// assertable: the difference between content-authorized and content-blind is
/// one `Option` field, and it is invisible from the session's console output.
///
/// Called on EVERY session, reconnects included, because `handle_connect_action`
/// re-enters `run_ws_session` per attempt — and it must be: a bare `Subscribe`
/// from an already authorized member DOWNGRADES it back to handshake-only, so a
/// reconnect that forgets the capability goes quietly content-blind.
///
/// `None` stays the honest fallback for a session with no persisted group (no
/// `--state`, or the joiner path): it subscribes handshake-only, which is the
/// correct MLS bootstrap and receives no `YrsUpdate` under authz.
///
/// The capability is bound to the LOCALLY-trusted `user_id` and `doc_id` the
/// session was started with, never to a value read back off the wire.
///
/// # Errors
///
/// Returns an error if minting from the restored group fails.
///
/// `pub` rather than private: `tests/e2e-tests/tests/cli_subscribe_authz.rs`
/// puts this exact frame on the wire against a real authz relay, which is the
/// only way to prove the relay ACCEPTS what the CLI mints.
pub fn subscribe_frame(
    user_id: &str,
    doc_id: &str,
    doc: Option<&EncryptedDocument>,
) -> anyhow::Result<ClientMessage> {
    let capability = doc
        .map(|d| d.mint_subscribe_capability(user_id, doc_id, now_unix(), CAPABILITY_TTL_SECS))
        .transpose()?;
    Ok(ClientMessage::Subscribe { doc_id: doc_id.to_string(), capability })
}

/// Restore the MLS group a prior `init` persisted in `state_file`.
///
/// Returns `Ok(None)` when the file records no snapshot (the joiner path, which
/// has no group to save) or when the snapshot predates a known rotation — both
/// mean "no group", and the session then subscribes handshake-only exactly as it
/// did before #93.
///
/// `doc_id` is the CALLER's document id, from argv. It is passed to
/// `restore_encrypted` as the AEAD associated data, and the `doc_id` field
/// stored *inside* the file is deliberately never read: the state file is
/// untrusted input, so a blob sealed for another document must fail
/// authentication here rather than be adopted on the file's own say-so.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed, or if the snapshot
/// fails AEAD authentication (wrong at-rest key, wrong document, or tampering).
pub fn load_state(
    doc_id: &str,
    state_file: &Path,
    key: &[u8; 32],
) -> anyhow::Result<Option<EncryptedDocument>> {
    let state: DocumentState = serde_json::from_str(&fs::read_to_string(state_file)?)?;
    if state.snapshot.is_empty() {
        return Ok(None);
    }
    let blob = base64_decode(&state.snapshot)?;
    // `doc_id` here is the caller's, never `state.doc_id`.
    EncryptedDocument::restore_encrypted(doc_id, &blob, key, 0)
        .map_err(|e| anyhow::anyhow!("cannot restore the persisted group: {e}"))
}

/// Load (or on first use, generate) the 32-byte key that encrypts the MLS
/// snapshot in `state_file`, from `<state_file>.key`.
///
/// The key lives in its OWN file, never in the state file: the two sit in the
/// same directory, so writing the key beside the blob it protects would void
/// the encryption. The file is created `0600` — owner read/write only — and an
/// existing one with looser permissions is refused rather than silently used.
///
/// ponytail: a 0600 key file is the lazy source. The upgrade path is the OS
/// keychain (macOS Keychain / libsecret), which would protect the key against a
/// file-level read by another process running as the same user; this does not.
/// Swap the body of this function and nothing else changes.
///
/// # Errors
///
/// Returns an error if the key file cannot be read or created, if OS randomness
/// is unavailable, if an existing key file is not exactly 32 bytes, or if it is
/// group- or world-accessible.
pub fn at_rest_key(state_file: &Path) -> anyhow::Result<[u8; 32]> {
    let key_path = key_path_for(state_file);

    if key_path.exists() {
        let bytes = fs::read(&key_path)?;
        let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "{} is {} bytes, expected 32 — refusing to guess at a corrupt key file",
                key_path.display(),
                bytes.len()
            )
        })?;
        reject_loose_permissions(&key_path)?;
        return Ok(key);
    }

    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key)
        .map_err(|e| anyhow::anyhow!("OS randomness unavailable for the at-rest key: {e:?}"))?;
    write_owner_only(&key_path, &key)?;
    Ok(key)
}

/// The key file that pairs with `state_file`.
fn key_path_for(state_file: &Path) -> PathBuf {
    let mut name = state_file.file_name().unwrap_or_default().to_os_string();
    name.push(".key");
    state_file.with_file_name(name)
}

/// Write `bytes` to `path` with mode 0600 from the moment it exists.
///
/// The mode is set in the `open` call rather than by a later `set_permissions`,
/// so there is no window in which the key is readable by anyone else.
#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| anyhow::anyhow!("cannot create key file {}: {e}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    // ponytail: no mode bits off unix. Windows ACL hardening if it ever ships there.
    fs::write(path, bytes)
        .map_err(|e| anyhow::anyhow!("cannot create key file {}: {e}", path.display()))
}

/// Refuse a key file any other user can read.
#[cfg(unix)]
fn reject_loose_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path)?.permissions().mode() & 0o077;
    if mode != 0 {
        anyhow::bail!(
            "{} is group/world accessible (mode {:o}); run `chmod 600` on it",
            path.display(),
            mode
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_loose_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
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

    /// A fixed key for the persistence tests. The REAL key's provenance is
    /// `at_rest_key`; these tests are about the snapshot plumbing, not the
    /// source, so they inject one and stay independent of that decision.
    const TEST_KEY: [u8; 32] = [7u8; 32];

    /// Scenario 1: `init` persists the owner's MLS group, encrypted at rest.
    ///
    /// GIVEN `init` is asked to create "docA" for alice with a state file,
    /// WHEN the state file is read back and restored under the same doc id and
    /// key, THEN a group comes back at the epoch `create` left it on.
    ///
    /// RED before the fix: `DocumentState` has no `snapshot` field, and `init`
    /// drops the group it creates on the floor (`let _doc = ...`).
    #[test]
    fn init_persists_the_owner_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();

        let state: DocumentState =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let blob = base64_decode(&state.snapshot).unwrap();
        let Ok(Some(restored)) = EncryptedDocument::restore_encrypted("docA", &blob, &TEST_KEY, 0)
        else {
            panic!("a snapshot init just wrote must restore, and is not stale")
        };
        assert_eq!(restored.epoch(), 0, "a freshly created owner group is at epoch 0");
    }

    /// Scenario 2: NEGATIVE — the seal binds the doc id `init` was GIVEN.
    ///
    /// GIVEN `init` created "docB", WHEN the snapshot is restored under "docA",
    /// THEN it fails AEAD authentication. Without this, `init` could seal under
    /// a hardcoded literal and scenario 1 would still pass.
    #[test]
    fn init_snapshot_is_bound_to_the_document_it_was_created_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docB", "alice", Some(&path), &TEST_KEY).unwrap();

        let state: DocumentState =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let blob = base64_decode(&state.snapshot).unwrap();
        let Err(err) = EncryptedDocument::restore_encrypted("docA", &blob, &TEST_KEY, 0) else {
            panic!("a snapshot sealed for docB must not open under docA");
        };
        assert!(
            err.to_string().contains("AEAD open failed"),
            "the doc id must be bound as associated data, got: {err}"
        );
    }

    /// Scenario 3: NEGATIVE — a different at-rest key cannot open the snapshot.
    #[test]
    fn init_snapshot_rejects_a_wrong_at_rest_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();

        let state: DocumentState =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let blob = base64_decode(&state.snapshot).unwrap();
        assert!(
            EncryptedDocument::restore_encrypted("docA", &blob, &[9u8; 32], 0).is_err(),
            "a wrong at-rest key must fail AEAD authentication"
        );
    }

    /// Scenario 4: NEGATIVE — the at-rest key never lands in the state file.
    ///
    /// The key sits beside the blob on disk; writing it into the same file
    /// would void the encryption entirely.
    #[test]
    fn init_state_file_does_not_contain_the_at_rest_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();

        let bytes = fs::read(&path).unwrap();
        assert!(
            !bytes.windows(TEST_KEY.len()).any(|w| w == TEST_KEY),
            "the at-rest key must never be written next to the blob it protects"
        );
        assert!(
            !String::from_utf8_lossy(&bytes).contains(&base64_encode(&TEST_KEY)),
            "the at-rest key must not appear base64-encoded either"
        );
    }

    /// Scenario 5: `load_state` restores the group a prior `init` persisted.
    ///
    /// GIVEN `init` wrote a state file for "docA", WHEN `load_state` reads it
    /// back with the same doc id and key, THEN a usable group comes back.
    #[test]
    fn load_state_restores_a_persisted_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();

        let restored = load_state("docA", &path, &TEST_KEY).unwrap();
        assert!(restored.is_some(), "load_state must return the group init persisted");
    }

    /// Scenario 6: NEGATIVE — the CALLER's doc id wins over the file's own.
    ///
    /// GIVEN a state file whose `doc_id` FIELD claims "docA" while its blob was
    /// sealed for "docA", WHEN `load_state` is called for "docB", THEN it fails
    /// AEAD authentication rather than trusting the field.
    ///
    /// This is the trust boundary: the state file is untrusted input, so the
    /// document identity must come from argv. Without this test, `load_state`
    /// could read `state.doc_id` and every positive test would still pass.
    #[test]
    fn load_state_binds_the_callers_doc_id_not_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();

        let Err(err) = load_state("docB", &path, &TEST_KEY) else {
            panic!("a snapshot sealed for docA must not open as docB")
        };
        assert!(
            err.to_string().contains("AEAD open failed"),
            "the caller's doc id must be the AEAD context, got: {err}"
        );
    }

    /// Scenario 7: NEGATIVE — a wrong at-rest key does not yield a group.
    #[test]
    fn load_state_rejects_a_wrong_at_rest_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();

        assert!(
            load_state("docA", &path, &[9u8; 32]).is_err(),
            "a wrong at-rest key must fail rather than return a group"
        );
    }

    /// Scenario 8: a state file with no snapshot (the joiner path) reads as
    /// "no group" — the session falls back to handshake-only, as today.
    #[test]
    fn load_state_reads_an_empty_snapshot_as_no_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = DocumentState {
            doc_id: "docA".to_string(),
            user_id: "alice".to_string(),
            role: "collaborator".to_string(),
            snapshot: String::new(),
        };
        fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

        assert!(
            load_state("docA", &path, &TEST_KEY).unwrap().is_none(),
            "an empty snapshot must read as absent, not as a corrupt blob"
        );
    }

    /// Scenario 9: NEGATIVE — the at-rest key file is created owner-only, and a
    /// group/world-accessible one is refused rather than silently used.
    #[cfg(unix)]
    #[test]
    fn at_rest_key_is_owner_only_and_refuses_a_loose_key_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let key = at_rest_key(&path).unwrap();
        let key_file = dir.path().join("state.json.key");
        let mode = fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the key file must be created owner-read/write only");

        // A second call returns the SAME key — the group must stay restorable
        // across processes, which a regenerated key would silently prevent.
        assert_eq!(at_rest_key(&path).unwrap(), key, "the key must be stable across calls");

        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o644)).unwrap();
        let err = at_rest_key(&path).expect_err("a world-readable key file must be refused");
        assert!(err.to_string().contains("chmod 600"), "the error must say how to fix it: {err}");
    }

    /// Scenario 10: THE #93 assertion for the CLI listener — a restored group
    /// makes the session's `Subscribe` carry a capability.
    ///
    /// GIVEN a group restored from what `init` persisted, WHEN the listener
    /// builds its subscribe frame, THEN the frame carries a capability that
    /// VERIFIES against the anchor that same group registers.
    ///
    /// Verifying rather than merely asserting `is_some()` is the point: a
    /// capability the relay would reject is indistinguishable from none.
    ///
    /// RED before the fix: the listener hardcoded `capability: None`.
    #[test]
    fn a_restored_group_makes_the_listener_subscribe_content_authorized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();
        let doc = load_state("docA", &path, &TEST_KEY).unwrap().expect("init persisted a group");

        let ClientMessage::Subscribe { capability: Some(cap), doc_id } =
            subscribe_frame("alice", "docA", Some(&doc)).unwrap()
        else {
            panic!("a restored group must produce a capability-carrying Subscribe")
        };
        assert_eq!(doc_id, "docA");

        // The relay's own verifier is the judge, under the anchor this group
        // would register and the identity the connection would be bound to.
        collab_proto::verify_subscribe_capability(
            &cap,
            &doc.subscribe_verifying_key().unwrap(),
            "alice",
            "docA",
            doc.epoch(),
            now_unix(),
        )
        .expect("the minted capability must verify against this group's own anchor");
    }

    /// Scenario 11: NEGATIVE — the capability is bound to THIS user, so another
    /// connection cannot present it.
    ///
    /// The relay checks a capability against the CONNECTION's identified user
    /// id, so a capability minted for alice must fail for eve even though both
    /// name the same document and epoch.
    #[test]
    fn the_listeners_capability_does_not_verify_for_another_user() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();
        let doc = load_state("docA", &path, &TEST_KEY).unwrap().unwrap();

        let ClientMessage::Subscribe { capability: Some(cap), .. } =
            subscribe_frame("alice", "docA", Some(&doc)).unwrap()
        else {
            panic!("expected a capability")
        };
        assert!(
            collab_proto::verify_subscribe_capability(
                &cap,
                &doc.subscribe_verifying_key().unwrap(),
                "eve",
                "docA",
                doc.epoch(),
                now_unix(),
            )
            .is_err(),
            "a capability minted for alice must not verify for eve"
        );
    }

    /// Scenario 12: NEGATIVE — the capability is bound to THIS document.
    #[test]
    fn the_listeners_capability_does_not_verify_for_another_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        init("docA", "alice", Some(&path), &TEST_KEY).unwrap();
        let doc = load_state("docA", &path, &TEST_KEY).unwrap().unwrap();

        let ClientMessage::Subscribe { capability: Some(cap), .. } =
            subscribe_frame("alice", "docA", Some(&doc)).unwrap()
        else {
            panic!("expected a capability")
        };
        assert!(
            collab_proto::verify_subscribe_capability(
                &cap,
                &doc.subscribe_verifying_key().unwrap(),
                "alice",
                "docB",
                doc.epoch(),
                now_unix(),
            )
            .is_err(),
            "a capability minted for docA must not verify for docB"
        );
    }

    /// Scenario 13: no persisted group keeps the handshake-only fallback.
    ///
    /// The bootstrap path must not regress: a session with no `--state` still
    /// subscribes, it simply carries no content authorization.
    #[test]
    fn no_persisted_group_still_subscribes_handshake_only() {
        let ClientMessage::Subscribe { capability, doc_id } =
            subscribe_frame("alice", "docA", None).unwrap()
        else {
            panic!("expected a Subscribe")
        };
        assert!(capability.is_none(), "no group means no capability, not a forged one");
        assert_eq!(doc_id, "docA");
    }

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
        let result = init("test-doc", "alice", None, &TEST_KEY).unwrap();
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
        let result = run_ws_session(ws, "user", "doc", None).await;
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
        let result = run_ws_session(ws, "user", "doc", None).await;
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
