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

use std::time::Duration;

use collab_core::{EncryptedDocument, MlsDocumentGroup};
use collab_proto::{ClientMessage, DocumentId, MlsMessageType, ServerMessage};
use collab_relay::RelayServer;
use e2e_tests::helpers::{
    assert_no_content, register_anchor, setup_two_user_group, subscribe_with_capability,
    TestClient, TestServer,
};

/// Start a relay with subscribe authorization ON — the configuration the CLI
/// could not previously work against.
async fn authz_relay() -> TestServer {
    TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await
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
    register_anchor(&mut eve, &squatter, &doc_id).await.unwrap();
    subscribe_with_capability(&mut eve, &squatter, "squatter", &doc_id).await.unwrap();

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
    register_anchor(&mut alice, &alice_doc, &doc_id).await.unwrap();
    subscribe_with_capability(&mut alice, &alice_doc, "alice", &doc_id).await.unwrap();
    subscribe_with_capability(&mut bob, &bob_doc, "bob", &doc_id).await.unwrap();

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
    subscribe_with_capability(&mut bob, &bob_doc, "bob", &doc_id).await.unwrap();
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

/// THE #93 assertion for `collab-cli connect`: a listener that restored its
/// persisted group is content-authorized on a real subscribe-authz relay, and
/// one without a group is not.
///
/// GIVEN a document whose anchor a previous session registered, AND a group
/// restored from the state file `init` wrote,
/// WHEN the listener presents the `Subscribe` frame `subscribe_frame` builds,
/// THEN the relay accepts it and a peer's `YrsUpdate` reaches the listener —
/// while a second listener with no persisted group receives none.
///
/// RED before #93: `run_ws_session` hardcoded `capability: None`, so the relay
/// correctly withheld content and the listener never saw an update.
#[tokio::test]
async fn a_restored_cli_listener_receives_content_over_an_authz_relay() {
    let server = authz_relay().await;
    let doc_id: DocumentId = "cli-restored-listener".to_string();

    // What a previous `collab-cli init --state` left on disk.
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let key = collab_cli::commands::at_rest_key(&state_path).unwrap();
    collab_cli::commands::init(&doc_id, "alice", Some(&state_path), &key).unwrap();
    let restored = collab_cli::commands::load_state(&doc_id, &state_path, &key)
        .unwrap()
        .expect("init persisted a group");

    // The anchor that session registered is still on the relay. A restored
    // client deliberately does NOT re-register: the relay accepts a rotation
    // only at a strictly higher epoch, so a same-epoch re-registration would be
    // refused as "stale or equal epoch".
    let mut anchorer = TestClient::connect_as(server.url(), "alice-prior").await.unwrap();
    register_anchor(&mut anchorer, &restored, &doc_id).await.unwrap();

    // The listener presents exactly what the CLI would put on the wire.
    let mut listener = TestClient::connect_as(server.url(), "alice").await.unwrap();
    listener
        .send(&collab_cli::commands::subscribe_frame("alice", &doc_id, Some(&restored)).unwrap())
        .await
        .unwrap();
    assert!(
        matches!(listener.recv().await.unwrap(), ServerMessage::Subscribed { .. }),
        "the relay must accept the capability the CLI minted from its restored group"
    );

    // A second listener with no persisted group: the honest fallback, and the
    // control that proves the relay is gating rather than fanning out to all.
    let mut blind = TestClient::connect_as(server.url(), "bob").await.unwrap();
    blind
        .send(&collab_cli::commands::subscribe_frame("bob", &doc_id, None).unwrap())
        .await
        .unwrap();
    assert!(matches!(blind.recv().await.unwrap(), ServerMessage::Subscribed { .. }));

    // A peer publishes content.
    let mut peer = TestClient::connect_as(server.url(), "peer").await.unwrap();
    peer.send(&ClientMessage::YrsUpdate {
        doc_id: doc_id.clone(),
        encrypted: b"opaque ciphertext".to_vec(),
        epoch: restored.epoch(),
    })
    .await
    .unwrap();

    let ServerMessage::YrsUpdate { encrypted, .. } = listener.recv().await.unwrap() else {
        panic!("the restored listener must receive the peer's update")
    };
    assert_eq!(encrypted, b"opaque ciphertext");

    // The peer also sends ungated handshake traffic, so the group-less listener
    // has something it MUST receive. Without it its silence would prove nothing:
    // a disconnected or unsubscribed listener is quiet for the same reason a
    // gated one is.
    peer.send(&ClientMessage::MlsHandshake {
        doc_id: doc_id.clone(),
        payload: b"key package".to_vec(),
        message_type: MlsMessageType::KeyPackage,
    })
    .await
    .unwrap();

    // The gate assertion: the group-less listener received the handshake and no
    // content. Checked after the authorized listener already got the update, so
    // the absence is the relay withholding, not the update never happening.
    assert_no_content(&mut blind, Duration::from_millis(500)).await.unwrap();
}
