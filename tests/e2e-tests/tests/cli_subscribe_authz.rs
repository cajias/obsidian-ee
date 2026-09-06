//! `collab-cli` over a subscribe-authorization-enabled relay (issue #72).
//!
//! `subscribe_authz.rs` proves the relay's half of the gate with hand-driven
//! `TestClient`s. This file proves the CLIENT half: that `collab-cli`'s own
//! session choreography registers the document anchor, mints a capability for
//! each member, and re-presents it — so content actually flows when the relay
//! gates `YrsUpdate` fan-out on a capability.
//!
//! Before the fix both of the CLI's subscribe sites hardcoded `capability:
//! None`, so the session completed the MLS handshake and then timed out waiting
//! for content that the relay was correctly withholding.
//!
//! NOT `#[ignore]`d, matching `subscribe_authz.rs`: every test here self-hosts
//! its relay via `TestServer`, so none needs Docker.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collab_core::{EncryptedDocument, MlsDocumentGroup};
use collab_proto::{ClientMessage, DocumentId, MlsMessageType, ServerMessage};
use collab_relay::RelayServer;
use e2e_tests::helpers::{setup_two_user_group, TestClient, TestServer};

/// Capability lifetime for the test (matches the design's 300s default).
const TTL_SECS: u64 = 300;

/// Whole seconds since the Unix epoch (for capability minting).
fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Start a relay with subscribe authorization ON — the configuration the CLI
/// could not previously work against.
async fn authz_relay() -> TestServer {
    TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await
}

/// Register `doc`'s current-epoch anchor over `client` (TOFU: no rotation proof).
async fn register_anchor(client: &mut TestClient, doc: &EncryptedDocument, doc_id: &DocumentId) {
    client
        .send(&ClientMessage::RegisterDocKey {
            doc_id: doc_id.clone(),
            epoch: doc.epoch(),
            public_key: doc.subscribe_verifying_key().unwrap().to_vec(),
            proof: doc.sign_doc_key_proof(doc_id).unwrap(),
            rotation_proof: Vec::new(),
        })
        .await
        .unwrap();
}

/// Subscribe `client` with a capability `doc` mints for `user_id`, asserting
/// the relay accepted it.
async fn subscribe_with_capability(
    client: &mut TestClient,
    doc: &EncryptedDocument,
    user_id: &str,
    doc_id: &DocumentId,
) {
    let capability = doc.mint_subscribe_capability(user_id, doc_id, now_unix(), TTL_SECS).unwrap();
    client
        .send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: Some(capability) })
        .await
        .unwrap();
    assert!(matches!(client.recv().await.unwrap(), ServerMessage::Subscribed { .. }));
}

/// THE test this change exists for: the CLI's own session flow must complete
/// and content must arrive against a relay with subscribe authorization ON.
///
/// RED before the fix: `session_check` returns `Err("timed out waiting for a
/// relay message")` because Bob only ever sent a capability-less `Subscribe`,
/// so the relay withheld the `YrsUpdate` it correctly refused to fan out.
#[tokio::test]
async fn cli_session_flows_content_over_an_authz_relay() {
    let server = authz_relay().await;
    let text = "members only: the CLI mints its own capability";

    let result =
        collab_cli::commands::session_check(Some(server.url()), "cli-authz-doc", text, text)
            .await
            .expect("the CLI session must complete against a subscribe-authz relay");

    assert!(
        result.matched,
        "the CLI peer must decrypt the relayed text, got {:?}",
        result.received
    );
}

/// The CLI must not weaken the relay's TOFU + rotation-continuity anchor rules:
/// against a document some other group already anchored, the CLI's own
/// registration is refused (it never forges a continuity proof — it always
/// sends an empty `rotation_proof`), its capability then fails to verify, and
/// the session fails closed instead of hijacking the anchor.
#[tokio::test]
async fn cli_cannot_hijack_an_already_anchored_document() {
    let server = authz_relay().await;
    let doc_id: DocumentId = "cli-preanchored-doc".to_string();

    // An outsider's independent group anchors the doc first (TOFU wins).
    let squatter = EncryptedDocument::create(&doc_id, "squatter").unwrap();
    let mut eve = TestClient::connect_as(server.url(), "squatter").await.unwrap();
    register_anchor(&mut eve, &squatter, &doc_id).await;
    subscribe_with_capability(&mut eve, &squatter, "squatter", &doc_id).await;

    let err = collab_cli::commands::session_check(Some(server.url()), &doc_id, "secret", "secret")
        .await
        .expect_err("the CLI must fail closed against an anchor it does not own");
    // Assert WHICH gate fired: a bad self-proof is also `Unauthorized`, so the
    // code alone would pass for the wrong reason. Rotation continuity is the
    // rule this change must not weaken.
    assert!(
        err.to_string().contains("rotation continuity"),
        "the hijack must be refused at the rotation-continuity check, got: {err}"
    );
}

/// Why both CLI subscribe sites must re-present: a bare `Subscribe` from an
/// already-authorized member DOWNGRADES it back to handshake-only, so a
/// reconnect that forgets the capability silently stops receiving content.
///
/// A characterization test of relay behaviour (green before and after the CLI
/// fix) — it is the reason the fix mints at every subscribe, not just the first.
#[tokio::test]
async fn a_bare_resubscribe_downgrades_an_authorized_member() {
    let server = authz_relay().await;
    let doc_id: DocumentId = "cli-downgrade-doc".to_string();

    let mut alice = TestClient::connect_as(server.url(), "alice").await.unwrap();
    let mut bob = TestClient::connect_as(server.url(), "bob").await.unwrap();
    // The bootstrap itself is capability-less — that part is not gated.
    let (mut alice_doc, mut bob_doc) =
        setup_two_user_group(&mut alice, &mut bob, &doc_id).await.unwrap();
    register_anchor(&mut alice, &alice_doc, &doc_id).await;
    subscribe_with_capability(&mut alice, &alice_doc, "alice", &doc_id).await;
    subscribe_with_capability(&mut bob, &bob_doc, "bob", &doc_id).await;

    // Authorized: content reaches Bob.
    alice_doc.insert(0, "before the reconnect");
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &update).await.unwrap();
    bob_doc.apply_encrypted_update(&bob.recv_update().await.unwrap()).unwrap();
    assert!(bob_doc.get_content().contains("before the reconnect"));

    // What a naive reconnect sends — and what it costs.
    bob.send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: None }).await.unwrap();
    assert!(matches!(bob.recv().await.unwrap(), ServerMessage::Subscribed { .. }));
    alice_doc.insert(0, "after the bare re-subscribe");
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &update).await.unwrap();
    let saw = bob.try_recv(Duration::from_secs(2)).await.unwrap();
    assert!(saw.is_none(), "a bare re-subscribe must downgrade to handshake-only; got {saw:?}");

    // Re-presenting the capability restores content — the CLI's fix.
    subscribe_with_capability(&mut bob, &bob_doc, "bob", &doc_id).await;
    alice_doc.insert(0, "after re-presenting");
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &update).await.unwrap();
    bob_doc.apply_encrypted_update(&bob.recv_update().await.unwrap()).unwrap();
    assert!(bob_doc.get_content().contains("after re-presenting"));
}

/// The handshake must still cross a gated relay: a capability-less `Subscribe`
/// is the join bootstrap, so the CLI's `Welcome` fan-out cannot be gated away.
/// Guards against "fixing" the deadlock by demanding a capability up front.
#[tokio::test]
async fn a_capability_less_subscriber_still_receives_the_welcome() {
    let server = authz_relay().await;
    let doc_id: DocumentId = "cli-welcome-doc".to_string();

    let mut alice = TestClient::connect_as(server.url(), "alice").await.unwrap();
    let mut bob = TestClient::connect_as(server.url(), "bob").await.unwrap();
    for client in [&mut alice, &mut bob] {
        client
            .send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: None })
            .await
            .unwrap();
        assert!(matches!(client.recv().await.unwrap(), ServerMessage::Subscribed { .. }));
    }

    let mut alice_doc = EncryptedDocument::create(&doc_id, "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();
    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: invite.welcome,
            message_type: MlsMessageType::Welcome,
        })
        .await
        .unwrap();

    assert!(
        matches!(
            bob.recv().await.unwrap(),
            ServerMessage::MlsHandshake { message_type: MlsMessageType::Welcome, .. }
        ),
        "a capability-less subscriber must still receive the Welcome, or the join deadlocks"
    );
}
