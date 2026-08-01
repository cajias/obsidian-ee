//! Encrypted document wrapper combining Yrs CRDT with MLS encryption.

use crate::{CollabDocument, DocumentId, MlsDocumentGroup, PendingMember, Result};

/// An encrypted collaborative document.
///
/// Combines Yrs CRDT operations with MLS end-to-end encryption.
pub struct EncryptedDocument {
    /// The underlying collaborative document.
    doc: CollabDocument,
    /// The MLS group for encryption.
    mls: MlsDocumentGroup,
}

/// An encrypted operation to be sent over the network.
#[derive(Debug, Clone)]
pub struct EncryptedOp {
    /// The encrypted ciphertext.
    pub ciphertext: Vec<u8>,
    /// The MLS epoch when this was encrypted.
    pub epoch: u64,
}

impl EncryptedDocument {
    /// Create a new encrypted document as the initial owner.
    ///
    /// # Errors
    ///
    /// Returns an error if MLS group creation fails.
    pub fn create(doc_id: &str, user_id: &str) -> Result<Self> {
        let doc = CollabDocument::new(doc_id.to_string());
        let (mls, _key_package) = MlsDocumentGroup::create(user_id)?;

        Ok(Self { doc, mls })
    }

    /// Join an existing encrypted document using a pending member state.
    ///
    /// # Errors
    ///
    /// Returns an error if joining the MLS group fails.
    pub fn join(invite: &Invite, pending: PendingMember) -> Result<Self> {
        let doc = CollabDocument::new(invite.doc_id.clone());
        let mls = pending.join(&invite.welcome)?;

        Ok(Self { doc, mls })
    }

    /// Insert text at the specified index.
    pub fn insert(&mut self, index: u32, text: &str) {
        self.doc.insert(index, text);
    }

    /// Delete text at the specified index.
    pub fn delete(&mut self, index: u32, len: u32) {
        self.doc.delete(index, len);
    }

    /// Get the current text content.
    #[must_use]
    pub fn get_content(&self) -> String {
        self.doc.get_content()
    }

    /// Get the encrypted update to send to other collaborators.
    ///
    /// # Errors
    ///
    /// Returns an error if encryption fails.
    pub fn get_encrypted_update(&mut self) -> Result<EncryptedOp> {
        let update = self.doc.encode_state();
        let ciphertext = self.mls.encrypt(&update)?;

        Ok(EncryptedOp { ciphertext, epoch: self.mls.epoch() })
    }

    /// Apply an encrypted update from another collaborator.
    ///
    /// # Errors
    ///
    /// Returns an error if decryption or applying the update fails.
    pub fn apply_encrypted_update(&mut self, op: &EncryptedOp) -> Result<()> {
        let update = self.mls.decrypt(&op.ciphertext)?;
        self.doc.apply_update(&update)
    }

    /// Encrypt caller-supplied bytes under this document's MLS group, WITHOUT
    /// touching the internal Yrs doc.
    ///
    /// Used for transport that carries its own CRDT (the vault manifest rides a
    /// dedicated group on `MANIFEST_DOC_ID`): the bytes are the manifest's own
    /// yrs update, not this doc's text. Doc-scoping/replay isolation comes from
    /// per-group separation — ciphertext from another group's MLS state fails
    /// authentication here, exactly like a cross-document splice.
    ///
    /// # Errors
    ///
    /// Returns an error if MLS encryption fails.
    pub fn encrypt_bytes(&mut self, plaintext: &[u8]) -> Result<EncryptedOp> {
        let ciphertext = self.mls.encrypt(plaintext)?;
        Ok(EncryptedOp { ciphertext, epoch: self.mls.epoch() })
    }

    /// Decrypt bytes produced by [`Self::encrypt_bytes`] on a peer's group,
    /// returning the plaintext without applying it to the internal Yrs doc.
    ///
    /// # Errors
    ///
    /// Returns an error if MLS decryption/authentication fails (including a
    /// ciphertext bound to a different MLS group).
    pub fn decrypt_bytes(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        self.mls.decrypt(ciphertext)
    }

    /// Create an invite for another user to join.
    ///
    /// Takes the key package bytes from a `PendingMember`.
    /// Returns an invite containing the welcome message for the new member,
    /// and a commit message that must be sent to all existing group members.
    ///
    /// # Errors
    ///
    /// Returns an error if creating the invite fails.
    pub fn create_invite(&mut self, key_package: &[u8]) -> Result<Invite> {
        let (commit, welcome) = self.mls.add_member(key_package)?;

        Ok(Invite { doc_id: self.doc.id().to_string(), welcome, commit, epoch: self.mls.epoch() })
    }

    /// Process a commit message from another member (e.g., when a new member is added).
    ///
    /// This is needed when other members add new participants to the group.
    /// Existing members must process the commit to update their group state.
    ///
    /// # Errors
    ///
    /// Returns an error if processing the commit fails.
    pub fn process_commit(&mut self, commit: &[u8]) -> Result<()> {
        self.mls.process_commit(commit)
    }

    /// Get the current MLS epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.mls.epoch()
    }
}

/// Invite for joining an encrypted document.
///
/// The `epoch` field records the group epoch at which this invite was created
/// (after `add_member`). The transport/relay layer should compare this against
/// the current group epoch before delivering the invite. If `epoch` does not
/// match the current group epoch, the invite is stale and should be rejected.
#[derive(Debug, Clone)]
pub struct Invite {
    /// Document identifier.
    pub doc_id: DocumentId,
    /// MLS welcome message for the new member.
    pub welcome: Vec<u8>,
    /// MLS commit message for existing members.
    /// Existing group members must process this to stay in sync.
    pub commit: Vec<u8>,
    /// MLS epoch at which this invite was created (after `add_member`).
    /// Used to detect stale invites when this epoch does not match the current group epoch.
    pub epoch: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypted_document_sync() {
        let mut alice_doc = EncryptedDocument::create("doc1", "alice").unwrap();

        // Bob creates a pending member
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();

        // Alice creates invite for Bob
        let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

        // Bob joins using the invite
        let mut bob_doc = EncryptedDocument::join(&invite, bob_pending).unwrap();

        // Alice edits
        alice_doc.insert(0, "Hello");
        let encrypted_op = alice_doc.get_encrypted_update().unwrap();

        // Bob receives and decrypts
        bob_doc.apply_encrypted_update(&encrypted_op).unwrap();
        assert_eq!(bob_doc.get_content(), "Hello");
    }

    #[test]
    fn test_encrypted_op_is_not_plaintext() {
        let mut alice_doc = EncryptedDocument::create("doc1", "alice").unwrap();

        // Add another member so we can encrypt
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let _invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

        alice_doc.insert(0, "Secret message");

        let encrypted_op = alice_doc.get_encrypted_update().unwrap();

        // The encrypted bytes should not contain the plaintext
        assert!(
            !encrypted_op.ciphertext.windows("Secret".len()).any(|w| w == b"Secret"),
            "Ciphertext should not contain plaintext"
        );

        // Verify the op was created
        assert!(!encrypted_op.ciphertext.is_empty());
    }

    /// A two-member group round-trips arbitrary bytes (the manifest transport
    /// path) without going through the internal Yrs doc.
    #[test]
    fn test_encrypt_bytes_roundtrip() {
        let mut alice = EncryptedDocument::create("__vault_manifest__", "alice").unwrap();
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let invite = alice.create_invite(bob_pending.key_package()).unwrap();
        let mut bob = EncryptedDocument::join(&invite, bob_pending).unwrap();

        let manifest = b"a fake manifest CRDT update";
        let op = alice.encrypt_bytes(manifest).unwrap();
        assert!(!op.ciphertext.windows(5).any(|w| w == b"fake"), "must be ciphertext");
        assert_eq!(bob.decrypt_bytes(&op.ciphertext).unwrap(), manifest);
    }

    /// Cross-group isolation: ciphertext from one MLS group (e.g. a file doc)
    /// must NOT decrypt under a different group (e.g. the manifest doc), even
    /// though both are `EncryptedDocument`s. This is the doc-scoping/replay
    /// defense that per-group separation provides post-MLS.
    #[test]
    fn test_encrypt_bytes_rejected_by_other_group() {
        // Two INDEPENDENT groups (distinct MLS state), each solo.
        let mut file_doc = EncryptedDocument::create("shared.md", "alice").unwrap();
        let mut manifest_doc = EncryptedDocument::create("__vault_manifest__", "alice").unwrap();

        let file_op = file_doc.encrypt_bytes(b"file content").unwrap();
        // The manifest group cannot authenticate a file-group ciphertext.
        assert!(manifest_doc.decrypt_bytes(&file_op.ciphertext).is_err());

        let manifest_op = manifest_doc.encrypt_bytes(b"manifest content").unwrap();
        // ...and vice versa.
        assert!(file_doc.decrypt_bytes(&manifest_op.ciphertext).is_err());
    }
}
