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
//! The second test covers the OTHER half of the same gate: what happens to those
//! capabilities once the group rekeys, and the anchor rotation that must
//! accompany it. See `test_anchor_rotation_unwedges_a_rekeyed_document`.
//!
//! The third takes the same rotation machinery down the REMOVAL path — where #31
//! (member removal) meets #72 (anchor rotation): rotating the anchor after a
//! removal is what makes the removal mean something at the relay. See
//! `removal_rotation_revokes_the_removed_members_capability`.
//!
//! TAGGING — no test here is `#[ignore]`d, deliberately. All self-host their
//! authz relay via `TestServer`, so none needs Docker. They still run under the
//! e2e gate (`cargo xtask e2e`), because `--include-ignored` is a superset: it
//! runs untagged tests too. Tagging them would only hide them from a plain
//! `cargo test --workspace` — coverage subtracted for nothing.
//!
//! The `#[ignore]`s in `full_flow.rs` and `fail_closed.rs` are the real thing:
//! those hardcode `ws://localhost:8080/ws` and do need the Docker relay.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collab_core::{AnchorRotation, EncryptedDocument, Invite, MlsDocumentGroup};
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
        &Invite {
            doc_id: doc_id.to_string(),
            welcome: invite.welcome,
            commit: vec![],
            epoch: 1,
            rotation: None,
        },
        bob_pending,
    )
    .unwrap();
    assert_eq!(alice_doc.epoch(), 1, "owner must be at epoch 1 after the add-commit");
    assert_eq!(bob_doc.epoch(), 1, "joiner must be at epoch 1");
    (alice_doc, bob_doc)
}

/// Three real members of one group, all settled at epoch 2 sharing the exporter
/// secret — Alice (owner), Bob and Carol. Extends [`two_real_members`] with a
/// second add-commit that Bob must also process to stay in step.
fn three_real_members(doc_id: &str) -> (EncryptedDocument, EncryptedDocument, EncryptedDocument) {
    let (mut alice_doc, mut bob_doc) = two_real_members(doc_id);

    let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
    let invite = alice_doc.create_invite(carol_pending.key_package()).unwrap();
    // Bob is an EXISTING member: he reaches epoch 2 by processing the add-commit,
    // not by the Welcome. Without this he stays at epoch 1 and mints stale caps.
    bob_doc.process_commit(&invite.commit).unwrap();
    let carol_doc = EncryptedDocument::join(
        &Invite {
            doc_id: doc_id.to_string(),
            welcome: invite.welcome,
            commit: vec![],
            epoch: 2,
            rotation: None,
        },
        carol_pending,
    )
    .unwrap();

    assert_eq!(alice_doc.epoch(), 2, "the owner must be at epoch 2 after the second add");
    assert_eq!(bob_doc.epoch(), 2, "the existing member must follow the add-commit to epoch 2");
    assert_eq!(carol_doc.epoch(), 2, "the second joiner enters at epoch 2");
    (alice_doc, bob_doc, carol_doc)
}

/// Subscribe `client` for `doc_id` with a capability `doc` mints for `user_id`,
/// and assert the relay accepted it.
async fn subscribe_ok(
    client: &mut TestClient,
    doc: &EncryptedDocument,
    user_id: &str,
    doc_id: &DocumentId,
    context: &str,
) {
    let capability = doc.mint_subscribe_capability(user_id, doc_id, now_unix(), TTL_SECS).unwrap();
    client
        .send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: Some(capability) })
        .await
        .unwrap();
    let reply = client.recv().await.unwrap();
    assert!(matches!(reply, ServerMessage::Subscribed { .. }), "{context}: got {reply:?}");
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
// DELIBERATELY NOT `#[ignore]`d — this is the default-run test defending the #72
// mapping end to end: a capability-less subscribe grants the handshake and no
// content. Tagging it would hide that defence from `cargo test --workspace` and
// buy nothing (see the module-level TAGGING note).
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
        // Joiner-side reconstruction from the Welcome bytes off the wire: no
        // commit and no rotation, because Bob holds no outgoing-epoch key to
        // sign one with. Only the owner's `create_invite` emits a rotation.
        &Invite {
            doc_id: doc_id.clone(),
            welcome: welcome_payload,
            commit: vec![],
            epoch: alice_doc.epoch(),
            rotation: None,
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

/// An outsider's own group on the same `doc_id`, advanced to epoch 2 so it emits
/// a rotation that is internally consistent — valid self-proof, right document,
/// right target epoch — but signed by a key that is NOT the relay's stored anchor.
fn foreign_rotation_to_epoch_2(doc_id: &str) -> AnchorRotation {
    let mut eve_doc = foreign_group_at_epoch_1(doc_id);
    let eve_device = MlsDocumentGroup::generate_key_package("eve-device-2").unwrap();
    let invite = eve_doc.create_invite(eve_device.key_package()).unwrap();
    assert_eq!(eve_doc.epoch(), 2, "Eve's group must reach the epoch it is trying to rotate to");
    invite.rotation.expect("create_invite must emit an anchor rotation")
}

/// Assert the relay rejected a `RegisterDocKey` at the ROTATION check specifically.
/// A bad self-proof is also `Unauthorized`, so the code alone would pass for the
/// wrong reason — the message is the positive reading of which gate fired.
async fn assert_rotation_rejected(client: &mut TestClient, context: &str) {
    match client.recv().await.unwrap() {
        ServerMessage::Error { code: ErrorCode::Unauthorized, message } => assert!(
            message.contains("rotation"),
            "{context}: expected the rotation-continuity check to reject it, got: {message}"
        ),
        other => panic!("{context}: expected Unauthorized, got {other:?}"),
    }
}

/// The wedge this makes escapable (issue #29 follow-up): once the group rekeys,
/// members mint capabilities at the new epoch while the relay's anchor is still
/// the old one, so every capability fails — and only a holder of the OUTGOING
/// epoch's key can authorize the rotation that fixes it. That key exists solely
/// inside the commit call, so the proof is emitted there.
///
/// GIVEN a relay with subscribe authorization ENABLED and an anchor registered at
/// epoch 1, WHEN a membership change advances the group to epoch 2, THEN a real
/// member's epoch-2 capability is REJECTED until the anchor is rotated; an
/// outsider's internally-consistent rotation and a rotation proof minted for a
/// DIFFERENT document are both REJECTED; and the rotation emitted by the commit
/// itself is ACCEPTED, after which epoch-2 capabilities work and content flows.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_anchor_rotation_unwedges_a_rekeyed_document() {
    let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
    let url = server.url().to_owned();
    let doc_id: DocumentId = "anchor-rotation-doc".to_string();
    let other_doc: DocumentId = "anchor-rotation-other-doc".to_string();
    let secret = "members only: still readable after the rekey";

    let (mut alice_doc, _bob_doc) = two_real_members(&doc_id);
    let mut alice = TestClient::connect_as(&url, "alice").await.unwrap();

    // --- The owner registers the epoch-1 anchor (TOFU) for BOTH documents ----
    // `other_doc` carries the same key under its own self-proof and exists only
    // so a rotation proof can be presented against a document it was not minted
    // for (negative (b) below).
    for target in [&doc_id, &other_doc] {
        alice
            .send(&ClientMessage::RegisterDocKey {
                doc_id: target.clone(),
                epoch: alice_doc.epoch(),
                public_key: alice_doc.subscribe_verifying_key().unwrap().to_vec(),
                proof: alice_doc.sign_doc_key_proof(target).unwrap(),
                rotation_proof: Vec::new(),
            })
            .await
            .unwrap();
    }

    // Baseline: an epoch-1 capability is accepted. Awaiting `Subscribed` also
    // flushes the two silent registrations above — one connection, FIFO handling.
    let now = now_unix();
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
        "the epoch-1 capability must be accepted against the epoch-1 anchor"
    );

    // --- A membership change advances the group to epoch 2 -------------------
    let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
    let invite = alice_doc.create_invite(carol_pending.key_package()).unwrap();
    let rotation = invite.rotation.expect("create_invite must emit the anchor rotation");
    let mut carol_doc = EncryptedDocument::join(
        &Invite {
            doc_id: doc_id.clone(),
            welcome: invite.welcome,
            commit: vec![],
            epoch: 2,
            rotation: None,
        },
        carol_pending,
    )
    .unwrap();
    assert_eq!(alice_doc.epoch(), 2, "the add-commit must advance the owner to epoch 2");
    assert_eq!(carol_doc.epoch(), 2, "the new member joins at epoch 2");
    assert_eq!(rotation.epoch, 2, "the rotation must name the epoch just created");

    // --- THE WEDGE: epoch-2 capabilities fail while the anchor is still at 1 --
    let mut carol = TestClient::connect_as(&url, "carol").await.unwrap();
    carol
        .send(&ClientMessage::Subscribe {
            doc_id: doc_id.clone(),
            capability: Some(
                carol_doc.mint_subscribe_capability("carol", &doc_id, now, TTL_SECS).unwrap(),
            ),
        })
        .await
        .unwrap();
    assert!(
        matches!(
            carol.recv().await.unwrap(),
            ServerMessage::Error { code: ErrorCode::Unauthorized, .. }
        ),
        "a real member's epoch-2 capability must fail against the stale epoch-1 anchor"
    );

    // --- Negative (a): an outsider's internally-consistent rotation ----------
    // Eve's self-proof is valid for her own key and the rotation names the right
    // document and epoch — only the CONTINUITY signature is under the wrong key.
    let mut eve = TestClient::connect_as(&url, "eve").await.unwrap();
    let eve_rotation = foreign_rotation_to_epoch_2(&doc_id);
    eve.send(&ClientMessage::RegisterDocKey {
        doc_id: doc_id.clone(),
        epoch: eve_rotation.epoch,
        public_key: eve_rotation.public_key.to_vec(),
        proof: eve_rotation.proof,
        rotation_proof: eve_rotation.rotation_proof,
    })
    .await
    .unwrap();
    assert_rotation_rejected(&mut eve, "a non-member's rotation of someone else's anchor").await;

    // --- Negative (b): the right proof, presented for the WRONG document -----
    // Same signer, same target epoch, same new key, valid self-proof for
    // `other_doc` — but the continuity proof binds `doc_id`, so it must fail.
    alice
        .send(&ClientMessage::RegisterDocKey {
            doc_id: other_doc.clone(),
            epoch: rotation.epoch,
            public_key: rotation.public_key.to_vec(),
            proof: alice_doc.sign_doc_key_proof(&other_doc).unwrap(),
            rotation_proof: rotation.rotation_proof.clone(),
        })
        .await
        .unwrap();
    assert_rotation_rejected(&mut alice, "a rotation proof minted for another document").await;

    // --- Positive: the rotation the commit itself emitted --------------------
    alice
        .send(&ClientMessage::RegisterDocKey {
            doc_id: doc_id.clone(),
            epoch: rotation.epoch,
            public_key: rotation.public_key.to_vec(),
            proof: rotation.proof,
            rotation_proof: rotation.rotation_proof,
        })
        .await
        .unwrap();

    // Success is silent, so the owner's epoch-2 subscribe is what proves the
    // anchor moved (and flushes the registration on the way).
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
        "after the rotation the anchor must accept an epoch-2 capability"
    );

    carol
        .send(&ClientMessage::Subscribe {
            doc_id: doc_id.clone(),
            capability: Some(
                carol_doc.mint_subscribe_capability("carol", &doc_id, now, TTL_SECS).unwrap(),
            ),
        })
        .await
        .unwrap();
    assert!(
        matches!(carol.recv().await.unwrap(), ServerMessage::Subscribed { .. }),
        "the member wedged out a moment ago must now subscribe with the same epoch-2 capability"
    );

    // --- Content flows again over the rotated anchor -------------------------
    alice_doc.insert(0, secret);
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &update).await.unwrap();

    let carol_op = carol.recv_update().await.unwrap();
    carol_doc.apply_encrypted_update(&carol_op).unwrap();
    assert_eq!(carol_doc.get_content(), secret, "the re-authorized member must decrypt the update");
}

/// #31 (member removal) meeting #72 (anchor rotation) over the real wire: the
/// rotation is what gives a removal teeth at the relay. Removing a member rekeys
/// the group, and registering the resulting rotation moves the anchor past the
/// epoch the removed member's key can reach — so her capability stops verifying
/// while the remaining members simply re-mint at the new epoch.
///
/// GIVEN a relay with subscribe authorization ENABLED, an anchor registered at
/// epoch 2, and three real members (Alice the owner, Bob, Carol) whose epoch-2
/// capabilities all subscribe successfully, WHEN Alice removes Carol and
/// registers the rotation the removal commit emitted, THEN Carol's epoch-2
/// capability is REJECTED `Unauthorized` at the capability gate, Carol receives
/// no subsequent `YrsUpdate`, Carol's own `process_commit` yields no rotation she
/// could use to move the anchor back, and Bob — who re-mints at epoch 3 —
/// subscribes and decrypts the content off the wire.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn removal_rotation_revokes_the_removed_members_capability() {
    let server = TestServer::start_with(RelayServer::new().with_subscribe_authz(true)).await;
    let url = server.url().to_owned();
    let doc_id: DocumentId = "removal-rotation-doc".to_string();
    let secret = "members only: readable after carol is gone";

    let (mut alice_doc, mut bob_doc, mut carol_doc) = three_real_members(&doc_id);
    let mut alice = TestClient::connect_as(&url, "alice").await.unwrap();
    let mut bob = TestClient::connect_as(&url, "bob").await.unwrap();
    let mut carol = TestClient::connect_as(&url, "carol").await.unwrap();

    // --- The owner registers the epoch-2 anchor (TOFU) -----------------------
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

    // --- Baseline: all three epoch-2 capabilities are ACCEPTED ---------------
    // Alice's Subscribed also flushes the silent registration above (one
    // connection, FIFO), so Bob and Carol act against a live anchor.
    subscribe_ok(&mut alice, &alice_doc, "alice", &doc_id, "the owner's epoch-2 capability").await;
    subscribe_ok(&mut bob, &bob_doc, "bob", &doc_id, "a member's epoch-2 capability").await;
    let carol_cap =
        carol_doc.mint_subscribe_capability("carol", &doc_id, now_unix(), TTL_SECS).unwrap();
    carol
        .send(&ClientMessage::Subscribe {
            doc_id: doc_id.clone(),
            capability: Some(carol_cap.clone()),
        })
        .await
        .unwrap();
    assert!(
        matches!(carol.recv().await.unwrap(), ServerMessage::Subscribed { .. }),
        "the member about to be removed must hold a WORKING epoch-2 capability first"
    );

    // LOAD-BEARING, do not simplify away: Carol must drop her live subscription
    // for the rest of this test to mean anything. Rotating the anchor does not
    // tear down existing subscriptions — the documented residual at
    // `docs/security.md` "Live subscriptions are checked at subscribe time only"
    // (#31 scope). Without this Unsubscribe, Carol stays in the fan-out set and
    // the test would prove nothing about her CAPABILITY, which is what it exists
    // to prove.
    carol.send(&ClientMessage::Unsubscribe { doc_id: doc_id.clone() }).await.unwrap();
    assert!(
        matches!(carol.recv().await.unwrap(), ServerMessage::Unsubscribed { .. }),
        "carol must be out of the fan-out set before the removal"
    );

    // --- The removal advances the group to epoch 3 ---------------------------
    let (commit, rotation) = alice_doc.remove_member("carol").unwrap();
    bob_doc.process_commit(&commit).unwrap();
    // Carol is evicted by her own removal: no new epoch key, so no rotation she
    // could present to drag the anchor back to a key she still holds.
    assert!(
        carol_doc.process_commit(&commit).is_err(),
        "the removed member must not obtain a rotation from her own removal"
    );
    assert_eq!(alice_doc.epoch(), 3, "the removal commit must advance the owner to epoch 3");
    assert_eq!(bob_doc.epoch(), 3, "the remaining member must follow the removal to epoch 3");
    assert_eq!(rotation.epoch, 3, "the rotation must name the epoch the removal created");
    // The positive reading of why she cannot re-mint: she has no epoch-3 key at all.
    assert!(
        carol_doc.mint_subscribe_capability("carol", &doc_id, now_unix(), TTL_SECS).is_err(),
        "the removed member must not be able to mint at the epoch her removal created"
    );

    // --- The owner registers the removal's rotation --------------------------
    alice
        .send(&ClientMessage::RegisterDocKey {
            doc_id: doc_id.clone(),
            epoch: rotation.epoch,
            public_key: rotation.public_key.to_vec(),
            proof: rotation.proof,
            rotation_proof: rotation.rotation_proof,
        })
        .await
        .unwrap();
    // Registration is silent; the owner's epoch-3 subscribe flushes it and proves
    // the anchor actually moved.
    subscribe_ok(&mut alice, &alice_doc, "alice", &doc_id, "the owner's epoch-3 capability").await;

    // --- THE REVOCATION: Carol's capability no longer verifies ---------------
    carol
        .send(&ClientMessage::Subscribe { doc_id: doc_id.clone(), capability: Some(carol_cap) })
        .await
        .unwrap();
    match carol.recv().await.unwrap() {
        ServerMessage::Error { code: ErrorCode::Unauthorized, message } => assert!(
            message.contains("capability"),
            "the removed member must fail the CAPABILITY gate, got: {message}"
        ),
        other => panic!("the removed member's stale capability must be rejected, got {other:?}"),
    }

    // --- A remaining member simply re-mints at the new epoch ------------------
    subscribe_ok(&mut bob, &bob_doc, "bob", &doc_id, "a remaining member's epoch-3 capability")
        .await;

    // --- Content flows to the remaining member, never to the removed one ------
    alice_doc.insert(0, secret);
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send_update(&doc_id, &update).await.unwrap();

    let bob_op = bob.recv_update().await.unwrap();
    bob_doc.apply_encrypted_update(&bob_op).unwrap();
    assert_eq!(bob_doc.get_content(), secret, "the remaining member must decrypt the update");

    let carol_saw = carol.try_recv(Duration::from_secs(2)).await.unwrap();
    assert!(carol_saw.is_none(), "the removed member must receive no YrsUpdate; got {carol_saw:?}");
}
