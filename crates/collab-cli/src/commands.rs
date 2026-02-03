//! CLI command implementations.

use collab_core::EncryptedDocument;
use collab_proto::Invite;

/// Initialize a new collaborative document.
///
/// # Errors
///
/// Returns an error if document creation fails.
pub fn init(doc_id: &str, user_id: &str) -> anyhow::Result<Invite> {
    let mut doc = EncryptedDocument::create(doc_id, user_id)?;
    let invite = doc.create_invite()?;

    Ok(Invite { doc_id: invite.doc_id, key_package: invite.welcome, relay_url: String::new() })
}

/// Join an existing collaborative document.
///
/// # Errors
///
/// Returns an error if joining fails.
#[allow(clippy::missing_const_for_fn)] // Will have implementation in T15
pub fn join(_invite_path: &str, _user_id: &str) -> anyhow::Result<()> {
    // TODO: Implement in T15
    Ok(())
}
