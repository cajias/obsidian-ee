//! Document registry for managing multiple collaborative documents.

use crate::document::CollabDocument;
use crate::encryption::EncryptedDocument;
use crate::{DocumentId, Invite, PendingMember};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{debug, error, info, warn};

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
    MlsError(#[from] Arc<crate::Error>),
    /// Invite is stale — the invite's epoch does not match the current group epoch.
    #[error("Stale invite for document {doc_id}: invite epoch {invite_epoch}, current epoch {current_epoch}")]
    StaleInvite {
        /// The document the invite was for.
        doc_id: DocumentId,
        /// The epoch recorded in the invite.
        invite_epoch: u64,
        /// The current group epoch.
        current_epoch: u64,
    },
    /// Internal inconsistency detected.
    #[error("Internal error: {0}")]
    InternalError(String),
}

/// Variant for documents in the registry - either plain or encrypted.
pub enum DocumentVariant {
    /// An unencrypted collaborative document.
    Plain(CollabDocument),
    /// An encrypted collaborative document with MLS encryption.
    /// Boxed to reduce size difference between variants.
    Encrypted(Box<EncryptedDocument>),
}

impl DocumentVariant {
    /// Returns true if this is an encrypted document.
    #[must_use]
    pub const fn is_encrypted(&self) -> bool {
        matches!(self, Self::Encrypted(_))
    }

    /// Returns true if this is a plain document.
    #[must_use]
    pub const fn is_plain(&self) -> bool {
        matches!(self, Self::Plain(_))
    }

    /// Returns a reference to the plain document, if this is a plain variant.
    #[must_use]
    pub const fn as_plain(&self) -> Option<&CollabDocument> {
        match self {
            Self::Plain(doc) => Some(doc),
            Self::Encrypted(_) => None,
        }
    }

    /// Returns a mutable reference to the plain document, if this is a plain variant.
    #[must_use]
    pub fn as_plain_mut(&mut self) -> Option<&mut CollabDocument> {
        match self {
            Self::Plain(doc) => Some(doc),
            Self::Encrypted(_) => None,
        }
    }

    /// Returns a reference to the encrypted document, if this is an encrypted variant.
    #[must_use]
    pub fn as_encrypted(&self) -> Option<&EncryptedDocument> {
        match self {
            Self::Encrypted(doc) => Some(doc.as_ref()),
            Self::Plain(_) => None,
        }
    }

    /// Returns a mutable reference to the encrypted document, if this is an encrypted variant.
    #[must_use]
    pub fn as_encrypted_mut(&mut self) -> Option<&mut EncryptedDocument> {
        match self {
            Self::Encrypted(doc) => Some(doc.as_mut()),
            Self::Plain(_) => None,
        }
    }
}

/// Metadata about document encryption state.
///
/// # Clone Semantics
///
/// `EncryptionMetadata` derives [`Clone`] and produces a fully independent copy.
/// All fields are simple owned types ([`String`], [`bool`], [`u64`]) — there are
/// no `Arc` references, interior mutability, or shared state. Cloning is cheap
/// (one heap allocation for the `user_id` string).
///
/// This struct holds **no cryptographic key material**. Keys live exclusively in
/// [`MlsDocumentGroup`]. Cloning `EncryptionMetadata` therefore has no security
/// implications — the clone is plain metadata.
///
/// Cloned instances track epoch independently; advancing the epoch on one copy
/// does not affect the other.
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
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::InvalidState` if user_id is empty.
    fn new(user_id: String, is_owner: bool) -> Result<Self, RegistryError> {
        if user_id.is_empty() {
            return Err(RegistryError::InvalidState(
                "User ID cannot be empty".to_string(),
            ));
        }
        Ok(Self { user_id, is_owner, epoch: 0 })
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
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::InvalidState` if the new epoch is less than
    /// the current epoch. Epochs must only increase (monotonicity).
    fn set_epoch(&mut self, epoch: u64) -> Result<(), RegistryError> {
        if epoch < self.epoch {
            return Err(RegistryError::InvalidState(format!(
                "Epoch regression: attempted to set epoch {} but current epoch is {}",
                epoch, self.epoch
            )));
        }
        self.epoch = epoch;
        Ok(())
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
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::InvalidState` if user_id is empty.
    fn new_encrypted(
        doc: EncryptedDocument,
        user_id: String,
        is_owner: bool,
    ) -> Result<Self, RegistryError> {
        Ok(Self {
            variant: DocumentVariant::Encrypted(Box::new(doc)),
            metadata: DocumentMetadata::new(),
            encryption_metadata: Some(EncryptionMetadata::new(user_id, is_owner)?),
        })
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
        info!(document_id = %id, "Creating plain document");

        if self.documents.contains_key(&id) {
            warn!(document_id = %id, "Attempted to create duplicate document");
            return Err(RegistryError::AlreadyExists(id));
        }

        let doc = CollabDocument::new(id.clone());
        let entry = DocumentEntry::new_plain(doc);
        self.documents.insert(id.clone(), entry);

        debug!(document_id = %id, "Plain document created successfully");

        let entry = self.documents.get_mut(&id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Plain(doc) => Ok(doc),
            DocumentVariant::Encrypted(_) => {
                error!(document_id = %id, "Internal error: variant mismatch after plain document operation");
                Err(RegistryError::InternalError(format!(
                    "Document '{}' has wrong variant after creation",
                    id
                )))
            }
        }
    }

    /// Get a reference to a plain document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is encrypted.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CollabDocument> {
        self.documents.get(id).and_then(|entry| match &entry.variant {
            DocumentVariant::Plain(doc) => {
                debug!(document_id = %id, "Retrieved plain document");
                Some(doc)
            }
            DocumentVariant::Encrypted(_) => {
                warn!(document_id = %id, "Attempted to access encrypted document with plain accessor");
                None
            }
        })
    }

    /// Get a mutable reference to a plain document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is encrypted.
    #[must_use]
    pub fn get_mut(&mut self, id: &str) -> Option<&mut CollabDocument> {
        self.documents.get_mut(id).and_then(|entry| match &mut entry.variant {
            DocumentVariant::Plain(doc) => {
                debug!(document_id = %id, "Retrieved mutable plain document");
                Some(doc)
            }
            DocumentVariant::Encrypted(_) => {
                warn!(document_id = %id, "Attempted to access encrypted document with plain mutable accessor");
                None
            }
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
        debug!(document_id = %id, "Attempting to close plain document");

        // Check if it's a plain document first
        let is_plain = self.documents.get(id).is_some_and(|entry| {
            matches!(entry.variant, DocumentVariant::Plain(_))
        });

        if is_plain {
            info!(document_id = %id, "Closing plain document");
            self.documents.remove(id).and_then(|entry| match entry.variant {
                DocumentVariant::Plain(doc) => Some(doc),
                DocumentVariant::Encrypted(_) => None,
            })
        } else {
            warn!(document_id = %id, "Cannot close: document not found or is encrypted");
            None
        }
    }

    /// Close and remove any document from the registry.
    ///
    /// Returns the document variant, or `None` if the document doesn't exist.
    pub fn close_any(&mut self, id: &str) -> Option<DocumentVariant> {
        info!(document_id = %id, "Closing document (any type)");

        let result = self.documents.remove(id).map(|entry| entry.variant);

        if result.is_some() {
            debug!(document_id = %id, "Document closed successfully");
        } else {
            warn!(document_id = %id, "Document not found");
        }

        result
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
        info!(document_id = %id, state_size = state.len(), "Opening plain document with existing state");

        if self.documents.contains_key(&id) {
            warn!(document_id = %id, "Attempted to open duplicate document");
            return Err(RegistryError::AlreadyExists(id));
        }

        let mut doc = CollabDocument::new(id.clone());
        doc.apply_update(state).map_err(|e| {
            error!(document_id = %id, error = %e, "Failed to apply document state");
            RegistryError::InvalidState(format!(
                "Failed to restore document '{}': {}. State may be corrupted or incompatible.",
                id, e
            ))
        })?;

        let entry = DocumentEntry::new_plain(doc);
        self.documents.insert(id.clone(), entry);

        debug!(document_id = %id, "Plain document opened successfully");

        let entry = self.documents.get_mut(&id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Plain(doc) => Ok(doc),
            DocumentVariant::Encrypted(_) => {
                error!(document_id = %id, "Internal error: variant mismatch after plain document operation");
                Err(RegistryError::InternalError(format!(
                    "Document '{}' has wrong variant after creation",
                    id
                )))
            }
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
        debug!(document_id = %id, key = %key, "Setting custom metadata");

        let entry = self.documents.get_mut(id).ok_or_else(|| {
            warn!(document_id = %id, "Cannot set metadata: document not found");
            RegistryError::NotFound(id.to_string())
        })?;

        entry.metadata.custom.insert(key.to_string(), value.to_string());
        debug!(document_id = %id, key = %key, "Custom metadata set successfully");
        Ok(())
    }

    /// Update the `last_modified` timestamp for a document.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::NotFound` if the document does not exist.
    pub fn touch(&mut self, id: &str) -> Result<(), RegistryError> {
        debug!(document_id = %id, "Updating last_modified timestamp");

        let entry = self.documents.get_mut(id).ok_or_else(|| {
            warn!(document_id = %id, "Cannot touch: document not found");
            RegistryError::NotFound(id.to_string())
        })?;

        entry.metadata.touch();
        debug!(document_id = %id, "Timestamp updated successfully");
        Ok(())
    }

    // ==================== Encrypted Document Methods ====================

    /// Create a new encrypted document as the owner.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::AlreadyExists` if a document with the given ID already exists.
    /// Returns `RegistryError::InvalidState` if `user_id` is empty.
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
        info!(document_id = %id, user_id = %user_id, "Creating encrypted document");

        if self.documents.contains_key(&id) {
            warn!(document_id = %id, "Attempted to create duplicate encrypted document");
            return Err(RegistryError::AlreadyExists(id));
        }

        let doc = EncryptedDocument::create(&id, user_id).map_err(|e| {
            error!(document_id = %id, user_id = %user_id, error = ?e, "MLS group creation failed");
            RegistryError::MlsError(Arc::new(e))
        })?;

        let mut entry = DocumentEntry::new_encrypted(doc, user_id.to_string(), true)?;

        // Set the epoch from the document
        let meta = entry
            .encryption_metadata
            .as_mut()
            .ok_or_else(|| {
                error!(document_id = %id, "Internal error: encrypted document missing encryption metadata");
                RegistryError::InternalError(
                    "Encrypted document missing encryption metadata".to_string(),
                )
            })?;
        let doc = match &entry.variant {
            DocumentVariant::Encrypted(d) => d,
            DocumentVariant::Plain(_) => {
                error!("Internal error: plain variant after encrypted document operation");
                return Err(RegistryError::InternalError(
                    "Document has plain variant after encrypted operation".to_string()
                ));
            }
        };
        meta.set_epoch(doc.epoch())?;

        self.documents.insert(id.clone(), entry);

        debug!(document_id = %id, user_id = %user_id, epoch = 0, is_owner = true,
               "Encrypted document created successfully");

        let entry = self.documents.get_mut(&id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => Ok(doc.as_mut()),
            DocumentVariant::Plain(_) => {
                error!("Internal error: variant mismatch after encrypted document operation");
                Err(RegistryError::InternalError(
                    "Document has wrong variant after encrypted operation".to_string(),
                ))
            }
        }
    }

    /// Join an existing encrypted document using an invite.
    ///
    /// `current_group_epoch` is the latest known group epoch, typically provided
    /// by the relay/transport layer. If it differs from the invite's epoch, the
    /// invite is considered stale and the join is rejected.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::StaleInvite` if the invite's epoch does not match
    /// `current_group_epoch`.
    /// Returns `RegistryError::AlreadyExists` if a document with the given ID already exists.
    /// Returns `RegistryError::InvalidState` if the pending member's user ID is empty.
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
        current_group_epoch: u64,
    ) -> Result<&mut EncryptedDocument, RegistryError> {
        let doc_id = invite.doc_id.clone();
        let user_id = pending.user_id().to_string();

        info!(document_id = %doc_id, user_id = %user_id,
              invite_epoch = invite.epoch, current_epoch = current_group_epoch,
              "Joining encrypted document via invite");

        // Reject stale invites: the invite's epoch must match the current group epoch
        if invite.epoch != current_group_epoch {
            warn!(document_id = %doc_id, invite_epoch = invite.epoch,
                  current_epoch = current_group_epoch,
                  "Rejecting stale invite: epoch mismatch");
            return Err(RegistryError::StaleInvite {
                doc_id,
                invite_epoch: invite.epoch,
                current_epoch: current_group_epoch,
            });
        }

        if self.documents.contains_key(&doc_id) {
            warn!(document_id = %doc_id, "Attempted to join duplicate encrypted document");
            return Err(RegistryError::AlreadyExists(doc_id));
        }

        let doc = EncryptedDocument::join(invite, pending).map_err(|e| {
            error!(document_id = %doc_id, user_id = %user_id, error = ?e, "MLS group join failed");
            RegistryError::MlsError(Arc::new(e))
        })?;

        let epoch = doc.epoch();
        let mut entry = DocumentEntry::new_encrypted(doc, user_id.clone(), false)?;

        // Set the epoch from the document
        let meta = entry
            .encryption_metadata
            .as_mut()
            .ok_or_else(|| {
                error!(document_id = %doc_id, "Internal error: encrypted document missing encryption metadata");
                RegistryError::InternalError(
                    "Encrypted document missing encryption metadata".to_string(),
                )
            })?;
        let doc_ref = match &entry.variant {
            DocumentVariant::Encrypted(d) => d,
            DocumentVariant::Plain(_) => {
                error!("Internal error: plain variant after encrypted document operation");
                return Err(RegistryError::InternalError(
                    "Document has plain variant after encrypted operation".to_string()
                ));
            }
        };
        meta.set_epoch(doc_ref.epoch())?;

        self.documents.insert(doc_id.clone(), entry);

        debug!(document_id = %doc_id, user_id = %user_id, epoch = %epoch, is_owner = false,
               "Successfully joined encrypted document");

        let entry = self.documents.get_mut(&doc_id).expect("just inserted");
        match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => Ok(doc.as_mut()),
            DocumentVariant::Plain(_) => {
                error!("Internal error: variant mismatch after encrypted document operation");
                Err(RegistryError::InternalError(
                    "Document has wrong variant after encrypted operation".to_string(),
                ))
            }
        }
    }

    /// Get a reference to an encrypted document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is plain.
    #[must_use]
    pub fn get_encrypted(&self, id: &str) -> Option<&EncryptedDocument> {
        self.documents.get(id).and_then(|entry| match &entry.variant {
            DocumentVariant::Encrypted(doc) => {
                debug!(document_id = %id, "Retrieved encrypted document");
                Some(doc.as_ref())
            }
            DocumentVariant::Plain(_) => {
                warn!(document_id = %id, "Attempted to access plain document with encrypted accessor");
                None
            }
        })
    }

    /// Get a mutable reference to an encrypted document by ID.
    ///
    /// Returns `None` if the document doesn't exist or is plain.
    #[must_use]
    pub fn get_encrypted_mut(&mut self, id: &str) -> Option<&mut EncryptedDocument> {
        self.documents.get_mut(id).and_then(|entry| match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => {
                debug!(document_id = %id, "Retrieved mutable encrypted document");
                Some(doc.as_mut())
            }
            DocumentVariant::Plain(_) => {
                warn!(document_id = %id, "Attempted to access plain document with encrypted mutable accessor");
                None
            }
        })
    }

    /// Create an invite for another user to join an encrypted document.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::NotFound` if the document doesn't exist.
    /// Returns `RegistryError::InvalidState` if the key package is empty.
    /// Returns `RegistryError::NotEncrypted` if the document is not encrypted.
    /// Returns `RegistryError::MlsError` if invite creation fails.
    pub fn create_invite(
        &mut self,
        id: &str,
        key_package: &[u8],
    ) -> Result<Invite, RegistryError> {
        info!(document_id = %id, key_package_len = key_package.len(), "Creating invite for new member");

        if key_package.is_empty() {
            warn!(document_id = %id, "Empty key package provided");
            return Err(RegistryError::InvalidState(
                "Key package cannot be empty".to_string(),
            ));
        }

        let entry = self.documents.get_mut(id).ok_or_else(|| {
            warn!(document_id = %id, "Cannot create invite: document not found");
            RegistryError::NotFound(id.to_string())
        })?;

        let doc = match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => doc.as_mut(),
            DocumentVariant::Plain(_) => {
                error!(document_id = %id, "Cannot create invite: document is not encrypted");
                return Err(RegistryError::NotEncrypted(id.to_string()));
            }
        };

        let old_epoch = doc.epoch();
        let invite = doc.create_invite(key_package).map_err(|e| {
            error!(document_id = %id, error = ?e, "Failed to create MLS invite");
            RegistryError::MlsError(Arc::new(e))
        })?;

        // Use the invite's epoch directly (authoritative post-add_member epoch)
        let new_epoch = invite.epoch;

        // Update epoch in metadata after adding member
        let meta = entry
            .encryption_metadata
            .as_mut()
            .ok_or_else(|| {
                error!(document_id = %id, "Internal error: encrypted document missing encryption metadata");
                RegistryError::InternalError(
                    "Encrypted document missing encryption metadata".to_string(),
                )
            })?;
        meta.set_epoch(new_epoch)?;

        info!(document_id = %id, old_epoch = %old_epoch, new_epoch = %new_epoch,
              "Invite created successfully, epoch advanced");

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
        info!(document_id = %id, commit_len = commit.len(), "Processing commit for encrypted document");

        let entry = self.documents.get_mut(id).ok_or_else(|| {
            warn!(document_id = %id, "Cannot process commit: document not found");
            RegistryError::NotFound(id.to_string())
        })?;

        let doc = match &mut entry.variant {
            DocumentVariant::Encrypted(doc) => doc.as_mut(),
            DocumentVariant::Plain(_) => {
                warn!(document_id = %id, "Attempted to process commit on plain document");
                return Err(RegistryError::NotEncrypted(id.to_string()));
            }
        };

        let old_epoch = doc.epoch();

        doc.process_commit(commit).map_err(|e| {
            error!(document_id = %id, error = ?e, old_epoch = %old_epoch,
                   "Failed to process commit");
            RegistryError::MlsError(Arc::new(e))
        })?;

        let new_epoch = doc.epoch();

        // Update epoch in metadata after processing commit
        let meta = entry
            .encryption_metadata
            .as_mut()
            .ok_or_else(|| {
                error!(document_id = %id, "Internal error: encrypted document missing encryption metadata");
                RegistryError::InternalError(
                    "Encrypted document missing encryption metadata".to_string(),
                )
            })?;
        meta.set_epoch(new_epoch)?;

        debug!(document_id = %id, old_epoch = %old_epoch, new_epoch = %new_epoch,
               "Commit processed successfully, epoch updated");

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
        let err = RegistryError::MlsError(Arc::new(crate::Error::Mls(
            "failed to encrypt".to_string(),
        )));
        assert!(err.to_string().contains("MLS"));
        assert!(err.to_string().contains("failed to encrypt"));
    }

    #[test]
    fn test_encryption_metadata_creation() {
        let meta = EncryptionMetadata::new("alice".to_string(), true).unwrap();
        assert_eq!(meta.user_id(), "alice");
        assert!(meta.is_owner());
        assert_eq!(meta.epoch(), 0);
    }

    #[test]
    fn test_encryption_metadata_rejects_empty_user_id() {
        let result = EncryptionMetadata::new("".to_string(), true);
        assert!(matches!(result, Err(RegistryError::InvalidState(_))));
    }

    #[test]
    fn test_encryption_metadata_epoch_update() {
        let mut meta = EncryptionMetadata::new("bob".to_string(), false).unwrap();
        assert_eq!(meta.epoch(), 0);
        meta.set_epoch(5).unwrap();
        assert_eq!(meta.epoch(), 5);
    }

    #[test]
    fn test_encryption_metadata_epoch_monotonicity() {
        let mut meta = EncryptionMetadata::new("alice".to_string(), true).unwrap();
        assert_eq!(meta.epoch(), 0);

        // Epoch can increase
        meta.set_epoch(1).unwrap();
        assert_eq!(meta.epoch(), 1);

        meta.set_epoch(5).unwrap();
        assert_eq!(meta.epoch(), 5);

        // Epoch can stay the same
        meta.set_epoch(5).unwrap();
        assert_eq!(meta.epoch(), 5);

        // Epoch regression is rejected
        let result = meta.set_epoch(3);
        assert!(
            matches!(result, Err(RegistryError::InvalidState(_))),
            "Epoch regression should be rejected"
        );
        // Epoch should remain unchanged after rejected regression
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
        let result = bob_registry.join_encrypted(&invite, bob_pending, invite.epoch);
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
        bob_registry.join_encrypted(&invite, bob_pending, invite.epoch).unwrap();

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
        bob_registry.join_encrypted(&invite, bob_pending, invite.epoch).unwrap();

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
        bob_registry.join_encrypted(&bob_invite, bob_pending, bob_invite.epoch).unwrap();

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

    #[test]
    fn test_document_variant_convenience_methods() {
        // Test plain variant
        let plain_doc = CollabDocument::new("test".to_string());
        let plain_variant = DocumentVariant::Plain(plain_doc);

        assert!(plain_variant.is_plain());
        assert!(!plain_variant.is_encrypted());
        assert!(plain_variant.as_plain().is_some());
        assert!(plain_variant.as_encrypted().is_none());

        // Test encrypted variant
        let encrypted_doc = EncryptedDocument::create("test", "alice").unwrap();
        let mut encrypted_variant = DocumentVariant::Encrypted(Box::new(encrypted_doc));

        assert!(encrypted_variant.is_encrypted());
        assert!(!encrypted_variant.is_plain());
        assert!(encrypted_variant.as_encrypted().is_some());
        assert!(encrypted_variant.as_plain().is_none());

        // Test mutable accessors
        assert!(encrypted_variant.as_encrypted_mut().is_some());
        assert!(encrypted_variant.as_plain_mut().is_none());
    }
}
