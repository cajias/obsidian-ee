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
    bob_registry.join_encrypted(&invite, bob_pending).unwrap();

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
    bob_registry.join_encrypted(&invite, bob_pending).unwrap();

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
    bob_registry.join_encrypted(&bob_invite, bob_pending).unwrap();

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
        .join_encrypted(&carol_invite, carol_pending)
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
    let result = registry.join_encrypted(&invite, bob_pending);
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
    bob_registry.join_encrypted(&invite, bob_pending).unwrap();
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
    bob_registry.join_encrypted(&invite, bob_pending).unwrap();

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
