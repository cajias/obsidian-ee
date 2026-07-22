//! Core collaboration engine with Yrs CRDT and MLS encryption.
//!
//! This crate provides:
//! - [`CollabDocument`]: A wrapper around Yrs for CRDT-based text editing
//! - [`MlsDocumentGroup`]: MLS group operations for end-to-end encryption
//! - [`EncryptedDocument`]: Combined encrypted collaborative document
//! - [`DocumentRegistry`]: Multi-document session management with metadata tracking
//! - [`VaultManifest`]: CRDT-backed file registry for full vault synchronization
//! - [`VaultSyncConfig`]: Settings controlling which files are included in sync
//! - [`VaultSyncManager`]: Coordination layer between watcher, registry, and manifest

mod connection;
mod document;
mod encryption;
mod mls;
mod registry;
mod vault_manifest;
mod vault_sync;

pub use connection::{
    ConnectionAction, ConnectionConfig, ConnectionState, ConnectionStateMachine, RetryPolicy,
};
pub use document::CollabDocument;
pub use encryption::{EncryptedDocument, EncryptedOp, Invite};
pub use mls::{MlsDocumentGroup, PendingMember};
pub use registry::{
    DocumentEntry, DocumentMetadata, DocumentRegistry, DocumentVariant, EncryptionMetadata,
    RegistryError,
};
pub use vault_manifest::{VaultManifest, MANIFEST_DOC_ID};
pub use vault_sync::{SyncAction, SyncActionKind, VaultSyncConfig, VaultSyncManager};

/// Document identifier type.
pub type DocumentId = String;

/// User identifier type.
pub type UserId = String;

/// Error types for the collab-core crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Error from the Yrs CRDT library.
    #[error("Yrs error: {0}")]
    Yrs(String),

    /// Error from MLS operations.
    #[error("MLS error: {0}")]
    Mls(String),

    /// Error during encryption/decryption.
    #[error("Encryption error: {0}")]
    Encryption(String),

    /// Invalid state error.
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// A replayed message was detected and rejected.
    ///
    /// Returned when a previously-processed encrypted update is presented
    /// again. Replay protection is provided by the MLS secret tree, which
    /// destroys each per-sender generation key after a single use.
    #[error("Replayed message rejected")]
    Replay,
}

/// Result type for collab-core operations.
pub type Result<T> = std::result::Result<T, Error>;
