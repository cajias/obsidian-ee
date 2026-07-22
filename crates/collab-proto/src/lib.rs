//! Protocol message types for collaborative editing.
//!
//! This crate defines the message types exchanged between clients and the relay server.
//!
//! ## Vault-wide synchronization
//!
//! Full vault sync is built on top of the existing [`ClientMessage::YrsUpdate`] /
//! [`ServerMessage::YrsUpdate`] mechanism. A special document (whose `doc_id`
//! equals `collab_core::MANIFEST_DOC_ID`) carries a Yrs Map that tracks every
//! file path and its alive/deleted state.
//!
//! Client/relay integration is still pending: today the manifest document is
//! standalone core infrastructure exercised by tests, and no client subscribes
//! to it on connect yet. When wired up, clients will subscribe to the manifest
//! document and react to updates by opening or closing documents in their local
//! registry.
//!
//! No new relay-level message types are required: the manifest is just another
//! Yrs document forwarded opaquely by the relay.

use serde::{Deserialize, Serialize};

/// Unique identifier for a document.
pub type DocumentId = String;

/// Unique identifier for a user.
pub type UserId = String;

/// Messages sent from client to relay server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Identify the user to the server.
    ///
    /// `token` is an optional bearer token. When the relay is configured with an
    /// authentication token (e.g. via `RELAY_AUTH_TOKEN`), a matching `token`
    /// must be supplied or the relay rejects the connection with
    /// [`ErrorCode::Unauthorized`]. When the relay has no token configured the
    /// field is ignored, so existing clients that omit it keep working.
    Identify {
        user_id: UserId,
        /// Optional bearer token authenticating the client to the relay.
        #[serde(default)]
        token: Option<String>,
    },

    /// Subscribe to document updates.
    Subscribe { doc_id: DocumentId },

    /// Unsubscribe from document updates.
    Unsubscribe { doc_id: DocumentId },

    /// Send a Yrs CRDT update (encrypted).
    YrsUpdate {
        doc_id: DocumentId,
        /// Encrypted update bytes.
        encrypted: Vec<u8>,
        /// MLS epoch for key rotation tracking.
        epoch: u64,
    },

    /// MLS handshake message (welcome, commit, etc.).
    MlsHandshake {
        doc_id: DocumentId,
        /// MLS message bytes.
        payload: Vec<u8>,
        /// Type of MLS message.
        message_type: MlsMessageType,
    },
}

/// Messages sent from relay server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Confirmation of successful identification.
    Identified { user_id: UserId },

    /// Confirmation of subscription.
    Subscribed { doc_id: DocumentId },

    /// Confirmation of unsubscription.
    Unsubscribed { doc_id: DocumentId },

    /// Forwarded Yrs update from another user.
    YrsUpdate { doc_id: DocumentId, from: UserId, encrypted: Vec<u8>, epoch: u64 },

    /// Forwarded MLS handshake message.
    MlsHandshake {
        doc_id: DocumentId,
        from: UserId,
        payload: Vec<u8>,
        message_type: MlsMessageType,
    },

    /// Error message.
    Error { code: ErrorCode, message: String },
}

/// Types of MLS handshake messages.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MlsMessageType {
    /// Key package for joining a group.
    KeyPackage,
    /// Welcome message for new members.
    Welcome,
    /// Commit message for group changes.
    Commit,
    /// Application message.
    Application,
}

/// Error codes for server responses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// User not identified.
    NotIdentified,
    /// Document not found.
    DocumentNotFound,
    /// Not subscribed to document.
    NotSubscribed,
    /// Invalid message format.
    InvalidMessage,
    /// Internal server error.
    InternalError,
    /// Authentication failed (missing or invalid token).
    Unauthorized,
    /// A resource limit was exceeded (subscriptions, document id length, etc.).
    LimitExceeded,
    /// The session was replaced by a newer connection identifying as the same
    /// user, or the connection was closed to enforce a resource limit.
    SessionReplaced,
}

/// Invite for joining a collaborative document.
///
/// Carries the full MLS material needed to reconstruct the group: the `welcome`
/// for the joining member and the `commit` that existing members must process,
/// tagged with the `epoch` at which the invite was created (for stale-invite
/// detection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    /// Document identifier.
    pub doc_id: DocumentId,
    /// MLS welcome message for the joining member.
    pub welcome: Vec<u8>,
    /// MLS commit message existing group members must process to stay in sync.
    pub commit: Vec<u8>,
    /// MLS epoch at which this invite was created.
    pub epoch: u64,
    /// Relay server URL.
    pub relay_url: String,
}
