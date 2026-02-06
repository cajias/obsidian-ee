//! End-to-end tests for DocumentRegistry with encrypted documents.
//!
//! These tests verify the registry's ability to manage both plain and encrypted
//! documents, handle multi-user encrypted sessions, and maintain metadata correctly.

use collab_core::{DocumentRegistry, DocumentVariant, MlsDocumentGroup, RegistryError};

// =============================================================================
// BASIC ENCRYPTED DOCUMENT MANAGEMENT
// =============================================================================

/// Test creating and retrieving encrypted documents in the registry.
#[test]
fn test_registry_create_encrypted() {
    let mut registry = DocumentRegistry::new();

    // Create encrypted document
    let doc = registry.create_encrypted("doc1", "alice").unwrap();
    doc.insert(0, "Hello encrypted world");

    // Verify it's listed
    assert_eq!(registry.list().len(), 1);

    // Verify we can retrieve it
    let doc = registry.get_encrypted("doc1").unwrap();
    assert_eq!(doc.get_content(), "Hello encrypted world");

    // Verify encryption metadata
    let meta = registry.get_encryption_metadata("doc1").unwrap();
    assert_eq!(meta.user_id(), "alice");
    assert!(meta.is_owner());
    assert_eq!(meta.epoch(), 0);
}

/// Test that plain and encrypted documents can coexist in the same registry.
#[test]
fn test_registry_mixed_documents() {
    let mut registry = DocumentRegistry::new();

    // Create plain document
    let plain_doc = registry.create("plain-doc").unwrap();
    plain_doc.insert(0, "Plain text");

    // Create encrypted document
    let enc_doc = registry.create_encrypted("enc-doc", "alice").unwrap();
    enc_doc.insert(0, "Secret text");

    // Both should be listed
    assert_eq!(registry.list().len(), 2);

    // Access with correct methods
    assert_eq!(registry.get("plain-doc").unwrap().get_content(), "Plain text");
    assert_eq!(
        registry.get_encrypted("enc-doc").unwrap().get_content(),
        "Secret text"
    );

    // Cross-type access returns None
    assert!(registry.get("enc-doc").is_none());
    assert!(registry.get_encrypted("plain-doc").is_none());

    // Metadata check
    assert!(registry.get_encryption_metadata("plain-doc").is_none());
    assert!(registry.get_encryption_metadata("enc-doc").is_some());
}

/// Test closing encrypted documents.
#[test]
fn test_registry_close_encrypted() {
    let mut registry = DocumentRegistry::new();

    registry.create_encrypted("doc1", "alice").unwrap();

    // close() returns None for encrypted docs
    assert!(registry.close("doc1").is_none());

    // Document still exists
    assert!(registry.get_encrypted("doc1").is_some());

    // close_any() works
    let variant = registry.close_any("doc1").unwrap();
    assert!(matches!(variant, DocumentVariant::Encrypted(_)));

    // Now it's gone
    assert!(registry.get_encrypted("doc1").is_none());
}

// =============================================================================
// MULTI-USER ENCRYPTED COLLABORATION
// =============================================================================

/// Test Alice creating a document and Bob joining via invite.
#[test]
fn test_registry_join_encrypted() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Alice creates document
    alice_registry.create_encrypted("doc1", "alice").unwrap();

    // Bob generates key package
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

    // Alice creates invite
    let invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();

    // Bob joins
    bob_registry.join_encrypted(&invite, bob_pending, invite.epoch).unwrap();

    // Verify Bob's metadata
    let bob_meta = bob_registry.get_encryption_metadata("doc1").unwrap();
    assert_eq!(bob_meta.user_id(), "bob");
    assert!(!bob_meta.is_owner());
    assert_eq!(bob_meta.epoch(), 1);

    // Verify Alice's epoch was updated
    let alice_meta = alice_registry.get_encryption_metadata("doc1").unwrap();
    assert_eq!(alice_meta.epoch(), 1);
}

/// Test encrypted message exchange between Alice and Bob through the registry.
#[test]
fn test_registry_encrypted_message_exchange() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Setup: Alice creates, Bob joins
    alice_registry.create_encrypted("doc1", "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();
    bob_registry.join_encrypted(&invite, bob_pending, invite.epoch).unwrap();

    // Alice sends message
    let alice_doc = alice_registry.get_encrypted_mut("doc1").unwrap();
    alice_doc.insert(0, "Hello Bob!");
    let alice_op = alice_doc.get_encrypted_update().unwrap();

    // Bob receives and decrypts
    let bob_doc = bob_registry.get_encrypted_mut("doc1").unwrap();
    bob_doc.apply_encrypted_update(&alice_op).unwrap();
    assert_eq!(bob_doc.get_content(), "Hello Bob!");

    // Bob sends reply
    bob_doc.insert(10, " Hello Alice!");
    let bob_op = bob_doc.get_encrypted_update().unwrap();

    // Alice receives and decrypts
    let alice_doc = alice_registry.get_encrypted_mut("doc1").unwrap();
    alice_doc.apply_encrypted_update(&bob_op).unwrap();
    assert_eq!(alice_doc.get_content(), "Hello Bob! Hello Alice!");

    // Verify ciphertexts are encrypted (don't contain plaintext)
    assert!(
        !alice_op
            .ciphertext
            .windows(5)
            .any(|w| w == b"Hello"),
        "Alice's ciphertext should not contain plaintext"
    );
    assert!(
        !bob_op
            .ciphertext
            .windows(5)
            .any(|w| w == b"Alice"),
        "Bob's ciphertext should not contain plaintext"
    );
}

/// Test three-user collaboration with commit processing.
#[test]
fn test_registry_three_user_collaboration() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();
    let mut carol_registry = DocumentRegistry::new();

    // Alice creates document
    alice_registry.create_encrypted("doc1", "alice").unwrap();

    // Bob joins
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let bob_invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();
    bob_registry.join_encrypted(&bob_invite, bob_pending, bob_invite.epoch).unwrap();

    // Carol joins (Alice adds her, Bob processes commit)
    let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
    let carol_invite = alice_registry
        .create_invite("doc1", carol_pending.key_package())
        .unwrap();

    // Bob must process the commit to stay in sync
    bob_registry
        .process_commit("doc1", &carol_invite.commit)
        .unwrap();

    // Carol joins
    carol_registry
        .join_encrypted(&carol_invite, carol_pending, carol_invite.epoch)
        .unwrap();

    // Verify epochs
    assert_eq!(
        alice_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        2
    );
    assert_eq!(
        bob_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        2
    );
    assert_eq!(
        carol_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        2
    );

    // All three can communicate
    let alice_doc = alice_registry.get_encrypted_mut("doc1").unwrap();
    alice_doc.insert(0, "Alice");
    let alice_op = alice_doc.get_encrypted_update().unwrap();

    let bob_doc = bob_registry.get_encrypted_mut("doc1").unwrap();
    bob_doc.apply_encrypted_update(&alice_op).unwrap();

    let carol_doc = carol_registry.get_encrypted_mut("doc1").unwrap();
    carol_doc.apply_encrypted_update(&alice_op).unwrap();

    assert_eq!(bob_doc.get_content(), "Alice");
    assert_eq!(carol_doc.get_content(), "Alice");
}

// =============================================================================
// ERROR HANDLING
// =============================================================================

/// Test that plain document operations on encrypted docs return proper errors.
#[test]
fn test_registry_error_plain_ops_on_encrypted() {
    let mut registry = DocumentRegistry::new();
    registry.create_encrypted("doc1", "alice").unwrap();

    // create_invite on plain doc returns error
    let mut plain_registry = DocumentRegistry::new();
    plain_registry.create("doc2").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let result = plain_registry.create_invite("doc2", bob_pending.key_package());
    assert!(matches!(result, Err(RegistryError::NotEncrypted(_))));

    // process_commit on plain doc returns error
    let result = plain_registry.process_commit("doc2", &[1, 2, 3]);
    assert!(matches!(result, Err(RegistryError::NotEncrypted(_))));
}

/// Test duplicate document creation errors.
#[test]
fn test_registry_duplicate_encrypted() {
    let mut registry = DocumentRegistry::new();
    registry.create_encrypted("doc1", "alice").unwrap();

    // Try to create again
    let result = registry.create_encrypted("doc1", "bob");
    assert!(matches!(result, Err(RegistryError::AlreadyExists(_))));

    // Try to join with same ID
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();
    let result = registry.join_encrypted(&invite, bob_pending, invite.epoch);
    assert!(matches!(result, Err(RegistryError::AlreadyExists(_))));
}

/// Test that operations on non-existent documents return proper errors.
#[test]
fn test_registry_not_found_errors() {
    let mut registry = DocumentRegistry::new();

    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let result = registry.create_invite("nonexistent", bob_pending.key_package());
    assert!(matches!(result, Err(RegistryError::NotFound(_))));

    let result = registry.process_commit("nonexistent", &[1, 2, 3]);
    assert!(matches!(result, Err(RegistryError::NotFound(_))));
}

// =============================================================================
// METADATA TRACKING
// =============================================================================

/// Test that encryption metadata is properly maintained.
#[test]
fn test_registry_encryption_metadata_tracking() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Create and verify initial metadata
    alice_registry.create_encrypted("doc1", "alice").unwrap();
    let meta = alice_registry.get_encryption_metadata("doc1").unwrap();
    assert_eq!(meta.user_id(), "alice");
    assert!(meta.is_owner());
    assert_eq!(meta.epoch(), 0);

    // Add Bob, check epoch updates
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();
    assert_eq!(
        alice_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        1
    );

    // Bob joins, check his metadata
    bob_registry.join_encrypted(&invite, bob_pending, invite.epoch).unwrap();
    let bob_meta = bob_registry.get_encryption_metadata("doc1").unwrap();
    assert_eq!(bob_meta.user_id(), "bob");
    assert!(!bob_meta.is_owner());
    assert_eq!(bob_meta.epoch(), 1);

    // Add Carol, check epoch updates for both
    let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
    let carol_invite = alice_registry
        .create_invite("doc1", carol_pending.key_package())
        .unwrap();

    bob_registry
        .process_commit("doc1", &carol_invite.commit)
        .unwrap();

    assert_eq!(
        alice_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        2
    );
    assert_eq!(
        bob_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        2
    );
}

/// Test that regular document metadata works for encrypted documents.
#[test]
fn test_registry_encrypted_has_document_metadata() {
    let mut registry = DocumentRegistry::new();

    let before = std::time::SystemTime::now();
    registry.create_encrypted("doc1", "alice").unwrap();
    let after = std::time::SystemTime::now();

    // Verify document metadata exists
    let meta = registry.get_metadata("doc1").unwrap();
    assert!(meta.created_at() >= before);
    assert!(meta.created_at() <= after);

    // Can set custom metadata
    registry
        .set_custom_metadata("doc1", "purpose", "collaboration")
        .unwrap();
    let meta = registry.get_metadata("doc1").unwrap();
    assert_eq!(
        meta.custom().get("purpose"),
        Some(&"collaboration".to_string())
    );

    // Can touch to update last_modified
    let old_last_modified = meta.last_modified();
    std::thread::sleep(std::time::Duration::from_millis(10));
    registry.touch("doc1").unwrap();
    let new_meta = registry.get_metadata("doc1").unwrap();
    assert!(new_meta.last_modified() > old_last_modified);
}

// =============================================================================
// CONCURRENT OPERATIONS
// =============================================================================

/// Test concurrent edits converge with encrypted documents in registry.
#[test]
fn test_registry_concurrent_edits_converge() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Setup
    alice_registry.create_encrypted("doc1", "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();
    bob_registry.join_encrypted(&invite, bob_pending, invite.epoch).unwrap();

    // Alice and Bob make concurrent edits
    let alice_doc = alice_registry.get_encrypted_mut("doc1").unwrap();
    alice_doc.insert(0, "Alice");
    let alice_op1 = alice_doc.get_encrypted_update().unwrap();

    let bob_doc = bob_registry.get_encrypted_mut("doc1").unwrap();
    bob_doc.insert(0, "Bob");
    let bob_op1 = bob_doc.get_encrypted_update().unwrap();

    // Apply updates in different orders
    alice_doc.apply_encrypted_update(&bob_op1).unwrap();
    bob_doc.apply_encrypted_update(&alice_op1).unwrap();

    // Both should converge to same content (CRDT guarantee)
    let alice_content = alice_doc.get_content();
    let bob_content = bob_doc.get_content();
    assert_eq!(alice_content, bob_content);
    assert!(alice_content.contains("Alice"));
    assert!(alice_content.contains("Bob"));
}

/// Test that epoch mismatch is properly detected and rejected.
///
/// This test verifies that when a member tries to decrypt a message from a future
/// epoch they haven't processed yet, the system properly rejects it rather than
/// silently failing or corrupting state.
#[test]
fn test_registry_epoch_mismatch_rejected() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Alice creates document (epoch 0)
    alice_registry.create_encrypted("doc1", "alice").unwrap();

    // Bob joins (both advance to epoch 1)
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let bob_invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();
    bob_registry.join_encrypted(&bob_invite, bob_pending, bob_invite.epoch).unwrap();

    // Verify both are at epoch 1
    assert_eq!(
        alice_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        1
    );
    assert_eq!(
        bob_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        1
    );

    // Alice adds Carol (Alice advances to epoch 2)
    let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
    let carol_invite = alice_registry
        .create_invite("doc1", carol_pending.key_package())
        .unwrap();

    // Verify Alice is now at epoch 2
    assert_eq!(
        alice_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        2
    );

    // Bob is still at epoch 1 (hasn't processed Carol's commit yet)
    assert_eq!(
        bob_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        1
    );

    // Alice encrypts a message at epoch 2
    let alice_doc = alice_registry.get_encrypted_mut("doc1").unwrap();
    alice_doc.insert(0, "Message from future epoch");
    let alice_op = alice_doc.get_encrypted_update().unwrap();

    // Bob tries to decrypt the message from future epoch - this should fail
    let bob_doc = bob_registry.get_encrypted_mut("doc1").unwrap();
    let result = bob_doc.apply_encrypted_update(&alice_op);

    assert!(
        result.is_err(),
        "Should reject message from future epoch (Bob is at epoch 1, message is from epoch 2)"
    );

    // Verify Bob's state is unchanged after failed decryption
    assert_eq!(bob_doc.get_content(), "");
    assert_eq!(
        bob_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        1,
        "Bob's epoch should remain unchanged after failed decryption"
    );

    // Now Bob processes the commit to catch up to epoch 2
    bob_registry
        .process_commit("doc1", &carol_invite.commit)
        .unwrap();

    assert_eq!(
        bob_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        2
    );

    // Now Bob can decrypt Alice's message
    let bob_doc = bob_registry.get_encrypted_mut("doc1").unwrap();
    bob_doc.apply_encrypted_update(&alice_op).unwrap();
    assert_eq!(bob_doc.get_content(), "Message from future epoch");
}

/// Test that stale invites are properly rejected.
///
/// This test verifies the race condition where Alice adds Bob (epoch 1), then adds
/// Carol (epoch 2), and Bob tries to join with the stale epoch 1 invite. The system
/// should reject the stale invite to prevent group state corruption.
#[test]
fn test_registry_stale_invite_rejected() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Alice creates document (epoch 0)
    alice_registry.create_encrypted("doc1", "alice").unwrap();

    // Alice creates invite for Bob (Alice advances to epoch 1)
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let stale_bob_invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();

    // Verify invite was created at epoch 1
    assert_eq!(stale_bob_invite.epoch, 1);

    // Before Bob joins, Alice creates another invite for Carol (Alice advances to epoch 2)
    let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
    let _carol_invite = alice_registry
        .create_invite("doc1", carol_pending.key_package())
        .unwrap();

    // The current group epoch is now 2
    let current_epoch = alice_registry
        .get_encryption_metadata("doc1")
        .unwrap()
        .epoch();
    assert_eq!(current_epoch, 2);

    // Bob tries to join with the stale invite from epoch 1.
    // The relay/transport layer provides the current group epoch (2).
    let result = bob_registry.join_encrypted(&stale_bob_invite, bob_pending, current_epoch);

    // Stale invite should be rejected
    assert!(
        matches!(result, Err(RegistryError::StaleInvite { invite_epoch: 1, current_epoch: 2, .. })),
        "Stale invite should be rejected: invite epoch 1, current epoch 2, got: {}",
        if result.is_ok() { "Ok(_)" } else { "unexpected Err" }
    );

    // Bob should NOT be in the registry (join was rejected)
    assert!(
        bob_registry.get_encrypted("doc1").is_none(),
        "Bob should not have joined with a stale invite"
    );
}

/// Test that invalid commits are properly rejected.
///
/// This test verifies that when process_commit() receives arbitrary or corrupted
/// commit data that doesn't correspond to the group state, it properly rejects it
/// without corrupting the group state.
#[test]
fn test_registry_process_invalid_commit() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Alice creates document (epoch 0)
    alice_registry.create_encrypted("doc1", "alice").unwrap();

    // Bob joins (both at epoch 1)
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let bob_invite = alice_registry
        .create_invite("doc1", bob_pending.key_package())
        .unwrap();
    bob_registry.join_encrypted(&bob_invite, bob_pending, bob_invite.epoch).unwrap();

    // Record Bob's epoch before the invalid commit
    let bob_epoch_before = bob_registry
        .get_encryption_metadata("doc1")
        .unwrap()
        .epoch();
    assert_eq!(bob_epoch_before, 1);

    // Try to process an invalid commit (arbitrary bytes)
    let invalid_commit = b"this is not a valid MLS commit";
    let result = bob_registry.process_commit("doc1", invalid_commit);

    assert!(
        result.is_err(),
        "Should reject invalid commit data"
    );

    // Verify Bob's state is unchanged after rejected commit
    let bob_epoch_after = bob_registry
        .get_encryption_metadata("doc1")
        .unwrap()
        .epoch();
    assert_eq!(
        bob_epoch_after, bob_epoch_before,
        "Epoch should remain unchanged after rejected commit"
    );

    // Verify Bob can still decrypt messages from Alice (group state not corrupted)
    let alice_doc = alice_registry.get_encrypted_mut("doc1").unwrap();
    alice_doc.insert(0, "Test message after invalid commit");
    let alice_op = alice_doc.get_encrypted_update().unwrap();

    let bob_doc = bob_registry.get_encrypted_mut("doc1").unwrap();
    bob_doc.apply_encrypted_update(&alice_op).unwrap();
    assert_eq!(
        bob_doc.get_content(),
        "Test message after invalid commit",
        "Bob should still be able to decrypt after rejecting invalid commit"
    );

    // Try to process a commit from the wrong group
    // (This simulates a commit meant for a different document)
    let mut carol_registry = DocumentRegistry::new();
    carol_registry.create_encrypted("doc2", "carol").unwrap();

    let wrong_group_pending = MlsDocumentGroup::generate_key_package("dave").unwrap();
    let wrong_group_invite = carol_registry
        .create_invite("doc2", wrong_group_pending.key_package())
        .unwrap();

    // Try to process doc2's commit on doc1 - should be rejected
    let result = bob_registry.process_commit("doc1", &wrong_group_invite.commit);

    assert!(
        result.is_err(),
        "Should reject commit from wrong group"
    );

    // Verify Bob's state remains unchanged
    assert_eq!(
        bob_registry
            .get_encryption_metadata("doc1")
            .unwrap()
            .epoch(),
        bob_epoch_before,
        "Epoch should remain unchanged after rejected wrong-group commit"
    );
}

// =============================================================================
// PERFORMANCE & SCALABILITY
// =============================================================================

/// Test that encryption works with large documents (~1MB).
///
/// This test verifies that MLS encryption doesn't fail or timeout on realistic
/// data sizes. Many collaborative editing sessions involve documents with
/// thousands of lines of text.
#[test]
fn test_registry_large_document_encryption() {
    let mut alice_registry = DocumentRegistry::new();
    let mut bob_registry = DocumentRegistry::new();

    // Alice creates document
    alice_registry.create_encrypted("large-doc", "alice").unwrap();

    // Bob joins
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let bob_invite = alice_registry
        .create_invite("large-doc", bob_pending.key_package())
        .unwrap();
    bob_registry.join_encrypted(&bob_invite, bob_pending, bob_invite.epoch).unwrap();

    // Create a large document (~1MB of text)
    // Simulate a realistic document with multiple paragraphs
    let paragraph = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
                     Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris. \
                     Nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in \
                     reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\n\n";

    // Repeat to create ~1MB (paragraph is ~330 bytes, so ~3000 repetitions)
    let large_text = paragraph.repeat(3000);
    assert!(
        large_text.len() > 900_000,
        "Document should be close to 1MB"
    );

    // Alice inserts large text
    let alice_doc = alice_registry.get_encrypted_mut("large-doc").unwrap();
    alice_doc.insert(0, &large_text);

    // Get encrypted update
    let alice_op = alice_doc.get_encrypted_update().unwrap();

    // Verify the ciphertext doesn't leak plaintext
    assert!(
        !alice_op
            .ciphertext
            .windows(5)
            .any(|w| w == b"Lorem"),
        "Ciphertext should not contain plaintext"
    );

    // Bob decrypts the large document
    let bob_doc = bob_registry.get_encrypted_mut("large-doc").unwrap();
    bob_doc.apply_encrypted_update(&alice_op).unwrap();

    // Verify content matches
    assert_eq!(
        bob_doc.get_content(),
        large_text,
        "Large document should decrypt correctly"
    );

    // Verify we can make incremental updates after large document
    alice_doc.insert(0, "HEADER: ");
    let update2 = alice_doc.get_encrypted_update().unwrap();

    bob_doc.apply_encrypted_update(&update2).unwrap();
    assert!(
        bob_doc.get_content().starts_with("HEADER: "),
        "Incremental updates should work after large document"
    );
}
