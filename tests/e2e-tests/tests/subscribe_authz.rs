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
//! THEN the relay REJECTS each attempt with `Unauthorized` (the replay fails
//! `UserIdMismatch`: the relay binds the presenting connection's LOCAL identified
//! uid, not `cap.user_id`), Eve is NOT added to the subscriber set (she receives
//! no subsequent `YrsUpdate`), AND the two real members DO subscribe
//! successfully (each with a capability naming its own identity) and round-trip
//! an encrypted update.
//!
//! NOTE ON BOOTSTRAP: the MLS-handshake-over-relay flow (`KeyPackage` ->
//! `Welcome`) is intentionally NOT run through this authz-enabled relay — it
//! cannot be. A joiner must Subscribe to *receive* the `Welcome`, but can only
//! mint a capability *after* joining, so an always-on authz relay would deadlock
//! the join. That is
//! exactly why the production relay binary keeps authz OFF for the un-gated
//! bootstrap subscribe (see `full_flow.rs`). Here membership is established
//! in-process (Alice creates + invites Bob → both real epoch-1 members sharing
//! the exporter secret, as in `collab-core`'s `two_member_group`), and the #29
//! authorization gate is exercised over the real wire.
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
use collab_proto::{ClientMessage, DocumentId, ErrorCode, ServerMessage};
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

    // --- Negative (a): Eve subscribes with NO capability → Unauthorized ------
    eve.send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: None }).await.unwrap();
    assert!(
        matches!(
            eve.recv().await.unwrap(),
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ),
        "a non-member with no capability must be rejected Unauthorized"
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

    // Eve was never added to the subscriber set (both subscribes were rejected),
    // so the fanned-out update never reaches her.
    let eve_saw = eve.try_recv(Duration::from_secs(2)).await.unwrap();
    assert!(eve_saw.is_none(), "a rejected non-member must receive no YrsUpdate; got {eve_saw:?}");
}
