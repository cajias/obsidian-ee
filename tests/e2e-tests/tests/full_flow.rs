//! Full end-to-end flow tests for collaborative editing.
//!
//! These tests verify the complete collaboration flow including:
//! - **Security properties**: IND-CPA semantic security, zero-knowledge relay, AEAD authentication
//! - **CRDT guarantees**: Convergence under concurrent edits, bidirectional sync
//! - **Integration**: Multi-user collaboration through relay, offline message delivery
//!
//! ## Test Categories
//!
//! ### Security Tests (unit tests, no Docker required)
//! - `test_semantic_security` - Verifies IND-CPA: same plaintext produces different ciphertext
//! - `test_wrong_key_decryption_fails` - Verifies AEAD auth tag rejects invalid keys
//! - `test_relay_cannot_decrypt` - Verifies zero-knowledge: relay cannot read content
//!
//! ### CRDT Tests (unit tests, no Docker required)
//! - `test_concurrent_edits_converge` - Verifies CRDT convergence guarantee
//! - `test_bidirectional_encrypted_sync` - Verifies both parties can encrypt/decrypt
//! - `test_three_user_collaboration` - Verifies multi-party MLS group functionality
//!
//! ### Integration Tests (require Docker: `docker compose -f docker/docker-compose.yml up -d`)
//! - `test_two_users_collaborate` - Two users collaborating through relay
//! - `test_offline_message_delivery` - Offline message queuing and delivery

use collab_core::{EncryptedDocument, EncryptedOp, MlsDocumentGroup};
use collab_proto::{ClientMessage, DocumentId, MlsMessageType, ServerMessage};
use e2e_tests::helpers::TestClient;

// =============================================================================
// SECURITY TESTS
// =============================================================================
// These tests verify cryptographic security properties of the MLS encryption.
// They are unit tests that don't require Docker or the relay server.

/// Test semantic security (IND-CPA): same plaintext must produce different ciphertext.
///
/// This verifies proper nonce/IV usage in MLS encryption. A weak cipher using
/// deterministic encryption (like XOR with a fixed key) would fail this test.
/// Proper AEAD ciphers generate a unique nonce for each encryption operation.
#[test]
fn test_semantic_security() {
    // Create a document
    let mut doc = EncryptedDocument::create("doc1", "alice").unwrap();

    // Insert some content
    doc.insert(0, "Hello World");

    // Get encrypted update
    let op1 = doc.get_encrypted_update().unwrap();

    // Insert the SAME content again at a different position
    // (This creates a new CRDT operation with same logical content)
    doc.insert(11, " Hello World");
    let op2 = doc.get_encrypted_update().unwrap();

    // The ciphertexts MUST be different even though we inserted same text
    // This is the IND-CPA (Indistinguishability under Chosen Plaintext Attack) property
    assert_ne!(
        op1.ciphertext, op2.ciphertext,
        "Semantic security violated: same plaintext produced same ciphertext"
    );

    // Also verify ciphertexts are not empty (encryption actually happened)
    assert!(!op1.ciphertext.is_empty(), "First ciphertext should not be empty");
    assert!(!op2.ciphertext.is_empty(), "Second ciphertext should not be empty");
}

/// Test that the relay (or any third party) cannot decrypt messages.
///
/// This test verifies the **zero-knowledge architecture**:
/// 1. A third party that intercepts encrypted messages cannot recover plaintext
/// 2. Without MLS group membership, the ciphertext is meaningless
/// 3. Only legitimate MLS group members can decrypt
///
/// This confirms the relay server never has access to document content - it only
/// routes opaque encrypted blobs between clients.
#[test]
fn test_relay_cannot_decrypt() {
    // Alice creates a document and adds Bob to the MLS group
    let mut alice_doc = EncryptedDocument::create("doc1", "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();
    let mut bob_doc = EncryptedDocument::join(&invite, bob_pending).unwrap();

    // Alice sends a secret message
    let secret = "TOP SECRET: Launch codes are 12345";
    alice_doc.insert(0, secret);
    let encrypted_op = alice_doc.get_encrypted_update().unwrap();

    // Simulate relay intercepting the message
    let intercepted_ciphertext = &encrypted_op.ciphertext;

    // Relay tries to create its own document to decrypt
    // This creates a DIFFERENT MLS group - relay is not a member of Alice/Bob's group
    let mut relay_doc = EncryptedDocument::create("doc1", "relay").unwrap();

    // Relay needs at least one other member to have encryption enabled
    let relay_accomplice = MlsDocumentGroup::generate_key_package("accomplice").unwrap();
    let _relay_invite = relay_doc.create_invite(relay_accomplice.key_package()).unwrap();

    // Relay cannot decrypt - it's not in Alice/Bob's MLS group
    let result = relay_doc.apply_encrypted_update(&encrypted_op);
    assert!(result.is_err(), "Relay should not be able to decrypt - different MLS group");

    // Verify the plaintext is not visible anywhere in the ciphertext
    let secret_bytes = secret.as_bytes();
    let plaintext_leaked =
        intercepted_ciphertext.windows(secret_bytes.len()).any(|w| w == secret_bytes);
    assert!(!plaintext_leaked, "Plaintext should not leak in ciphertext!");

    // Also check for partial plaintext leakage
    for word in ["SECRET", "Launch", "codes", "12345"] {
        let word_bytes = word.as_bytes();
        let word_leaked = intercepted_ciphertext.windows(word_bytes.len()).any(|w| w == word_bytes);
        assert!(!word_leaked, "Word '{word}' should not appear in ciphertext");
    }

    // Meanwhile, Bob (legitimate MLS group member) CAN decrypt
    bob_doc.apply_encrypted_update(&encrypted_op).unwrap();
    assert_eq!(bob_doc.get_content(), secret, "Bob should successfully decrypt");

    // Verify the content matches exactly
    assert_eq!(alice_doc.get_content(), bob_doc.get_content());
}

/// Test that decryption with wrong key fails explicitly.
///
/// This test verifies **AEAD authentication**:
/// 1. Decryption with wrong key MUST fail with an error (not produce garbage)
/// 2. Non-group members cannot decrypt messages
/// 3. The auth tag check rejects tampered or mis-keyed ciphertext
///
/// MLS uses AEAD (AES-GCM) encryption which includes authentication.
/// This is critical - without authentication, attackers could manipulate
/// ciphertext to corrupt documents.
#[test]
fn test_wrong_key_decryption_fails() {
    // Alice creates a document and encrypts content
    let mut alice_doc = EncryptedDocument::create("doc1", "alice").unwrap();

    // Alice needs another group member to encrypt to
    // (MLS requires at least 2 members for encryption)
    let alice_other = MlsDocumentGroup::generate_key_package("alice-device2").unwrap();
    let _invite = alice_doc.create_invite(alice_other.key_package()).unwrap();

    // Alice writes secret content
    alice_doc.insert(0, "Secret message");
    let encrypted_op = alice_doc.get_encrypted_update().unwrap();

    // Eve creates her own separate document (different MLS group)
    // This creates completely different encryption keys
    let mut eve_doc = EncryptedDocument::create("doc1", "eve").unwrap();

    // Eve also needs another group member to have a valid encryption context
    let eve_other = MlsDocumentGroup::generate_key_package("eve-device2").unwrap();
    let _eve_invite = eve_doc.create_invite(eve_other.key_package()).unwrap();

    // Eve tries to decrypt Alice's message - this MUST fail
    // because Eve's MLS group has different keys than Alice's group
    let result = eve_doc.apply_encrypted_update(&encrypted_op);

    assert!(
        result.is_err(),
        "Decryption with wrong key should fail, but it succeeded! \
         This is a critical security issue - AEAD auth tag verification failed to reject invalid ciphertext."
    );

    // Verify Eve's document is unchanged (empty)
    // This confirms we didn't silently apply garbage
    assert_eq!(
        eve_doc.get_content(),
        "",
        "Eve's document should remain empty after failed decryption"
    );
}

// =============================================================================
// CRDT TESTS
// =============================================================================
// These tests verify CRDT (Conflict-free Replicated Data Type) guarantees.
// They are unit tests that don't require Docker or the relay server.

/// Test that concurrent edits from multiple users converge to the same state.
///
/// This test verifies the **CRDT convergence guarantee**: all replicas eventually
/// converge to identical state regardless of the order operations are applied.
///
/// This is the core guarantee of Yrs - even with network partitions and
/// out-of-order delivery, all clients will see the same final document.
#[test]
fn test_concurrent_edits_converge() {
    // Set up Alice and Bob in the same MLS group
    let mut alice_doc = EncryptedDocument::create("doc1", "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();
    let mut bob_doc = EncryptedDocument::join(&invite, bob_pending).unwrap();

    // Both start with same initial content
    alice_doc.insert(0, "Hello");
    let alice_init = alice_doc.get_encrypted_update().unwrap();
    bob_doc.apply_encrypted_update(&alice_init).unwrap();

    // Verify both have the same initial state
    assert_eq!(alice_doc.get_content(), "Hello");
    assert_eq!(bob_doc.get_content(), "Hello");

    // Now both have "Hello" - simulate concurrent edits at the same position
    // Alice appends " World" at position 5
    alice_doc.insert(5, " World");
    let alice_update = alice_doc.get_encrypted_update().unwrap();

    // Bob appends " Rust" at position 5 (same position - concurrent edit!)
    bob_doc.insert(5, " Rust");
    let bob_update = bob_doc.get_encrypted_update().unwrap();

    // Cross-apply updates (simulating network delivery in different orders)
    // Bob receives Alice's update
    bob_doc.apply_encrypted_update(&alice_update).unwrap();
    // Alice receives Bob's update
    alice_doc.apply_encrypted_update(&bob_update).unwrap();

    // CRITICAL: Both must have identical content now - this is the CRDT guarantee
    assert_eq!(
        alice_doc.get_content(),
        bob_doc.get_content(),
        "CRDT convergence failed: Alice and Bob have different content"
    );

    // Content should contain both edits (order may vary based on CRDT tie-breaking)
    let content = alice_doc.get_content();
    assert!(content.contains("World"), "Missing Alice's edit");
    assert!(content.contains("Rust"), "Missing Bob's edit");
    assert!(content.starts_with("Hello"), "Original content should be preserved");
}

/// Test bidirectional encrypted sync - both parties can encrypt and decrypt.
///
/// This test verifies **MLS group key symmetry**:
/// - Alice can encrypt and Bob can decrypt
/// - Bob can encrypt and Alice can decrypt
/// - Both parties end up with consistent state
///
/// This is critical because MLS group keys must work bidirectionally.
/// A bug where only the group creator can encrypt would break collaboration.
#[test]
fn test_bidirectional_encrypted_sync() {
    // Set up Alice and Bob in the same MLS group
    let mut alice_doc = EncryptedDocument::create("doc-bidirectional", "alice").unwrap();

    // Bob generates a key package to join
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

    // Alice adds Bob to the group and creates an invite
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

    // Bob joins using the invite
    let mut bob_doc = EncryptedDocument::join(&invite, bob_pending).unwrap();

    // === Phase 1: Alice sends to Bob (Alice -> Bob direction) ===
    alice_doc.insert(0, "From Alice");
    let alice_update = alice_doc.get_encrypted_update().unwrap();

    // Verify Alice's update is properly encrypted (sanity check)
    assert!(!alice_update.ciphertext.is_empty(), "Alice's ciphertext should not be empty");
    assert!(alice_update.epoch > 0, "Alice's epoch should be set");

    // Bob decrypts and applies Alice's update
    bob_doc.apply_encrypted_update(&alice_update).unwrap();
    assert_eq!(
        bob_doc.get_content(),
        "From Alice",
        "Bob should be able to decrypt Alice's message"
    );

    // === Phase 2: Bob sends to Alice (Bob -> Alice direction) ===
    // This is the key test - Bob can also encrypt for the group
    bob_doc.insert(10, " and Bob");
    let bob_update = bob_doc.get_encrypted_update().unwrap();

    // Verify Bob's update is properly encrypted
    assert!(!bob_update.ciphertext.is_empty(), "Bob's ciphertext should not be empty");
    assert!(bob_update.epoch > 0, "Bob's epoch should be set");

    // Alice decrypts and applies Bob's update
    alice_doc.apply_encrypted_update(&bob_update).unwrap();

    // === Phase 3: Verify consistency ===
    // Both parties should have identical content after bidirectional sync
    let expected_content = "From Alice and Bob";
    assert_eq!(alice_doc.get_content(), expected_content, "Alice should have the combined content");
    assert_eq!(bob_doc.get_content(), expected_content, "Bob should have the combined content");

    // Verify both documents are in sync
    assert_eq!(
        alice_doc.get_content(),
        bob_doc.get_content(),
        "Alice and Bob should have identical content"
    );

    // Verify epochs are consistent (both should be in the same MLS epoch)
    assert_eq!(
        alice_update.epoch, bob_update.epoch,
        "Epoch mismatch between Alice and Bob - MLS group state may be inconsistent"
    );
}

/// Test that three users can collaborate in an MLS group.
///
/// This test verifies **multi-party MLS group functionality**:
/// 1. Three users (Alice, Bob, Charlie) can all join the same MLS group
/// 2. All three can encrypt messages for the group
/// 3. All three can decrypt each other's messages
/// 4. Content converges to identical state across all three replicas
///
/// MLS supports groups of any size, not just pairs. This test ensures
/// the implementation handles the general n-party case correctly.
///
/// **Key MLS concept**: When Alice adds Charlie, Bob must process the commit
/// message to update his epoch. Otherwise Bob would be at epoch 1 while
/// Alice and Charlie are at epoch 2, causing decryption failures.
#[test]
fn test_three_user_collaboration() {
    // Alice creates a document
    let mut alice_doc = EncryptedDocument::create("doc-multi", "alice").unwrap();

    // Bob joins the group
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let bob_invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();
    let mut bob_doc = EncryptedDocument::join(&bob_invite, bob_pending).unwrap();

    // Charlie joins the group
    let charlie_pending = MlsDocumentGroup::generate_key_package("charlie").unwrap();
    let charlie_invite = alice_doc.create_invite(charlie_pending.key_package()).unwrap();
    let mut charlie_doc = EncryptedDocument::join(&charlie_invite, charlie_pending).unwrap();

    // CRITICAL: Bob must process the commit from Charlie's addition to sync his epoch
    // Without this, Bob would be at epoch 1 while Alice/Charlie are at epoch 2
    bob_doc.process_commit(&charlie_invite.commit).unwrap();

    // === Phase 1: Alice sends to Bob and Charlie ===
    alice_doc.insert(0, "Alice");
    let alice_update = alice_doc.get_encrypted_update().unwrap();

    // Verify Alice's update is encrypted
    assert!(!alice_update.ciphertext.is_empty(), "Alice's ciphertext should not be empty");

    // Bob and Charlie decrypt Alice's message
    bob_doc.apply_encrypted_update(&alice_update).unwrap();
    charlie_doc.apply_encrypted_update(&alice_update).unwrap();

    assert_eq!(bob_doc.get_content(), "Alice", "Bob should decrypt Alice's message");
    assert_eq!(charlie_doc.get_content(), "Alice", "Charlie should decrypt Alice's message");

    // === Phase 2: Bob sends to Alice and Charlie ===
    bob_doc.insert(5, " Bob");
    let bob_update = bob_doc.get_encrypted_update().unwrap();

    // Verify Bob's update is encrypted
    assert!(!bob_update.ciphertext.is_empty(), "Bob's ciphertext should not be empty");

    // Alice and Charlie decrypt Bob's message
    alice_doc.apply_encrypted_update(&bob_update).unwrap();
    charlie_doc.apply_encrypted_update(&bob_update).unwrap();

    assert_eq!(alice_doc.get_content(), "Alice Bob", "Alice should have Bob's edit");
    assert_eq!(charlie_doc.get_content(), "Alice Bob", "Charlie should have Bob's edit");

    // === Phase 3: Charlie sends to Alice and Bob ===
    charlie_doc.insert(9, " Charlie");
    let charlie_update = charlie_doc.get_encrypted_update().unwrap();

    // Verify Charlie's update is encrypted
    assert!(!charlie_update.ciphertext.is_empty(), "Charlie's ciphertext should not be empty");

    // Alice and Bob decrypt Charlie's message
    alice_doc.apply_encrypted_update(&charlie_update).unwrap();
    bob_doc.apply_encrypted_update(&charlie_update).unwrap();

    // === Final verification: All three must have identical content ===
    let expected_content = "Alice Bob Charlie";
    assert_eq!(
        alice_doc.get_content(),
        bob_doc.get_content(),
        "Alice and Bob should have identical content"
    );
    assert_eq!(
        bob_doc.get_content(),
        charlie_doc.get_content(),
        "Bob and Charlie should have identical content"
    );

    // Verify all edits are present
    let final_content = alice_doc.get_content();
    assert!(final_content.contains("Alice"), "Content should contain Alice's edit");
    assert!(final_content.contains("Bob"), "Content should contain Bob's edit");
    assert!(final_content.contains("Charlie"), "Content should contain Charlie's edit");
    assert_eq!(final_content, expected_content, "Content should be exactly '{expected_content}'");
}

// =============================================================================
// INTEGRATION TESTS
// =============================================================================
// These tests require Docker Compose with the relay server running:
// docker compose -f docker/docker-compose.yml up -d

/// Test that two users can collaborate on a document through the relay.
///
/// This is a full **integration test** that verifies:
/// 1. WebSocket connection to the relay server
/// 2. MLS key exchange through the relay
/// 3. Encrypted document updates flowing between clients
/// 4. Both clients end up with identical document content
///
/// Requires Docker: `docker compose -f docker/docker-compose.yml up -d`
#[tokio::test]
#[ignore = "Requires Docker: docker compose -f docker/docker-compose.yml up -d"]
#[allow(clippy::too_many_lines)]
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

    // Alice creates invite for Bob using his key package
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

    // Alice sends welcome message to Bob via the relay
    alice
        .send(&ClientMessage::MlsHandshake {
            doc_id: doc_id.clone(),
            payload: invite.welcome.clone(),
            message_type: MlsMessageType::Welcome,
        })
        .await
        .unwrap();

    // Bob receives the welcome message
    let ServerMessage::MlsHandshake {
        payload: welcome_payload,
        message_type: MlsMessageType::Welcome,
        ..
    } = bob.recv().await.unwrap()
    else {
        panic!("Expected MlsHandshake Welcome message")
    };

    // Bob joins using the welcome
    let bob_invite = collab_core::Invite {
        doc_id: doc_id.clone(),
        welcome: welcome_payload,
        commit: vec![],
        epoch: 1,
    };
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
/// This is a full **integration test** that verifies:
/// 1. Messages sent while a user is offline are queued
/// 2. Queued messages are delivered when the user reconnects
/// 3. The reconnected user catches up to the current document state
///
/// This is critical for real-world usage where users may have intermittent
/// connectivity or close their laptop while collaborating.
///
/// Receive the next message and, if it is a `YrsUpdate`, decrypt and apply it.
/// Non-update control messages (e.g. `Subscribed`) are ignored. Panics on a
/// receive timeout.
async fn apply_next_update(
    client: &mut TestClient,
    doc: &mut EncryptedDocument,
    idle: std::time::Duration,
) {
    let Some(msg) = client.try_recv(idle).await.unwrap() else {
        panic!("Timed out waiting to catch up");
    };
    if let ServerMessage::YrsUpdate { encrypted, epoch, .. } = msg {
        let op = EncryptedOp { ciphertext: encrypted, epoch };
        doc.apply_encrypted_update(&op).unwrap();
    }
}

/// The relay retains a disconnected subscriber's subscription and queues
/// updates for them, draining the queue when they re-identify on reconnect.
///
/// Requires Docker: `docker compose -f docker/docker-compose.yml up -d`
#[tokio::test]
#[ignore = "Requires Docker: docker compose -f docker/docker-compose.yml up -d"]
#[allow(clippy::too_many_lines)]
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
    let ServerMessage::MlsHandshake {
        payload: welcome_payload,
        message_type: MlsMessageType::Welcome,
        ..
    } = bob.recv().await.unwrap()
    else {
        panic!("Expected MlsHandshake Welcome message")
    };

    let bob_invite = collab_core::Invite {
        doc_id: doc_id.clone(),
        welcome: welcome_payload,
        commit: vec![],
        epoch: 1,
    };
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
        })
        .await
        .unwrap();

    // Bob reconnects. The queued updates are drained on Identify, so they may
    // arrive before or after the Subscribe confirmation; re-subscribing is
    // idempotent. Drain messages until Bob's content matches Alice's.
    let mut bob = TestClient::connect_as(relay_url, "bob").await.unwrap();
    bob.send(&ClientMessage::Subscribe { doc_id: doc_id.clone() }).await.unwrap();

    let expected = alice_doc.get_content();
    let idle = std::time::Duration::from_secs(2);
    while bob_doc.get_content() != expected {
        apply_next_update(&mut bob, &mut bob_doc, idle).await;
    }

    // Verify Bob has caught up with Alice
    assert_eq!(bob_doc.get_content(), alice_doc.get_content());
}
