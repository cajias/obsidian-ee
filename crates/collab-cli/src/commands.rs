//! CLI command implementations.

use collab_core::{EncryptedDocument, MlsDocumentGroup, PendingMember};
use collab_proto::Invite;
use serde::Serialize;

/// Initialize a new collaborative document.
///
/// Creates a new encrypted document as the owner.
///
/// # Errors
///
/// Returns an error if document creation fails.
#[allow(clippy::unnecessary_wraps)] // Will have error handling in T15
pub fn init(doc_id: &str, user_id: &str) -> anyhow::Result<InitResult> {
    let _doc = EncryptedDocument::create(doc_id, user_id)?;

    // Return info about the created document
    // The invite will be created when we have the joiner's key package
    Ok(InitResult {
        doc_id: doc_id.to_string(),
        user_id: user_id.to_string(),
    })
}

/// Result of initializing a document.
#[derive(Debug, Serialize)]
pub struct InitResult {
    /// The document ID.
    pub doc_id: String,
    /// The user ID of the owner.
    pub user_id: String,
}

/// Generate a key package for joining a group.
///
/// Returns a pending member with its key package bytes.
///
/// # Errors
///
/// Returns an error if key package generation fails.
pub fn generate_key_package(user_id: &str) -> anyhow::Result<PendingMemberState> {
    let pending = MlsDocumentGroup::generate_key_package(user_id)?;
    let key_package = pending.key_package().to_vec();

    Ok(PendingMemberState {
        pending,
        key_package,
    })
}

/// State for a pending member waiting to join.
pub struct PendingMemberState {
    /// The pending member (must be kept for joining).
    pub pending: PendingMember,
    /// The serialized key package to send to the group owner.
    pub key_package: Vec<u8>,
}

/// Create an invite for a new member.
///
/// Takes the owner's document and the joiner's key package.
///
/// # Errors
///
/// Returns an error if invite creation fails.
pub fn create_invite(
    doc: &mut EncryptedDocument,
    joiner_key_package: &[u8],
) -> anyhow::Result<Invite> {
    let invite = doc.create_invite(joiner_key_package)?;

    Ok(Invite {
        doc_id: invite.doc_id,
        key_package: invite.welcome,
        relay_url: String::new(),
    })
}

/// Join an existing collaborative document.
///
/// # Errors
///
/// Returns an error if joining fails.
#[allow(clippy::missing_const_for_fn)] // Will have implementation in T15
pub fn join(_invite_path: &str, _user_id: &str) -> anyhow::Result<()> {
    // TODO: Implement full join flow in T15
    // This will need to:
    // 1. Load the pending member state from disk
    // 2. Load the invite from invite_path
    // 3. Call pending.join(&invite.welcome)
    Ok(())
}
