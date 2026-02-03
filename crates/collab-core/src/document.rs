//! Yrs CRDT document wrapper for collaborative text editing.

use crate::{DocumentId, Error, Result};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, Text, TextRef, Transact};

/// A collaborative document backed by Yrs CRDT.
pub struct CollabDocument {
    /// Document identifier.
    id: DocumentId,
    /// The underlying Yrs document.
    doc: Doc,
    /// Text content reference.
    text: TextRef,
    /// State vector for tracking updates.
    state_vector: Vec<u8>,
}

impl CollabDocument {
    /// Create a new empty collaborative document.
    #[must_use]
    pub fn new(id: DocumentId) -> Self {
        let doc = Doc::new();
        let text = doc.get_or_insert_text("content");
        let state_vector = doc.transact().state_vector().encode_v1();

        Self { id, doc, text, state_vector }
    }

    /// Get the document identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Insert text at the specified index.
    pub fn insert(&mut self, index: u32, text: &str) {
        let mut txn = self.doc.transact_mut();
        self.text.insert(&mut txn, index, text);
        self.state_vector = txn.state_vector().encode_v1();
    }

    /// Delete text starting at the specified index.
    pub fn delete(&mut self, index: u32, len: u32) {
        let mut txn = self.doc.transact_mut();
        self.text.remove_range(&mut txn, index, len);
        self.state_vector = txn.state_vector().encode_v1();
    }

    /// Get the current text content.
    #[must_use]
    pub fn get_content(&self) -> String {
        let txn = self.doc.transact();
        self.text.get_string(&txn)
    }

    /// Encode the full document state as an update.
    #[must_use]
    pub fn encode_state(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    }

    /// Encode only the changes since the last sync.
    #[must_use]
    pub fn encode_update(&self) -> Vec<u8> {
        self.encode_state()
    }

    /// Apply an update from another document.
    ///
    /// # Errors
    ///
    /// Returns an error if the update cannot be applied.
    pub fn apply_update(&mut self, update: &[u8]) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        txn.apply_update(yrs::Update::decode_v1(update).map_err(|e| Error::Yrs(e.to_string()))?)
            .map_err(|e| Error::Yrs(e.to_string()))?;
        self.state_vector = txn.state_vector().encode_v1();
        drop(txn);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_document_is_empty() {
        let doc = CollabDocument::new("test-doc".into());
        assert_eq!(doc.get_content(), "");
    }

    #[test]
    fn test_insert_text() {
        let mut doc = CollabDocument::new("test-doc".into());
        doc.insert(0, "Hello");
        assert_eq!(doc.get_content(), "Hello");
    }

    #[test]
    fn test_delete_text() {
        let mut doc = CollabDocument::new("test-doc".into());
        doc.insert(0, "Hello World");
        doc.delete(5, 6); // delete " World"
        assert_eq!(doc.get_content(), "Hello");
    }

    #[test]
    fn test_sync_two_documents() {
        let mut doc_a = CollabDocument::new("test-doc".into());
        let mut doc_b = CollabDocument::new("test-doc".into());

        doc_a.insert(0, "Hello");
        let update = doc_a.encode_update();

        doc_b.apply_update(&update).unwrap();
        assert_eq!(doc_b.get_content(), "Hello");
    }

    #[test]
    fn test_concurrent_edits_merge() {
        let mut doc_a = CollabDocument::new("test-doc".into());
        let mut doc_b = CollabDocument::new("test-doc".into());

        // Both start with same state
        doc_a.insert(0, "Hello");
        let update_a = doc_a.encode_update();
        doc_b.apply_update(&update_a).unwrap();

        // Concurrent edits
        doc_a.insert(5, " World");
        doc_b.insert(5, " Rust");

        let alice_update = doc_a.encode_update();
        let bob_update = doc_b.encode_update();

        doc_a.apply_update(&bob_update).unwrap();
        doc_b.apply_update(&alice_update).unwrap();

        // Both should converge to same content
        assert_eq!(doc_a.get_content(), doc_b.get_content());
    }
}
