//! Document registry for managing multiple collaborative documents.

use crate::document::CollabDocument;
use crate::DocumentId;
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
    document: CollabDocument,
    metadata: DocumentMetadata,
}

impl DocumentEntry {
    /// Get a reference to the collaborative document.
    #[must_use]
    pub const fn document(&self) -> &CollabDocument {
        &self.document
    }

    /// Get a reference to the document metadata.
    #[must_use]
    pub const fn metadata(&self) -> &DocumentMetadata {
        &self.metadata
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
        let entry = DocumentEntry { document: doc, metadata: DocumentMetadata::new() };
        self.documents.insert(id.clone(), entry);
        Ok(&mut self.documents.get_mut(&id).expect("just inserted").document)
    }

    /// Get a reference to a document by ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CollabDocument> {
        self.documents.get(id).map(|entry| &entry.document)
    }

    /// Get a mutable reference to a document by ID.
    #[must_use]
    pub fn get_mut(&mut self, id: &str) -> Option<&mut CollabDocument> {
        self.documents.get_mut(id).map(|entry| &mut entry.document)
    }

    /// List all document IDs in the registry.
    #[must_use]
    pub fn list(&self) -> Vec<&DocumentId> {
        self.documents.keys().collect()
    }

    /// Close and remove a document from the registry.
    pub fn close(&mut self, id: &str) -> Option<CollabDocument> {
        self.documents.remove(id).map(|entry| entry.document)
    }

    /// Open a document with existing state.
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
        let entry = DocumentEntry { document: doc, metadata: DocumentMetadata::new() };
        self.documents.insert(id.clone(), entry);
        Ok(&mut self.documents.get_mut(&id).expect("just inserted").document)
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
}
