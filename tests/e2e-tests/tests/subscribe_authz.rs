//! Over-relay subscribe-authorization wire test (issue #29, BDD scenario 5).
//!
//! This is the metric-relevant negative test: it lifts the per-document
//! subscribe-authorization trust boundary over a REAL relay binding (real
//! WebSocket transport), not just the in-process unit tests. It asserts the
//! attacker case is REJECTED — the CLAUDE.md trust-boundary rule made
//! load-bearing over the network.
//!
//! GIVEN two real MLS members (Alice owns the group and registers the doc's
//! subscribe anchor; Bob is a second real member sharing the epoch-1 exporter
//! secret), over a relay with subscribe authorization ENABLED,
//! WHEN a THIRD identified client (Eve) who is NOT in the group tries to
//! Subscribe — first with NO capability, then with a FOREIGN capability minted
//! by her own independent MLS group — AND WHEN Alice's connection tries to
//! replay a VALID capability that member Bob minted for himself,
//! THEN the relay REJECTS every capability-bearing attempt with `Unauthorized`
//! (the replay fails `UserIdMismatch`: the relay binds the presenting
//! connection's LOCAL identified uid, not `cap.user_id`), Eve's capability-less
//! subscribe is accepted for the MLS handshake ONLY so she still receives no
//! `YrsUpdate` (issue #72), AND the two real members DO subscribe successfully
//! (each with a capability naming its own identity) and round-trip an encrypted
//! update.
//!
//! NOTE ON BOOTSTRAP: membership here is established in-process (Alice creates +
//! invites Bob → both real epoch-1 members sharing the exporter secret, as in
//! `collab-core`'s `two_member_group`) so this test can focus on the #29
//! authorization gate over the real wire. The MLS-handshake-over-relay bootstrap
//! under an authz-enabled relay — `KeyPackage` -> `Welcome` -> join -> mint ->
//! re-subscribe -> content — is the separate `test_joiner_bootstraps_...` test
//! below (issue #72). Before #72 that flow deadlocked: a joiner had to Subscribe
//! to *receive* the `Welcome`, but could only mint a capability *after* joining.
//!
//! Runs both under `cargo test --workspace` (self-hosted relay, no Docker) and
//! under `cargo xtask e2e` via `--include-ignored`. It is marked `#[ignore]` to
//! keep it in the wire-test tier counted by the e2e gate.
//!
//! Requires Docker: `docker compose -f docker/docker-compose.yml up -d`
//! (This test self-hosts its authz relay and does not use the Docker relay, but
//! carries the tag so it lands in the `--include-ignored` wire-test tier.)

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collab_core::{EncryptedDocument, Invite, MlsDocumentGroup};
use collab_proto::{ClientMessage, DocumentId, ErrorCode, MlsMessageType, ServerMessage};
use collab_relay::RelayServer;
use e2e_tests::helpers::{TestClient, TestServer};

/// Capability lifetime for the test (matches the design's 300s default).
const TTL_SECS: u64 = 300;

/// Whole seconds since the Unix epoch (for capability minting).
fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Two real members of one group, both settled at epoch 1 sharing the exporter
/// secret — established in-process (see the module note on bootstrap).
fn two_real_members(doc_id: &str) -> (EncryptedDocument, EncryptedDocument) {
    let mut alice_doc = EncryptedDocument::create(doc_id, "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();
    let bob_doc = EncryptedDocument::join(
        &Invite { doc_id: doc_id.to_string(), welcome: invite.welcome, commit: vec![], epoch: 1 },
        bob_pending,
    )
    .unwrap();
    assert_eq!(alice_doc.epoch(), 1, "owner must be at epoch 1 after the add-commit");
    assert_eq!(bob_doc.epoch(), 1, "joiner must be at epoch 1");
    (alice_doc, bob_doc)
}

/// A foreign group (Eve's own), advanced to epoch 1 so she can mint a capability
/// at the anchor's epoch — a correct-epoch but WRONG-KEY capability.
fn foreign_group_at_epoch_1(doc_id: &str) -> EncryptedDocument {
    let mut eve_doc = EncryptedDocument::create(doc_id, "eve").unwrap();
    let eve_device = MlsDocumentGroup::generate_key_package("eve-device").unwrap();
    let _ = eve_doc.create_invite(eve_device.key_package()).unwrap();
    assert_eq!(eve_doc.epoch(), 1, "Eve's independent group must also be at epoch 1");
    eve_doc
}

#[tokio::test]
#[ignore = "Requires Docker: docker compose -f docker/docker-compose.yml up -d"]
#[allow(clippy::too_many_lines)]
async fn test_non_member_subscribe_rejected_over_relay() {
    // Self-hosted relay with subscribe authorization ENABLED.
    let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
    let url = server.url().to_owned();
    let doc_id: DocumentId = "authz-wire-doc".to_string();
    let secret = "members only: the coordinates are 47.6,-122.3";

    // --- Two real members (in-process) + one non-member's foreign group ------
    let (mut alice_doc, mut bob_doc) = two_real_members(&doc_id);
    let eve_doc = foreign_group_at_epoch_1(&doc_id);

    // --- Three clients connect and identify over the real relay --------------
    let mut alice = TestClient::connect_as(&url, "alice").await.unwrap();
    let mut bob = TestClient::connect_as(&url, "bob").await.unwrap();
    let mut eve = TestClient::connect_as(&url, "eve").await.unwrap();

    // --- The owner registers the doc's subscribe anchor (epoch 1) ------------
    // Silent on success. A valid self-proof under the epoch key proves membership.
    alice
        .send(&ClientMessage::RegisterDocKey {
            doc_id: doc_id.clone(),
            epoch: alice_doc.epoch(),
            public_key: alice_doc.subscribe_verifying_key().unwrap().to_vec(),
            proof: alice_doc.sign_doc_key_proof(&doc_id).unwrap(),
            // First (TOFU) registration for this doc: no rotation continuity proof.
            rotation_proof: Vec::new(),
        })
        .await
        .unwrap();

    let now = now_unix();

    // --- Negative (a): Eve subscribes with NO capability → handshake-only ----
    // Since #72 this subscribe is ACCEPTED (it is the join-bootstrap path, which
    // the relay cannot distinguish from Eve's), but it authorizes nothing: the
    // final assertion — Eve receives no `YrsUpdate` — is where the teeth moved.
    eve.send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: None }).await.unwrap();
    assert!(
        matches!(eve.recv().await.unwrap(), ServerMessage::Subscribed { .. }),
        "a capability-less subscribe is accepted for the handshake bootstrap"
    );

    // --- Negative (b): Eve subscribes with a FOREIGN capability → Unauthorized
    // The capability is at the correct epoch (1) but signed by Eve's own group's
    // key, which is not the registered anchor key.
    let foreign_cap = eve_doc.mint_subscribe_capability("eve", &doc_id, now, TTL_SECS).unwrap();
    eve.send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: Some(foreign_cap) })
        .await
        .unwrap();
    assert!(
        matches!(
            eve.recv().await.unwrap(),
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ),
        "a non-member's foreign capability must be rejected Unauthorized"
    );

    // --- Negative (c): same-group replay-as-someone-else -----------------------
    // Bob mints a VALID capability naming himself (signed by the real anchor key),
    // but Alice's connection presents it. The relay binds the presenting
    // connection's identified uid ("alice") as expected_user_id, so a capability
    // naming "bob" is rejected (UserIdMismatch surfaces as Unauthorized). This is
    // the trust-boundary teeth for Finding 4: a bearer token cannot be replayed as
    // another subscriber even inside the same group.
    let bobs_cap = bob_doc.mint_subscribe_capability("bob", &doc_id, now, TTL_SECS).unwrap();
    alice
        .send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: Some(bobs_cap) })
        .await
        .unwrap();
    assert!(
        matches!(
            alice.recv().await.unwrap(),
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ),
        "a capability minted for bob, presented by alice's connection, must be rejected"
    );

    // --- Positive: both real members subscribe with valid capabilities -------
    // Each mints a capability naming its OWN identity (matching how it identified).
    alice
        .send(&ClientMessage::Subscribe {
            doc_id: doc_id.clone(),
            capability: Some(
                alice_doc.mint_subscribe_capability("alice", &doc_id, now, TTL_SECS).unwrap(),
            ),
        })
        .await
        .unwrap();
    assert!(
        matches!(alice.recv().await.unwrap(), ServerMessage::Subscribed { .. }),
        "the owner (anchor key holder) must subscribe successfully"
    );

    bob.send(&ClientMessage::Subscribe {
        doc_id: doc_id.clone(),
        capability: Some(bob_doc.mint_subscribe_capability("bob", &doc_id, now, TTL_SECS).unwrap()),
    })
    .await
    .unwrap();
    assert!(
        matches!(bob.recv().await.unwrap(), ServerMessage::Subscribed { .. }),
        "the second real member (same epoch key) must subscribe successfully"
    );

    // --- Round-trip: Alice edits and sends; Bob decrypts; Eve observes nothing
    alice_doc.insert(0, secret);
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &update).await.unwrap();

    let bob_op = bob.recv_update().await.unwrap();
    bob_doc.apply_encrypted_update(&bob_op).unwrap();
    assert_eq!(
        bob_doc.get_content(),
        secret,
        "the second member must decrypt the update off the wire"
    );

    // Eve is subscribed (handshake-only) but was never content-authorized: her
    // capability-bearing attempt was rejected and her capability-less one granted
    // no content. The fanned-out update never reaches her.
    let eve_saw = eve.try_recv(Duration::from_secs(2)).await.unwrap();
    assert!(
        eve_saw.is_none(),
        "an unauthorized subscriber must receive no YrsUpdate; got {eve_saw:?}"
    );
}

/// The #72 bootstrap: an MLS join completes over an authz-ENABLED relay, and the
/// joiner receives document content only once it presents a capability.
///
/// GIVEN a relay with subscribe authorization enabled and an owner (Alice) whose
/// group does not yet include Bob,
/// WHEN Bob subscribes with NO capability (he cannot mint one — the key derives
/// from the group's per-epoch exporter secret and he is not yet a member),
/// publishes his `KeyPackage` over the wire, and receives Alice's `Welcome` over
/// the relay to join, WHILE Alice registers the doc anchor and sends an encrypted
/// update,
/// THEN Bob's capability-less subscribe SUCCEEDS (before #72 it was rejected and
/// the join deadlocked), he receives the handshake but NOT that first update,
/// AND only after he mints a capability at the anchor epoch and re-subscribes
/// with it does relayed content reach him and decrypt.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_joiner_bootstraps_and_gains_content_after_capability() {
    let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
    let url = server.url().to_owned();
    let doc_id: DocumentId = "authz-bootstrap-doc".to_string();
    let secret = "members only: the rendezvous is at 0300";

    let mut alice = TestClient::connect_as(&url, "alice").await.unwrap();
    let mut bob = TestClient::connect_as(&url, "bob").await.unwrap();

    // --- Both subscribe with NO capability: no anchor exists yet, and Bob is
    // --- not a member, so neither CAN present one. This is the deadlock fix.
    for client in [&mut alice, &mut bob] {
        client
            .send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: None })
            .await
            .unwrap();
        assert!(
            matches!(client.recv().await.unwrap(), ServerMessage::Subscribed { .. }),
            "a pre-membership subscribe must be accepted, or the MLS join deadlocks"
        );
    }

    // --- Bob publishes his KeyPackage over the wire ---------------------------
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    bob.send(&ClientMessage::MlsHandshake {
        doc_id: doc_id.clone(),
        payload: bob_pending.key_package().to_vec(),
        message_type: MlsMessageType::KeyPackage,
    })
    .await
    .unwrap();

    // --- Alice builds the invite from the WIRE bytes only ---------------------
    let mut alice_doc = EncryptedDocument::create(&doc_id, "alice").unwrap();
    let ServerMessage::MlsHandshake {
        payload: bob_key_package,
        message_type: MlsMessageType::KeyPackage,
        ..
    } = alice.recv().await.unwrap()
    else {
        panic!("Alice expected Bob's KeyPackage over the wire")
    };
    let invite = alice_doc.create_invite(&bob_key_package).unwrap();
    assert_eq!(alice_doc.epoch(), 1, "the add-commit advances the owner to epoch 1");

    // --- The Welcome crosses the relay to a handshake-only subscriber ---------
    // This is the fan-out that content gating must NOT block.
    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await
        .unwrap();
    let ServerMessage::MlsHandshake {
        payload: welcome_payload,
        message_type: MlsMessageType::Welcome,
        ..
    } = bob.recv().await.unwrap()
    else {
        panic!("Bob expected the Welcome over the wire")
    };
    let mut bob_doc = EncryptedDocument::join(
        &Invite {
            doc_id: doc_id.clone(),
            welcome: welcome_payload,
            commit: vec![],
            epoch: alice_doc.epoch(),
        },
        bob_pending,
    )
    .unwrap();

    // --- Alice anchors the doc at her current epoch and re-subscribes with a
    // --- capability, upgrading her own subscription to content-authorized.
    let now = now_unix();
    alice
        .send(&ClientMessage::RegisterDocKey {
            doc_id: doc_id.clone(),
            epoch: alice_doc.epoch(),
            public_key: alice_doc.subscribe_verifying_key().unwrap().to_vec(),
            proof: alice_doc.sign_doc_key_proof(&doc_id).unwrap(),
            rotation_proof: Vec::new(),
        })
        .await
        .unwrap();
    alice
        .send(&ClientMessage::Subscribe {
            doc_id: doc_id.clone(),
            capability: Some(
                alice_doc.mint_subscribe_capability("alice", &doc_id, now, TTL_SECS).unwrap(),
            ),
        })
        .await
        .unwrap();
    assert!(matches!(alice.recv().await.unwrap(), ServerMessage::Subscribed { .. }));

    // --- Bob has JOINED but has not presented a capability: no content --------
    alice_doc.insert(0, "pre-capability content");
    let early = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &early).await.unwrap();
    let bob_saw = bob.try_recv(Duration::from_secs(2)).await.unwrap();
    assert!(
        bob_saw.is_none(),
        "a joined-but-handshake-only subscriber must receive no YrsUpdate; got {bob_saw:?}"
    );

    // --- Bob mints a capability at the anchor epoch and re-subscribes ---------
    assert_eq!(bob_doc.epoch(), alice_doc.epoch(), "joiner and owner share the anchored epoch");
    bob.send(&ClientMessage::Subscribe {
        doc_id: doc_id.clone(),
        capability: Some(bob_doc.mint_subscribe_capability("bob", &doc_id, now, TTL_SECS).unwrap()),
    })
    .await
    .unwrap();
    assert!(matches!(bob.recv().await.unwrap(), ServerMessage::Subscribed { .. }));

    // --- Now content reaches him and decrypts --------------------------------
    // `get_encrypted_update` encodes full CRDT state, so the update Bob was
    // denied above is not needed to converge.
    alice_doc.insert(0, secret);
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &update).await.unwrap();
    let bob_op = bob.recv_update().await.unwrap();
    bob_doc.apply_encrypted_update(&bob_op).unwrap();
    assert!(
        bob_doc.get_content().contains(secret),
        "a content-authorized joiner must decrypt relayed content"
    );
}
