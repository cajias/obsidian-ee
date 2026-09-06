//! Over-relay fail-closed wire tests (issue #48).
//!
//! Issue #27 proved fail-closed on a bad key as a Tier-1, *in-process* guard.
//! Issue #48 lifts that negative-path assertion over the REAL relay (Tier-2):
//! a wrong-key client (Eve, in her own independent MLS group) subscribes to the
//! doc, receives Alice's encrypted `YrsUpdate` off the wire, and MUST fail to
//! decrypt it — her document stays empty and she never enters the group.
//!
//! This is the trust-boundary rule from CLAUDE.md made load-bearing over the
//! network: "Every trust boundary ... MUST have a NEGATIVE-path test asserting
//! the attacker case is REJECTED — not only a positive round-trip."
//!
//! Requires Docker: `docker compose -f docker/docker-compose.yml up -d`

use collab_core::{EncryptedDocument, EncryptedOp, Invite, MlsDocumentGroup};
use collab_proto::{ClientMessage, DocumentId, MlsMessageType, ServerMessage};
use e2e_tests::helpers::TestClient;

/// A wrong-key client, subscribed to the same doc through the relay, receives
/// Alice's encrypted update off the wire but cannot decrypt it — proving
/// fail-closed holds over the real relay, not just in-process.
///
/// GIVEN Alice and Bob establish a real MLS group over the relay (`KeyPackage` →
/// Welcome crossing the wire), AND Eve is a third client with her OWN
/// independent MLS group (wrong key) subscribed to the same `doc_id`.
/// WHEN Alice edits and sends an encrypted `YrsUpdate` over the relay (fanned
/// out to BOTH Bob and Eve).
/// THEN Bob decrypts it (positive control — the update really flowed), Eve's
/// decrypt is REJECTED, Eve's document stays empty, and the ciphertext Eve
/// received leaks no plaintext.
///
/// Requires Docker: `docker compose -f docker/docker-compose.yml up -d`
#[tokio::test]
#[ignore = "Requires Docker: docker compose -f docker/docker-compose.yml up -d"]
#[allow(clippy::too_many_lines)]
async fn test_wrong_key_client_observes_nothing_over_relay() {
    let relay_url = "ws://localhost:8080/ws";
    let doc_id: DocumentId = "test-doc-fail-closed".to_string();
    let secret = "TOP SECRET: Launch codes are 12345";

    // --- Three clients connect and subscribe to the same document ---------
    let mut alice = TestClient::connect_as(relay_url, "alice").await.unwrap();
    let mut bob = TestClient::connect_as(relay_url, "bob").await.unwrap();
    let mut eve = TestClient::connect_as(relay_url, "eve").await.unwrap();

    // Subscribe before any handshake is broadcast so nobody misses it.
    alice.subscribe(&doc_id).await.unwrap();
    bob.subscribe(&doc_id).await.unwrap();
    eve.subscribe(&doc_id).await.unwrap();

    // --- Alice creates the group and invites Bob over the relay -----------
    let mut alice_doc = EncryptedDocument::create(&doc_id, "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await
        .unwrap();

    // Bob receives Alice's Welcome off the wire and joins the real group.
    let ServerMessage::MlsHandshake {
        payload: bob_welcome,
        message_type: MlsMessageType::Welcome,
        ..
    } = bob.recv().await.unwrap()
    else {
        panic!("Bob expected an MlsHandshake Welcome message");
    };
    let bob_invite = Invite {
        doc_id: doc_id.clone(),
        welcome: bob_welcome,
        commit: vec![],
        epoch: 1,
        rotation: None,
    };
    let mut bob_doc = EncryptedDocument::join(&bob_invite, bob_pending).unwrap();

    // Eve also receives the Welcome off the wire (fanned out to every other
    // subscriber). It was minted for Bob's KeyPackage, not hers.
    let ServerMessage::MlsHandshake {
        payload: eve_seen_welcome,
        message_type: MlsMessageType::Welcome,
        ..
    } = eve.recv().await.unwrap()
    else {
        panic!("Eve expected to observe the broadcast Welcome");
    };

    // --- "Starts no session": Eve cannot join with a Welcome not hers ------
    // Alice's Welcome encrypts the group secrets to Bob's HPKE key. Eve's own
    // key material can't unseal it, so joining must fail — she never enters
    // the session via the intercepted Welcome.
    let eve_join_pending = MlsDocumentGroup::generate_key_package("eve").unwrap();
    let eve_bad_invite = Invite {
        doc_id: doc_id.clone(),
        welcome: eve_seen_welcome,
        commit: vec![],
        epoch: 1,
        rotation: None,
    };
    let eve_join_result = EncryptedDocument::join(&eve_bad_invite, eve_join_pending);
    assert!(
        eve_join_result.is_err(),
        "Eve joined with a Welcome minted for Bob's KeyPackage — she must be rejected \
         and never enter the session"
    );

    // Eve instead stands up her OWN independent MLS group (wrong key) for the
    // same doc_id, mirroring the in-process wrong-key test.
    let mut eve_doc = EncryptedDocument::create(&doc_id, "eve").unwrap();
    let eve_device = MlsDocumentGroup::generate_key_package("eve-device2").unwrap();
    let _eve_invite = eve_doc.create_invite(eve_device.key_package()).unwrap();

    // --- Alice edits and sends the encrypted update over the relay --------
    alice_doc.insert(0, secret);
    let alice_update = alice_doc.get_encrypted_update().unwrap();
    alice
        .send(&ClientMessage::YrsUpdate {
            doc_id: doc_id.clone(),
            encrypted: alice_update.ciphertext.clone(),
            epoch: alice_update.epoch,
        })
        .await
        .unwrap();

    // --- Positive control: Bob (legit member) decrypts off the wire -------
    let ServerMessage::YrsUpdate { encrypted: bob_ct, epoch: bob_epoch, .. } =
        bob.recv().await.unwrap()
    else {
        panic!("Bob expected a YrsUpdate off the wire");
    };
    let bob_op = EncryptedOp { ciphertext: bob_ct, epoch: bob_epoch };
    bob_doc.apply_encrypted_update(&bob_op).unwrap();
    assert_eq!(
        bob_doc.get_content(),
        secret,
        "Bob (legit MLS member) must decrypt the update that flowed over the relay"
    );

    // --- Negative path: Eve receives the SAME ciphertext, cannot decrypt --
    let ServerMessage::YrsUpdate { encrypted: eve_ct, epoch: eve_epoch, .. } =
        eve.recv().await.unwrap()
    else {
        panic!("Eve expected to receive the broadcast YrsUpdate off the wire");
    };
    let eve_op = EncryptedOp { ciphertext: eve_ct.clone(), epoch: eve_epoch };

    // Eve's failure comes from her having the WRONG KEY (independent MLS
    // group), NOT from any doctored input — she applies the exact bytes the
    // relay delivered.
    let result = eve_doc.apply_encrypted_update(&eve_op);
    assert!(
        result.is_err(),
        "Wrong-key client decrypted an update over the relay — fail-closed is BROKEN. \
         AEAD auth-tag verification failed to reject a ciphertext from a foreign MLS group."
    );

    // Eve's document stays EMPTY — no silent garbage, no plaintext observed.
    assert_eq!(
        eve_doc.get_content(),
        "",
        "Eve's document must remain empty after failed decryption"
    );

    // The ciphertext Eve received off the wire leaks no plaintext.
    let secret_bytes = secret.as_bytes();
    let plaintext_leaked = eve_ct.windows(secret_bytes.len()).any(|w| w == secret_bytes);
    assert!(!plaintext_leaked, "Plaintext must not leak in the ciphertext Eve received");
    for word in ["SECRET", "Launch", "codes", "12345"] {
        let word_bytes = word.as_bytes();
        let word_leaked = eve_ct.windows(word_bytes.len()).any(|w| w == word_bytes);
        assert!(!word_leaked, "Word '{word}' must not appear in the ciphertext Eve received");
    }
}
