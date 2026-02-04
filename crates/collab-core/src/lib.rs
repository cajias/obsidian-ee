//! Core collaboration engine with Yrs CRDT and MLS encryption.
//!
//! This crate provides:
//! - `CollabDocument`: A wrapper around Yrs for CRDT-based text editing
//! - `MlsDocumentGroup`: MLS group operations for end-to-end encryption
//! - `EncryptedDocument`: Combined encrypted collaborative document

mod document;
mod encryption;
mod mls;

pub use document::CollabDocument;
pub use encryption::{EncryptedDocument, EncryptedOp, Invite};
pub use mls::{MlsDocumentGroup, PendingMember};

/// Document identifier type.
pub type DocumentId = String;

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
}

/// Result type for collab-core operations.
pub type Result<T> = std::result::Result<T, Error>;
