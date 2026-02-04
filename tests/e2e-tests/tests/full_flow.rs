//! Full end-to-end flow tests for collaborative editing.
//!
//! These tests verify the complete collaboration flow:
//! - MLS encryption hides content from the relay
//! - Two users can collaborate on a document
//! - Offline messages are queued and delivered on reconnect

use collab_core::{EncryptedDocument, EncryptedOp, MlsDocumentGroup};
use collab_proto::{ClientMessage, DocumentId, MlsMessageType, ServerMessage};
use e2e_tests::helpers::TestClient;

/// Test that encryption actually hides content from the ciphertext.
///
/// This is a unit test that doesn't require Docker or the relay server.
#[test]
fn test_encryption_hides_content() {
    // Alice creates a document
    let mut alice_doc = EncryptedDocument::create("doc1", "alice").unwrap();

    // Bob generates a key package to join
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

    // Alice adds Bob to the group
    let _invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

    // Alice writes some secret content
    let secret_text = "This is a very secret message that should be encrypted";
    alice_doc.insert(0, secret_text);

    // Get the encrypted update
    let encrypted_op: EncryptedOp = alice_doc.get_encrypted_update().unwrap();

    // Verify the ciphertext doesn't contain the plaintext
    let ciphertext = &encrypted_op.ciphertext;

    // Check that none of the words from the secret appear in the ciphertext
    for word in ["secret", "message", "encrypted", "This", "very"] {
        let word_bytes = word.as_bytes();
        let contains_word = ciphertext.windows(word_bytes.len()).any(|window| window == word_bytes);
        assert!(!contains_word, "Ciphertext should not contain the word '{word}' in plaintext");
    }

    // Also verify the ciphertext is not empty (encryption actually happened)
    assert!(!ciphertext.is_empty(), "Ciphertext should not be empty");

    // Verify epoch is set
    assert!(encrypted_op.epoch > 0, "Epoch should be greater than 0");
}

/// Test that two users can collaborate on a document through the relay.
///
/// This test requires Docker Compose to be running with the relay server.
#[tokio::test]
#[ignore = "Requires Docker: docker compose -f docker/docker-compose.yml up -d"]
async fn test_two_users_collaborate() {
    let relay_url = "ws://localhost:8080/ws";
    let doc_id: DocumentId = "test-doc-collab".to_string();

    // Alice connects and identifies
    let mut alice = TestClient::connect_as(relay_url, "alice").await.unwrap();

    // Bob connects and identifies
    let mut bob = TestClient::connect_as(relay_url, "bob").await.unwrap();

    // Both subscribe to the document
    alice.send(&ClientMessage::Subscribe { doc_id: doc_id.clone() }).await.unwrap();

    bob.send(&ClientMessage::Subscribe { doc_id: doc_id.clone() }).await.unwrap();

    // Wait for subscription confirmations
    let alice_sub = alice.recv().await.unwrap();
    assert!(
        matches!(alice_sub, ServerMessage::Subscribed { .. }),
        "Alice should receive subscription confirmation"
    );

    let bob_sub = bob.recv().await.unwrap();
    assert!(
        matches!(bob_sub, ServerMessage::Subscribed { .. }),
        "Bob should receive subscription confirmation"
    );

    // Alice creates the encrypted document
    let mut alice_doc = EncryptedDocument::create(&doc_id, "alice").unwrap();

    // Bob generates key package
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

    // Alice sends Bob's key package via the relay (simulating key exchange)
    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: bob_pending.key_package().to_vec(),
            message_type: MlsMessageType::KeyPackage,
        })
        .await
        .unwrap();

    // Alice creates invite for Bob
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

    // Alice sends welcome message to Bob
    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await
        .unwrap();

    // Bob receives the welcome message
    let ServerMessage::MlsHandshake { payload: welcome_payload, .. } = bob.recv().await.unwrap()
    else {
        panic!("Expected MlsHandshake message")
    };

    // Bob joins using the welcome
    let bob_invite = collab_core::Invite { doc_id: doc_id.clone(), welcome: welcome_payload };
    let mut bob_doc = EncryptedDocument::join(&bob_invite, bob_pending).unwrap();

    // Alice edits the document
    alice_doc.insert(0, "Hello from Alice!");
    let alice_update = alice_doc.get_encrypted_update().unwrap();

    // Alice sends the encrypted update
    alice
        .send(&ClientMessage::YrsUpdate {
            doc_id: doc_id.clone(),
            encrypted: alice_update.ciphertext.clone(),
            epoch: alice_update.epoch,
            signature: vec![], // TODO: Add proper signatures in T12
        })
        .await
        .unwrap();

    // Bob receives the update
    let ServerMessage::YrsUpdate { encrypted, epoch, .. } = bob.recv().await.unwrap() else {
        panic!("Expected YrsUpdate message")
    };

    // Bob decrypts and applies the update
    let received_op = EncryptedOp { ciphertext: encrypted, epoch };
    bob_doc.apply_encrypted_update(&received_op).unwrap();

    // Verify Bob has the same content as Alice
    assert_eq!(bob_doc.get_content(), "Hello from Alice!");
    assert_eq!(alice_doc.get_content(), bob_doc.get_content());
}

/// Test that offline messages are delivered when a user reconnects.
///
/// This test requires Docker Compose to be running with the relay server.
#[tokio::test]
#[ignore = "Requires Docker: docker compose -f docker/docker-compose.yml up -d"]
async fn test_offline_message_delivery() {
    let relay_url = "ws://localhost:8080/ws";
    let doc_id: DocumentId = "test-doc-offline".to_string();

    // Alice connects and subscribes
    let mut alice = TestClient::connect_as(relay_url, "alice").await.unwrap();
    alice.send(&ClientMessage::Subscribe { doc_id: doc_id.clone() }).await.unwrap();
    let _ = alice.recv().await.unwrap(); // Subscription confirmation

    // Bob connects and subscribes
    let mut bob = TestClient::connect_as(relay_url, "bob").await.unwrap();
    bob.send(&ClientMessage::Subscribe { doc_id: doc_id.clone() }).await.unwrap();
    let _ = bob.recv().await.unwrap(); // Subscription confirmation

    // Set up MLS group
    let mut alice_doc = EncryptedDocument::create(&doc_id, "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

    // Send welcome to Bob
    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await
        .unwrap();

    // Bob receives welcome and joins
    let ServerMessage::MlsHandshake { payload: welcome_payload, .. } = bob.recv().await.unwrap()
    else {
        panic!("Expected MlsHandshake message")
    };

    let bob_invite = collab_core::Invite { doc_id: doc_id.clone(), welcome: welcome_payload };
    let mut bob_doc = EncryptedDocument::join(&bob_invite, bob_pending).unwrap();

    // Bob goes offline (drop connection)
    drop(bob);

    // Alice makes some edits while Bob is offline
    alice_doc.insert(0, "Edit 1. ");
    let update1 = alice_doc.get_encrypted_update().unwrap();
    alice
        .send(&ClientMessage::YrsUpdate {
            doc_id: doc_id.clone(),
            encrypted: update1.ciphertext.clone(),
            epoch: update1.epoch,
            signature: vec![],
        })
        .await
        .unwrap();

    alice_doc.insert(8, "Edit 2. ");
    let update2 = alice_doc.get_encrypted_update().unwrap();
    alice
        .send(&ClientMessage::YrsUpdate {
            doc_id: doc_id.clone(),
            encrypted: update2.ciphertext.clone(),
            epoch: update2.epoch,
            signature: vec![],
        })
        .await
        .unwrap();

    // Bob reconnects
    let mut bob = TestClient::connect_as(relay_url, "bob").await.unwrap();
    bob.send(&ClientMessage::Subscribe { doc_id: doc_id.clone() }).await.unwrap();
    let _ = bob.recv().await.unwrap(); // Subscription confirmation

    // Bob should receive the queued messages
    // Note: The relay should deliver offline messages on reconnect
    // This may need adjustment based on the actual relay implementation
    let msg1 = bob.recv().await.unwrap();
    if let ServerMessage::YrsUpdate { encrypted, epoch, .. } = msg1 {
        let op = EncryptedOp { ciphertext: encrypted, epoch };
        bob_doc.apply_encrypted_update(&op).unwrap();
    }

    let msg2 = bob.recv().await.unwrap();
    if let ServerMessage::YrsUpdate { encrypted, epoch, .. } = msg2 {
        let op = EncryptedOp { ciphertext: encrypted, epoch };
        bob_doc.apply_encrypted_update(&op).unwrap();
    }

    // Verify Bob has caught up with Alice
    assert_eq!(bob_doc.get_content(), alice_doc.get_content());
}
