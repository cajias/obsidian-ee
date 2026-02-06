//! Document registry for managing multiple collaborative documents.

use crate::document::CollabDocument;
use crate::encryption::EncryptedDocument;
use crate::{DocumentId, Invite, PendingMember};
use std::collections::HashMap;
use std::time::SystemTime;

/// Error types for registry operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RegistryError {
    /// Document already exists.
    #[error("Document already exists: {0}")]
    AlreadyExists(DocumentId),
    /// Failed to restore document state.
    #[error("Failed to restore document state: {0}")]
    InvalidState(String),
    /// Document not found.
    #[error("Document not found: {0}")]
    NotFound(DocumentId),
    /// Document is not encrypted (attempted encrypted operation on plain doc).
    #[error("Document is not encrypted: {0}")]
    NotEncrypted(DocumentId),
    /// Document is encrypted (attempted plain operation on encrypted doc).
    #[error("Document is encrypted: {0}")]
    IsEncrypted(DocumentId),
    /// MLS operation failed.
    #[error("MLS error: {0}")]
    MlsError(String),
}

/// Variant for documents in the registry - either plain or encrypted.
pub enum DocumentVariant {
    /// An unencrypted collaborative document.
    Plain(CollabDocument),
    /// An encrypted collaborative document with MLS encryption.
    /// Boxed to reduce size difference between variants.
    Encrypted(Box<EncryptedDocument>),
}

/// Metadata about document encryption state.
#[derive(Debug, Clone)]
pub struct EncryptionMetadata {
    /// The user ID of the local participant.
    user_id: String,
    /// Whether this user is the owner (creator) of the encrypted document.
    is_owner: bool,
    /// The current MLS epoch (increments on membership changes).
    epoch: u64,
}

impl EncryptionMetadata {
    /// Create new encryption metadata.
    #[allow(clippy::missing_const_for_fn)] // String parameter prevents const
    fn new(user_id: String, is_owner: bool) -> Self {
        Self { user_id, is_owner, epoch: 0 }
    }

    /// The user ID of the local participant.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Whether this user is the owner (creator) of the encrypted document.
    #[must_use]
    pub const fn is_owner(&self) -> bool {
        self.is_owner
    }

    /// The current MLS epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Update the epoch.
    fn set_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
    }
}

/// Metadata associated with a document.
#[derive(Debug, Clone)]
pub struct DocumentMetadata {
    created_at: SystemTime,
    last_modified: SystemTime,
    custom: HashMap<String, String>,
}

impl DocumentMetadata {
    /// Create new metadata with current timestamps.
    fn new() -> Self {
        let now = SystemTime::now();
        Self { created_at: now, last_modified: now, custom: HashMap::new() }
    }

    /// When the document was created.
    #[must_use]
    pub const fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// When the document was last modified.
    #[must_use]
    pub const fn last_modified(&self) -> SystemTime {
        self.last_modified
    }

    /// Custom key-value metadata.
    #[must_use]
    pub const fn custom(&self) -> &HashMap<String, String> {
        &self.custom
    }

    /// Update `last_modified` to current time.
    fn touch(&mut self) {
        self.last_modified = SystemTime::now();
    }
}

/// An entry in the document registry containing both the document and its metadata.
pub struct DocumentEntry {
    variant: DocumentVariant,
    metadata: DocumentMetadata,
    encryption_metadata: Option<EncryptionMetadata>,
}

impl DocumentEntry {
    /// Create a new entry with a plain document.
    fn new_plain(doc: CollabDocument) -> Self {
        Self { variant: DocumentVariant::Plain(doc), metadata: DocumentMetadata::new(), encryption_metadata: None }
    }

    /// Create a new entry with an encrypted document.
    fn new_encrypted(doc: EncryptedDocument, user_id: String, is_owner: bool) -> Self {
        Self {
            variant: DocumentVariant::Encrypted(Box::new(doc)),
            metadata: DocumentMetadata::new(),
            encryption_metadata: Some(EncryptionMetadata::new(user_id, is_owner)),
        }
    }

    /// Get a reference to the document variant.
    #[must_use]
    pub const fn variant(&self) -> &DocumentVariant {
        &self.variant
    }

    /// Get a reference to the collaborative document (only for plain documents).
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // Match expression prevents const
    pub fn document(&self) -> Option<&CollabDocument> {
        match &self.variant {
            DocumentVariant::Plain(doc) => Some(doc),
            DocumentVariant::Encrypted(_) => None,
        }
    }

    /// Get a reference to the document metadata.
    #[must_use]
    pub const fn metadata(&self) -> &DocumentMetadata {
        &self.metadata
    }

    /// Get a reference to the encryption metadata (only for encrypted documents).
    #[must_use]
    pub const fn encryption_metadata(&self) -> Option<&EncryptionMetadata> {
        self.encryption_metadata.as_ref()
    }
}

/// A registry for managing multiple collaborative documents.
pub struct DocumentRegistry {
    documents: HashMap<DocumentId, DocumentEntry>,
}

impl DocumentRegistry {
    /// Create a new empty document registry.
    #[must_use]
    pub fn new() -> Self {
        Self { documents: HashMap::new() }
    }

    /// Create a new document with the given ID.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::AlreadyExists` if a document with the given ID already exists.
    ///
    /// # Panics
    ///
    /// This function will not panic under normal circumstances. The internal
    /// `expect` is guarded by the insertion that occurs immediately before.
    pub fn create(
        &mut self,
        id: impl Into<DocumentId>,
    ) -> Result<&mut CollabDocument, RegistryError> {
        let id = id.into();
        if self.documents.contains_key(&id) {
            return Err(RegistryError::AlreadyExists(id));
        }
        let doc = CollabDocument::new(id.clone());
        let entry = DocumentEntry::new_plain(doc);
        self.documents.insert(id.clone(), entry);
        let entry = self.documents.get_mut(&id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Plain(doc) => Ok(doc),
            DocumentVariant::Encrypted(_) => unreachable!("just created plain"),
        }
    }

    /// Get a reference to a plain document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is encrypted.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CollabDocument> {
        self.documents.get(id).and_then(|entry| match &entry.variant {
            DocumentVariant::Plain(doc) => Some(doc),
            DocumentVariant::Encrypted(_) => None,
        })
    }

    /// Get a mutable reference to a plain document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is encrypted.
    #[must_use]
    pub fn get_mut(&mut self, id: &str) -> Option<&mut CollabDocument> {
        self.documents.get_mut(id).and_then(|entry| match &mut entry.variant {
            DocumentVariant::Plain(doc) => Some(doc),
            DocumentVariant::Encrypted(_) => None,
        })
    }

    /// List all document IDs in the registry.
    #[must_use]
    pub fn list(&self) -> Vec<&DocumentId> {
        self.documents.keys().collect()
    }

    /// Close and remove a plain document from the registry.
    ///
    /// Returns `None` if the document doesn't exist or is encrypted.
    /// Use `close_any` to close any document type.
    pub fn close(&mut self, id: &str) -> Option<CollabDocument> {
        // Check if it's a plain document first
        let is_plain = self.documents.get(id).is_some_and(|entry| {
            matches!(entry.variant, DocumentVariant::Plain(_))
        });
        if is_plain {
            self.documents.remove(id).and_then(|entry| match entry.variant {
                DocumentVariant::Plain(doc) => Some(doc),
                DocumentVariant::Encrypted(_) => None,
            })
        } else {
            None
        }
    }

    /// Close and remove any document from the registry.
    ///
    /// Returns the document variant, or `None` if the document doesn't exist.
    pub fn close_any(&mut self, id: &str) -> Option<DocumentVariant> {
        self.documents.remove(id).map(|entry| entry.variant)
    }

    /// Open a plain document with existing state.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::AlreadyExists` if a document with the given ID already exists.
    /// Returns `RegistryError::InvalidState` if the state cannot be applied.
    ///
    /// # Panics
    ///
    /// This function will not panic under normal circumstances. The internal
    /// `expect` is guarded by the insertion that occurs immediately before.
    pub fn open(
        &mut self,
        id: impl Into<DocumentId>,
        state: &[u8],
    ) -> Result<&mut CollabDocument, RegistryError> {
        let id = id.into();
        if self.documents.contains_key(&id) {
            return Err(RegistryError::AlreadyExists(id));
        }
        let mut doc = CollabDocument::new(id.clone());
        doc.apply_update(state)
            .map_err(|e| RegistryError::InvalidState(e.to_string()))?;
        let entry = DocumentEntry::new_plain(doc);
        self.documents.insert(id.clone(), entry);
        let entry = self.documents.get_mut(&id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Plain(doc) => Ok(doc),
            DocumentVariant::Encrypted(_) => unreachable!("just created plain"),
        }
    }

    /// Get metadata for a document by ID.
    #[must_use]
    pub fn get_metadata(&self, id: &str) -> Option<&DocumentMetadata> {
        self.documents.get(id).map(|entry| &entry.metadata)
    }

    /// Set custom metadata for a document.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::NotFound` if the document does not exist.
    pub fn set_custom_metadata(
        &mut self,
        id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), RegistryError> {
        let entry = self
            .documents
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        entry.metadata.custom.insert(key.to_string(), value.to_string());
        Ok(())
    }

    /// Update the `last_modified` timestamp for a document.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::NotFound` if the document does not exist.
    pub fn touch(&mut self, id: &str) -> Result<(), RegistryError> {
        let entry = self
            .documents
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        entry.metadata.touch();
        Ok(())
    }

    // ==================== Encrypted Document Methods ====================

    /// Create a new encrypted document as the owner.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::AlreadyExists` if a document with the given ID already exists.
    /// Returns `RegistryError::MlsError` if MLS group creation fails.
    ///
    /// # Panics
    ///
    /// This function will not panic under normal circumstances. The internal
    /// `expect` is guarded by the insertion that occurs immediately before.
    pub fn create_encrypted(
        &mut self,
        id: impl Into<DocumentId>,
        user_id: &str,
    ) -> Result<&mut EncryptedDocument, RegistryError> {
        let id = id.into();
        if self.documents.contains_key(&id) {
            return Err(RegistryError::AlreadyExists(id));
        }

        let doc = EncryptedDocument::create(&id, user_id)
            .map_err(|e| RegistryError::MlsError(e.to_string()))?;

        let mut entry = DocumentEntry::new_encrypted(doc, user_id.to_string(), true);

        // Set the epoch from the document
        if let (DocumentVariant::Encrypted(doc), Some(meta)) =
            (&entry.variant, &mut entry.encryption_metadata)
        {
            meta.set_epoch(doc.epoch());
        }

        self.documents.insert(id.clone(), entry);

        let entry = self.documents.get_mut(&id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => Ok(doc.as_mut()),
            DocumentVariant::Plain(_) => unreachable!("just created encrypted"),
        }
    }

    /// Join an existing encrypted document using an invite.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::AlreadyExists` if a document with the given ID already exists.
    /// Returns `RegistryError::MlsError` if joining fails.
    ///
    /// # Panics
    ///
    /// This function will not panic under normal circumstances. The internal
    /// `expect` is guarded by the insertion that occurs immediately before.
    pub fn join_encrypted(
        &mut self,
        invite: &Invite,
        pending: PendingMember,
    ) -> Result<&mut EncryptedDocument, RegistryError> {
        let doc_id = invite.doc_id.clone();
        if self.documents.contains_key(&doc_id) {
            return Err(RegistryError::AlreadyExists(doc_id));
        }

        let user_id = pending.user_id().to_string();
        let doc = EncryptedDocument::join(invite, pending)
            .map_err(|e| RegistryError::MlsError(e.to_string()))?;

        let mut entry = DocumentEntry::new_encrypted(doc, user_id, false);

        // Set the epoch from the document
        if let (DocumentVariant::Encrypted(doc), Some(meta)) =
            (&entry.variant, &mut entry.encryption_metadata)
        {
            meta.set_epoch(doc.epoch());
        }

        self.documents.insert(doc_id.clone(), entry);

        let entry = self.documents.get_mut(&doc_id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => Ok(doc.as_mut()),
            DocumentVariant::Plain(_) => unreachable!("just created encrypted"),
        }
    }

    /// Get a reference to an encrypted document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is plain.
    #[must_use]
    pub fn get_encrypted(&self, id: &str) -> Option<&EncryptedDocument> {
        self.documents.get(id).and_then(|entry| match &entry.variant {
            DocumentVariant::Encrypted(doc) => Some(doc.as_ref()),
            DocumentVariant::Plain(_) => None,
        })
    }

    /// Get a mutable reference to an encrypted document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is plain.
    #[must_use]
    pub fn get_encrypted_mut(&mut self, id: &str) -> Option<&mut EncryptedDocument> {
        self.documents.get_mut(id).and_then(|entry| match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => Some(doc.as_mut()),
            DocumentVariant::Plain(_) => None,
        })
    }

    /// Create an invite for another user to join an encrypted document.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::NotFound` if the document doesn't exist.
    /// Returns `RegistryError::NotEncrypted` if the document is not encrypted.
    /// Returns `RegistryError::MlsError` if invite creation fails.
    pub fn create_invite(
        &mut self,
        id: &str,
        key_package: &[u8],
    ) -> Result<Invite, RegistryError> {
        let entry = self
            .documents
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;

        let doc = match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => doc.as_mut(),
            DocumentVariant::Plain(_) => return Err(RegistryError::NotEncrypted(id.to_string())),
        };

        let invite = doc
            .create_invite(key_package)
            .map_err(|e| RegistryError::MlsError(e.to_string()))?;

        // Update epoch in metadata after adding member
        if let Some(meta) = &mut entry.encryption_metadata {
            meta.set_epoch(doc.epoch());
        }

        Ok(invite)
    }

    /// Process a commit message from another member (e.g., when a new member is added).
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::NotFound` if the document doesn't exist.
    /// Returns `RegistryError::NotEncrypted` if the document is not encrypted.
    /// Returns `RegistryError::MlsError` if processing the commit fails.
    pub fn process_commit(&mut self, id: &str, commit: &[u8]) -> Result<(), RegistryError> {
        let entry = self
            .documents
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;

        let doc = match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => doc.as_mut(),
            DocumentVariant::Plain(_) => return Err(RegistryError::NotEncrypted(id.to_string())),
        };

        doc.process_commit(commit)
            .map_err(|e| RegistryError::MlsError(e.to_string()))?;

        // Update epoch in metadata after processing commit
        if let Some(meta) = &mut entry.encryption_metadata {
            meta.set_epoch(doc.epoch());
        }

        Ok(())
    }

    /// Get encryption metadata for a document.
    ///
    /// Returns `None` if the document doesn't exist or is not encrypted.
    #[must_use]
    pub fn get_encryption_metadata(&self, id: &str) -> Option<&EncryptionMetadata> {
        self.documents.get(id).and_then(|entry| entry.encryption_metadata.as_ref())
    }
}

impl Default for DocumentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Phase 1 Tests: Basic Registry

    #[test]
    fn test_empty_registry_has_no_documents() {
        let registry = DocumentRegistry::new();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_create_and_get_document() {
        let mut registry = DocumentRegistry::new();

        // Create a document
        let doc = registry.create("doc-1").expect("should create document");
        doc.insert(0, "Hello");

        // Get the document
        let retrieved = registry.get("doc-1").expect("should get document");
        assert_eq!(retrieved.id(), "doc-1");
        assert_eq!(retrieved.get_content(), "Hello");
    }

    #[test]
    fn test_multiple_documents() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap();
        registry.create("doc-2").unwrap();
        registry.create("doc-3").unwrap();

        assert!(registry.get("doc-1").is_some());
        assert!(registry.get("doc-2").is_some());
        assert!(registry.get("doc-3").is_some());
        assert!(registry.get("doc-4").is_none());
    }

    #[test]
    fn test_list_document_ids() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-a").unwrap();
        registry.create("doc-b").unwrap();

        let ids = registry.list();
        assert_eq!(ids.len(), 2);

        let id_strings: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        assert!(id_strings.contains(&"doc-a"));
        assert!(id_strings.contains(&"doc-b"));
    }

    // Phase 2 Tests: Document Lifecycle

    #[test]
    fn test_close_document() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap().insert(0, "Hello");

        let closed = registry.close("doc-1").expect("should close document");
        assert_eq!(closed.get_content(), "Hello");

        // Document should no longer be in registry
        assert!(registry.get("doc-1").is_none());
    }

    #[test]
    fn test_get_closed_returns_none() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap();
        registry.close("doc-1");

        assert!(registry.get("doc-1").is_none());
        assert!(registry.get_mut("doc-1").is_none());
    }

    #[test]
    fn test_open_with_state() {
        let mut registry = DocumentRegistry::new();

        // Create a document and get its state
        registry.create("doc-1").unwrap().insert(0, "Hello World");
        let state = registry.get("doc-1").unwrap().encode_state();

        // Close the document
        registry.close("doc-1");

        // Reopen with saved state
        let doc = registry.open("doc-1", &state).expect("should open with state");
        assert_eq!(doc.get_content(), "Hello World");
    }

    #[test]
    fn test_duplicate_create_returns_error() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap();

        let result = registry.create("doc-1");
        assert!(matches!(result, Err(RegistryError::AlreadyExists(_))));
    }

    // Phase 3 Tests: Metadata Tracking

    #[test]
    fn test_metadata_has_created_at() {
        use std::time::SystemTime;

        let mut registry = DocumentRegistry::new();
        let before = SystemTime::now();

        registry.create("doc-1").unwrap();

        let after = SystemTime::now();
        let metadata = registry.get_metadata("doc-1").expect("should have metadata");

        assert!(metadata.created_at() >= before);
        assert!(metadata.created_at() <= after);
    }

    #[test]
    fn test_metadata_has_last_modified() {
        use std::time::SystemTime;

        let mut registry = DocumentRegistry::new();
        let before = SystemTime::now();

        registry.create("doc-1").unwrap();

        let after = SystemTime::now();
        let metadata = registry.get_metadata("doc-1").expect("should have metadata");

        assert!(metadata.last_modified() >= before);
        assert!(metadata.last_modified() <= after);
    }

    #[test]
    fn test_last_modified_updates_on_edit() {
        use std::time::Duration;

        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap();
        let initial_modified = registry.get_metadata("doc-1").unwrap().last_modified();

        // Small delay to ensure time difference
        std::thread::sleep(Duration::from_millis(10));

        // Touch the document to update last_modified
        registry.touch("doc-1").unwrap();

        let updated_modified = registry.get_metadata("doc-1").unwrap().last_modified();
        assert!(updated_modified > initial_modified);
    }

    #[test]
    fn test_custom_metadata() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap();

        // Set custom metadata
        registry
            .set_custom_metadata("doc-1", "author", "Alice")
            .expect("should set custom metadata");
        registry
            .set_custom_metadata("doc-1", "version", "1.0")
            .expect("should set custom metadata");

        let metadata = registry.get_metadata("doc-1").unwrap();
        assert_eq!(metadata.custom().get("author"), Some(&"Alice".to_string()));
        assert_eq!(metadata.custom().get("version"), Some(&"1.0".to_string()));
    }

    #[test]
    fn test_set_custom_metadata_not_found() {
        let mut registry = DocumentRegistry::new();

        let result = registry.set_custom_metadata("nonexistent", "key", "value");
        assert!(matches!(result, Err(RegistryError::NotFound(_))));
    }

    #[test]
    fn test_get_metadata_not_found() {
        let registry = DocumentRegistry::new();

        assert!(registry.get_metadata("nonexistent").is_none());
    }

    // Additional tests for edge cases and error paths

    #[test]
    fn test_open_with_invalid_state_returns_error() {
        let mut registry = DocumentRegistry::new();
        let invalid_state = b"this is not valid yrs state";

        let result = registry.open("doc-1", invalid_state);
        assert!(matches!(result, Err(RegistryError::InvalidState(_))));

        // Document should not be partially created
        assert!(registry.get("doc-1").is_none());
    }

    #[test]
    fn test_open_duplicate_returns_error() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap().insert(0, "Original");
        let state = registry.get("doc-1").unwrap().encode_state();

        // Try to open the same ID while document exists
        let result = registry.open("doc-1", &state);
        assert!(matches!(result, Err(RegistryError::AlreadyExists(_))));

        // Original document should be unchanged
        assert_eq!(registry.get("doc-1").unwrap().get_content(), "Original");
    }

    #[test]
    fn test_close_nonexistent_returns_none() {
        let mut registry = DocumentRegistry::new();

        let result = registry.close("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_touch_nonexistent_returns_error() {
        let mut registry = DocumentRegistry::new();

        let result = registry.touch("nonexistent");
        assert!(matches!(result, Err(RegistryError::NotFound(_))));
    }

    #[test]
    fn test_get_mut_allows_modification() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap();

        // Get mutable reference and modify
        let doc = registry.get_mut("doc-1").expect("should exist");
        doc.insert(0, "Modified via get_mut");

        // Verify change persisted
        assert_eq!(
            registry.get("doc-1").unwrap().get_content(),
            "Modified via get_mut"
        );
    }

    #[test]
    fn test_default_creates_empty_registry() {
        let registry = DocumentRegistry::default();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_custom_metadata_overwrites_existing() {
        let mut registry = DocumentRegistry::new();
        registry.create("doc-1").unwrap();

        registry.set_custom_metadata("doc-1", "key", "value1").unwrap();
        registry.set_custom_metadata("doc-1", "key", "value2").unwrap();

        let metadata = registry.get_metadata("doc-1").unwrap();
        assert_eq!(metadata.custom().get("key"), Some(&"value2".to_string()));
    }

    // ==================== Phase 1: Error Types & DocumentVariant ====================

    #[test]
    fn test_not_encrypted_error() {
        let err = RegistryError::NotEncrypted("doc-1".to_string());
        assert!(err.to_string().contains("not encrypted"));
        assert!(err.to_string().contains("doc-1"));
    }

    #[test]
    fn test_is_encrypted_error() {
        let err = RegistryError::IsEncrypted("doc-1".to_string());
        assert!(err.to_string().contains("encrypted"));
        assert!(err.to_string().contains("doc-1"));
    }

    #[test]
    fn test_mls_error() {
        let err = RegistryError::MlsError("failed to encrypt".to_string());
        assert!(err.to_string().contains("MLS"));
        assert!(err.to_string().contains("failed to encrypt"));
    }

    #[test]
    fn test_encryption_metadata_creation() {
        let meta = EncryptionMetadata::new("alice".to_string(), true);
        assert_eq!(meta.user_id(), "alice");
        assert!(meta.is_owner());
        assert_eq!(meta.epoch(), 0);
    }

    #[test]
    fn test_encryption_metadata_epoch_update() {
        let mut meta = EncryptionMetadata::new("bob".to_string(), false);
        assert_eq!(meta.epoch(), 0);
        meta.set_epoch(5);
        assert_eq!(meta.epoch(), 5);
    }

    // ==================== Phase 2: Create Encrypted ====================

    #[test]
    fn test_create_encrypted_document() {
        let mut registry = DocumentRegistry::new();

        let result = registry.create_encrypted("doc-1", "alice");
        assert!(result.is_ok());

        // Should be in the list
        assert!(registry.list().iter().any(|id| id.as_str() == "doc-1"));
    }

    #[test]
    fn test_create_encrypted_duplicate_returns_error() {
        let mut registry = DocumentRegistry::new();

        registry.create_encrypted("doc-1", "alice").unwrap();
        let result = registry.create_encrypted("doc-1", "bob");

        assert!(matches!(result, Err(RegistryError::AlreadyExists(_))));
    }

    #[test]
    fn test_get_encrypted_returns_ref() {
        let mut registry = DocumentRegistry::new();

        registry.create_encrypted("doc-1", "alice").unwrap();
        let doc = registry.get_encrypted("doc-1");

        assert!(doc.is_some());
    }

    #[test]
    fn test_get_encrypted_mut_allows_modification() {
        let mut registry = DocumentRegistry::new();

        registry.create_encrypted("doc-1", "alice").unwrap();

        // Modify via mutable reference
        let doc = registry.get_encrypted_mut("doc-1").expect("should exist");
        doc.insert(0, "Hello encrypted");

        // Verify change persisted
        let doc = registry.get_encrypted("doc-1").unwrap();
        assert_eq!(doc.get_content(), "Hello encrypted");
    }

    #[test]
    fn test_get_encrypted_on_plain_returns_none() {
        let mut registry = DocumentRegistry::new();

        registry.create("doc-1").unwrap();
        assert!(registry.get_encrypted("doc-1").is_none());
        assert!(registry.get_encrypted_mut("doc-1").is_none());
    }

    #[test]
    fn test_get_plain_on_encrypted_returns_none() {
        let mut registry = DocumentRegistry::new();

        registry.create_encrypted("doc-1", "alice").unwrap();
        assert!(registry.get("doc-1").is_none());
        assert!(registry.get_mut("doc-1").is_none());
    }

    #[test]
    fn test_encryption_metadata_on_create() {
        let mut registry = DocumentRegistry::new();

        registry.create_encrypted("doc-1", "alice").unwrap();
        let meta = registry.get_encryption_metadata("doc-1").expect("should have encryption metadata");

        assert_eq!(meta.user_id(), "alice");
        assert!(meta.is_owner());
        assert_eq!(meta.epoch(), 0);
    }

    // ==================== Phase 3: Join Encrypted ====================

    #[test]
    fn test_join_encrypted_document() {
        use crate::MlsDocumentGroup;

        let mut registry = DocumentRegistry::new();

        // Alice creates encrypted document
        registry.create_encrypted("doc-1", "alice").unwrap();

        // Bob generates key package
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

        // Alice creates invite for Bob
        let invite = registry.create_invite("doc-1", bob_pending.key_package()).unwrap();

        // Bob joins in a separate registry
        let mut bob_registry = DocumentRegistry::new();
        let result = bob_registry.join_encrypted(&invite, bob_pending);
        assert!(result.is_ok());
    }

    #[test]
    fn test_join_encrypted_metadata_not_owner() {
        use crate::MlsDocumentGroup;

        let mut registry = DocumentRegistry::new();

        // Alice creates encrypted document
        registry.create_encrypted("doc-1", "alice").unwrap();

        // Bob generates key package
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

        // Alice creates invite for Bob
        let invite = registry.create_invite("doc-1", bob_pending.key_package()).unwrap();

        // Bob joins
        let mut bob_registry = DocumentRegistry::new();
        bob_registry.join_encrypted(&invite, bob_pending).unwrap();

        // Bob's metadata should show is_owner=false
        let meta = bob_registry.get_encryption_metadata("doc-1").unwrap();
        assert_eq!(meta.user_id(), "bob");
        assert!(!meta.is_owner());
    }

    #[test]
    fn test_join_encrypted_can_decrypt() {
        use crate::MlsDocumentGroup;

        let mut registry = DocumentRegistry::new();

        // Alice creates and adds content
        registry.create_encrypted("doc-1", "alice").unwrap();

        // Bob generates key package
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

        // Alice creates invite for Bob
        let invite = registry.create_invite("doc-1", bob_pending.key_package()).unwrap();

        // Alice adds some content
        let alice_doc = registry.get_encrypted_mut("doc-1").unwrap();
        alice_doc.insert(0, "Hello Bob!");
        let encrypted_op = alice_doc.get_encrypted_update().unwrap();

        // Bob joins
        let mut bob_registry = DocumentRegistry::new();
        bob_registry.join_encrypted(&invite, bob_pending).unwrap();

        // Bob can decrypt Alice's message
        let bob_doc = bob_registry.get_encrypted_mut("doc-1").unwrap();
        bob_doc.apply_encrypted_update(&encrypted_op).unwrap();
        assert_eq!(bob_doc.get_content(), "Hello Bob!");
    }

    // ==================== Phase 4: Invite/Commit Flow ====================

    #[test]
    fn test_create_invite_returns_invite() {
        use crate::MlsDocumentGroup;

        let mut registry = DocumentRegistry::new();
        registry.create_encrypted("doc-1", "alice").unwrap();

        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let invite = registry.create_invite("doc-1", bob_pending.key_package());

        assert!(invite.is_ok());
        let invite = invite.unwrap();
        assert_eq!(invite.doc_id, "doc-1");
        assert!(!invite.welcome.is_empty());
        assert!(!invite.commit.is_empty());
    }

    #[test]
    fn test_create_invite_on_plain_returns_error() {
        use crate::MlsDocumentGroup;

        let mut registry = DocumentRegistry::new();
        registry.create("doc-1").unwrap();

        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let result = registry.create_invite("doc-1", bob_pending.key_package());

        assert!(matches!(result, Err(RegistryError::NotEncrypted(_))));
    }

    #[test]
    fn test_process_commit_updates_epoch() {
        use crate::MlsDocumentGroup;

        let mut alice_registry = DocumentRegistry::new();
        alice_registry.create_encrypted("doc-1", "alice").unwrap();

        // Bob generates key package
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

        // Carol generates key package
        let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();

        // Alice adds Bob
        let bob_invite = alice_registry.create_invite("doc-1", bob_pending.key_package()).unwrap();

        // Bob joins
        let mut bob_registry = DocumentRegistry::new();
        bob_registry.join_encrypted(&bob_invite, bob_pending).unwrap();

        // Alice adds Carol (this creates a commit)
        let carol_invite = alice_registry.create_invite("doc-1", carol_pending.key_package()).unwrap();

        // Bob processes the commit to stay in sync
        bob_registry.process_commit("doc-1", &carol_invite.commit).unwrap();

        // Bob's epoch should have advanced
        let bob_meta = bob_registry.get_encryption_metadata("doc-1").unwrap();
        assert!(bob_meta.epoch() > 1);
    }

    #[test]
    fn test_process_commit_on_plain_returns_error() {
        let mut registry = DocumentRegistry::new();
        registry.create("doc-1").unwrap();

        let result = registry.process_commit("doc-1", &[1, 2, 3]);
        assert!(matches!(result, Err(RegistryError::NotEncrypted(_))));
    }

    // ==================== Phase 5: Backward Compatibility ====================

    #[test]
    fn test_mixed_plain_and_encrypted_documents() {
        let mut registry = DocumentRegistry::new();

        // Create both types
        registry.create("plain-doc").unwrap();
        registry.create_encrypted("encrypted-doc", "alice").unwrap();

        // Both should be listed
        let ids = registry.list();
        assert_eq!(ids.len(), 2);

        // Plain accessors work for plain doc
        assert!(registry.get("plain-doc").is_some());
        assert!(registry.get_mut("plain-doc").is_some());

        // Encrypted accessors work for encrypted doc
        assert!(registry.get_encrypted("encrypted-doc").is_some());
        assert!(registry.get_encrypted_mut("encrypted-doc").is_some());

        // Cross-access returns None
        assert!(registry.get("encrypted-doc").is_none());
        assert!(registry.get_encrypted("plain-doc").is_none());
    }

    #[test]
    fn test_close_encrypted_returns_none() {
        let mut registry = DocumentRegistry::new();
        registry.create_encrypted("doc-1", "alice").unwrap();

        // close() should return None for encrypted docs
        let result = registry.close("doc-1");
        assert!(result.is_none());

        // Document should still exist
        assert!(registry.get_encrypted("doc-1").is_some());
    }

    #[test]
    fn test_close_any_works_for_both() {
        let mut registry = DocumentRegistry::new();

        registry.create("plain-doc").unwrap();
        registry.create_encrypted("encrypted-doc", "alice").unwrap();

        // close_any works for plain
        let plain_result = registry.close_any("plain-doc");
        assert!(matches!(plain_result, Some(DocumentVariant::Plain(_))));

        // close_any works for encrypted
        let encrypted_result = registry.close_any("encrypted-doc");
        assert!(matches!(encrypted_result, Some(DocumentVariant::Encrypted(_))));

        // Both should be gone
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_document_entry_variant_accessor() {
        let doc = CollabDocument::new("test".to_string());
        let entry = DocumentEntry::new_plain(doc);

        assert!(matches!(entry.variant(), DocumentVariant::Plain(_)));
        assert!(entry.document().is_some());
        assert!(entry.encryption_metadata().is_none());
    }
}
