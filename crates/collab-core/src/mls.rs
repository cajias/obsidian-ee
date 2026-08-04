//! MLS (Messaging Layer Security) group operations for E2E encryption.

use crate::{Error, Result};
use openmls::framing::errors::{MessageDecryptionError, SecretTreeError};
use openmls::prelude::tls_codec::{Deserialize, Serialize};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

/// The ciphersuite to use for MLS operations.
const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// MLS exporter label (RFC 9420 §8.5) that domain-separates the per-epoch secret
/// from which the subscribe-capability signing key is derived (issue #29).
///
/// This is the *exporter* label, distinct from `collab-proto`'s `LABEL_MSG` which
/// domain-separates the signed *message*. They intentionally share the
/// `obsidian-ee/subscribe-capability/v1` string but sit on opposite sides of the
/// derive/sign boundary — do not conflate the two.
const SUBSCRIBE_EXPORTER_LABEL: &str = "obsidian-ee/subscribe-capability/v1";

/// Maps a `process_message` failure to a crate [`Error`], distinguishing a
/// replayed message from other decryption failures.
///
/// The MLS secret tree assigns each application message a per-sender
/// generation key and destroys it after a single use. Re-presenting the same
/// message therefore surfaces as `SecretReuseError`, which is mapped to
/// [`Error::Replay`]. A message whose generation has aged out of the retention
/// window (`TooDistantInThePast`) is a different error and remains a generic
/// [`Error::Mls`].
fn map_process_message_error<S: std::fmt::Debug>(err: &ProcessMessageError<S>) -> Error {
    if matches!(
        err,
        ProcessMessageError::ValidationError(ValidationError::UnableToDecrypt(
            MessageDecryptionError::SecretTreeError(SecretTreeError::SecretReuseError)
        ))
    ) {
        Error::Replay
    } else {
        Error::Mls(format!("Failed to process message: {err:?}"))
    }
}

/// An MLS group for a document, managing encryption keys and group membership.
pub struct MlsDocumentGroup {
    /// User identifier for this group member.
    user_id: String,
    /// User identifier of the group creator (the owner). Set to `user_id` at
    /// `create`; learned from leaf 0 at `join`. The owner is the only member
    /// allowed to remove others (issue #31).
    owner_id: String,
    /// The MLS group.
    group: MlsGroup,
    /// The crypto provider.
    crypto: OpenMlsRustCrypto,
    /// The signature key pair.
    signature_keys: SignatureKeyPair,
    /// The credential with key.
    _credential_with_key: CredentialWithKey,
}

/// Extract the UTF-8 `user_id` from a member's `BasicCredential`.
///
/// Every credential in this project is a [`BasicCredential`] wrapping the
/// `user_id` bytes (see `create` / `PendingMember::new`). Anything else is a
/// protocol violation and surfaces as an error rather than a panic.
fn credential_identity(credential: &Credential) -> Result<String> {
    let basic = BasicCredential::try_from(credential.clone())
        .map_err(|e| Error::Mls(format!("Expected a basic credential: {e:?}")))?;
    String::from_utf8(basic.identity().to_vec())
        .map_err(|e| Error::Mls(format!("Credential identity is not valid UTF-8: {e:?}")))
}

/// A pending member waiting to join a group.
///
/// This struct holds the crypto state needed to process a welcome message.
pub struct PendingMember {
    /// User identifier.
    user_id: String,
    /// The crypto provider with stored keys.
    crypto: OpenMlsRustCrypto,
    /// The signature key pair.
    signature_keys: SignatureKeyPair,
    /// The credential with key.
    credential_with_key: CredentialWithKey,
    /// Serialized key package.
    key_package_bytes: Vec<u8>,
}

impl PendingMember {
    /// Create a new pending member with a key package.
    ///
    /// # Errors
    ///
    /// Returns an error if key generation fails.
    pub fn new(user_id: &str) -> Result<Self> {
        let crypto = OpenMlsRustCrypto::default();

        // Generate signature keys
        let signature_keys = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm())
            .map_err(|e| Error::Mls(format!("Failed to generate signature keys: {e:?}")))?;
        signature_keys
            .store(crypto.storage())
            .map_err(|e| Error::Mls(format!("Failed to store signature keys: {e:?}")))?;

        // Create basic credential
        let credential = BasicCredential::new(user_id.as_bytes().to_vec());
        let credential_with_key = CredentialWithKey {
            credential: credential.into(),
            signature_key: signature_keys.public().into(),
        };

        // Create key package
        let bundle = KeyPackage::builder()
            .build(CIPHERSUITE, &crypto, &signature_keys, credential_with_key.clone())
            .map_err(|e| Error::Mls(format!("Failed to create key package: {e:?}")))?;

        let key_package_bytes = bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(|e| Error::Mls(format!("Failed to serialize key package: {e:?}")))?;

        Ok(Self {
            user_id: user_id.to_string(),
            crypto,
            signature_keys,
            credential_with_key,
            key_package_bytes,
        })
    }

    /// Get the user ID for this pending member.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Get the serialized key package to send to the group owner.
    #[must_use]
    pub fn key_package(&self) -> &[u8] {
        &self.key_package_bytes
    }

    /// Join an existing group using a welcome message.
    ///
    /// Consumes this pending member and returns a full group member.
    ///
    /// # Errors
    ///
    /// Returns an error if joining fails.
    pub fn join(self, welcome_bytes: &[u8]) -> Result<MlsDocumentGroup> {
        // Deserialize the welcome message
        let mls_message = MlsMessageIn::tls_deserialize_exact(welcome_bytes)
            .map_err(|e| Error::Mls(format!("Failed to deserialize welcome: {e:?}")))?;

        let MlsMessageBodyIn::Welcome(welcome) = mls_message.extract() else {
            return Err(Error::Mls("Expected welcome message".to_string()));
        };

        // Join configuration
        let join_config = MlsGroupJoinConfig::builder().use_ratchet_tree_extension(true).build();

        // Join the group
        let group = StagedWelcome::new_from_welcome(&self.crypto, &join_config, welcome, None)
            .map_err(|e| Error::Mls(format!("Failed to stage welcome: {e:?}")))?
            .into_group(&self.crypto)
            .map_err(|e| Error::Mls(format!("Failed to join group: {e:?}")))?;

        // The group creator (owner) always holds leaf index 0 in openmls, and
        // owner-removal is out of scope for #31, so leaf 0 == owner for the
        // whole supported lifecycle. Reading it here avoids threading owner_id
        // through the Invite/Welcome proto. (See design doc for the choice.)
        let owner_id = group
            .members()
            .find(|m| m.index == LeafNodeIndex::new(0))
            .ok_or_else(|| Error::Mls("Group has no leaf-0 member (owner)".to_string()))
            .and_then(|m| credential_identity(&m.credential))?;

        Ok(MlsDocumentGroup {
            user_id: self.user_id,
            owner_id,
            group,
            crypto: self.crypto,
            signature_keys: self.signature_keys,
            _credential_with_key: self.credential_with_key,
        })
    }
}

impl MlsDocumentGroup {
    /// Create a new MLS group as the initial member.
    ///
    /// Returns the group and a serialized key package for sharing.
    ///
    /// # Errors
    ///
    /// Returns an error if group creation fails.
    pub fn create(user_id: &str) -> Result<(Self, Vec<u8>)> {
        let crypto = OpenMlsRustCrypto::default();

        // Generate signature keys
        let signature_keys = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm())
            .map_err(|e| Error::Mls(format!("Failed to generate signature keys: {e:?}")))?;
        signature_keys
            .store(crypto.storage())
            .map_err(|e| Error::Mls(format!("Failed to store signature keys: {e:?}")))?;

        // Create basic credential
        let credential = BasicCredential::new(user_id.as_bytes().to_vec());
        let credential_with_key = CredentialWithKey {
            credential: credential.into(),
            signature_key: signature_keys.public().into(),
        };

        // Create MLS group configuration
        let group_config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .build();

        // Create the group
        let group =
            MlsGroup::new(&crypto, &signature_keys, &group_config, credential_with_key.clone())
                .map_err(|e| Error::Mls(format!("Failed to create MLS group: {e:?}")))?;

        // Generate a key package for potential future use
        let key_package = Self::create_key_package(&crypto, &signature_keys, &credential_with_key)?;
        let key_package_bytes = key_package
            .tls_serialize_detached()
            .map_err(|e| Error::Mls(format!("Failed to serialize key package: {e:?}")))?;

        Ok((
            Self {
                user_id: user_id.to_string(),
                owner_id: user_id.to_string(),
                group,
                crypto,
                signature_keys,
                _credential_with_key: credential_with_key,
            },
            key_package_bytes,
        ))
    }

    /// Generate a key package for a user to join a group.
    ///
    /// This creates a `PendingMember` that can later be used to join.
    ///
    /// # Errors
    ///
    /// Returns an error if key package generation fails.
    pub fn generate_key_package(user_id: &str) -> Result<PendingMember> {
        PendingMember::new(user_id)
    }

    /// Create a key package for a user.
    fn create_key_package(
        crypto: &OpenMlsRustCrypto,
        signature_keys: &SignatureKeyPair,
        credential_with_key: &CredentialWithKey,
    ) -> Result<KeyPackage> {
        let bundle = KeyPackage::builder()
            .build(CIPHERSUITE, crypto, signature_keys, credential_with_key.clone())
            .map_err(|e| Error::Mls(format!("Failed to create key package: {e:?}")))?;
        Ok(bundle.key_package().clone())
    }

    /// Get the current epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    /// Get the user ID.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Add a new member to the group.
    ///
    /// # Errors
    ///
    /// Returns an error if adding the member fails.
    pub fn add_member(&mut self, key_package_bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        // Deserialize the key package
        let key_package_in = KeyPackageIn::tls_deserialize_exact(key_package_bytes)
            .map_err(|e| Error::Mls(format!("Failed to deserialize key package: {e:?}")))?;

        let key_package = key_package_in
            .validate(self.crypto.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| Error::Mls(format!("Failed to validate key package: {e:?}")))?;

        // Add the member
        let (commit, welcome, _group_info) = self
            .group
            .add_members(&self.crypto, &self.signature_keys, &[key_package])
            .map_err(|e| Error::Mls(format!("Failed to add member: {e:?}")))?;

        // Merge the pending commit
        self.group
            .merge_pending_commit(&self.crypto)
            .map_err(|e| Error::Mls(format!("Failed to merge commit: {e:?}")))?;

        // Serialize the commit and welcome
        let commit_bytes = commit
            .tls_serialize_detached()
            .map_err(|e| Error::Mls(format!("Failed to serialize commit: {e:?}")))?;

        let welcome_bytes = welcome
            .tls_serialize_detached()
            .map_err(|e| Error::Mls(format!("Failed to serialize welcome: {e:?}")))?;

        Ok((commit_bytes, welcome_bytes))
    }

    /// True iff this member created the group (is the owner).
    #[must_use]
    pub fn is_owner(&self) -> bool {
        self.user_id == self.owner_id
    }

    /// Find the leaf index of the member whose credential identity is `user_id`,
    /// or `None` if no current member matches.
    ///
    /// # Errors
    ///
    /// Returns an error if any member's credential is not a valid UTF-8
    /// `BasicCredential`.
    fn find_member_leaf(&self, user_id: &str) -> Result<Option<LeafNodeIndex>> {
        // Materialize (identity, leaf) pairs first so a malformed credential
        // surfaces as an error, then locate the match. Flat iterator style keeps
        // nesting within the linter's limit.
        let members = self
            .group
            .members()
            .map(|m| credential_identity(&m.credential).map(|id| (id, m.index)))
            .collect::<Result<Vec<_>>>()?;
        Ok(members.into_iter().find(|(id, _)| id == user_id).map(|(_, index)| index))
    }

    /// Owner-only: remove the member whose credential identity == `member_user_id`.
    ///
    /// Advances the epoch and rekeys the group, cutting the removed member off
    /// from subsequent messages (issue #31). Returns the serialized commit that
    /// existing members must `process_commit`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Mls`] if this member is not the owner, if `member_user_id`
    /// is the owner (the owner cannot remove themselves — future work), if no
    /// current member has that identity, or if the MLS operation fails.
    pub fn remove_member(&mut self, member_user_id: &str) -> Result<Vec<u8>> {
        // Mint-side policy: only the owner's client may remove a member.
        if !self.is_owner() {
            return Err(Error::Mls("only the group owner may remove members".to_string()));
        }
        // The owner cannot remove themselves (self-leave / succession is future work).
        if member_user_id == self.owner_id {
            return Err(Error::Mls("the owner cannot be removed".to_string()));
        }

        let leaf = self
            .find_member_leaf(member_user_id)?
            .ok_or_else(|| Error::Mls(format!("{member_user_id} is not a member")))?;

        let (commit, _welcome, _group_info) = self
            .group
            .remove_members(&self.crypto, &self.signature_keys, &[leaf])
            .map_err(|e| Error::Mls(format!("Failed to remove member: {e:?}")))?;

        self.group
            .merge_pending_commit(&self.crypto)
            .map_err(|e| Error::Mls(format!("Failed to merge removal commit: {e:?}")))?;

        commit
            .tls_serialize_detached()
            .map_err(|e| Error::Mls(format!("Failed to serialize commit: {e:?}")))
    }

    /// Encrypt a message for the group.
    ///
    /// # Errors
    ///
    /// Returns an error if encryption fails.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let ciphertext = self
            .group
            .create_message(&self.crypto, &self.signature_keys, plaintext)
            .map_err(|e| Error::Mls(format!("Failed to encrypt message: {e:?}")))?;

        ciphertext
            .tls_serialize_detached()
            .map_err(|e| Error::Mls(format!("Failed to serialize ciphertext: {e:?}")))
    }

    /// Decrypt a message from the group.
    ///
    /// # Errors
    ///
    /// Returns an error if decryption fails.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let message = MlsMessageIn::tls_deserialize_exact(ciphertext)
            .map_err(|e| Error::Mls(format!("Failed to deserialize ciphertext: {e:?}")))?;

        let processed = self
            .group
            .process_message(
                &self.crypto,
                message
                    .try_into_protocol_message()
                    .map_err(|_| Error::Mls("Expected protocol message".to_string()))?,
            )
            .map_err(|e| map_process_message_error(&e))?;

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app_msg) => Ok(app_msg.into_bytes()),
            ProcessedMessageContent::ProposalMessage(_) => {
                Err(Error::Mls("Unexpected proposal message".to_string()))
            }
            ProcessedMessageContent::StagedCommitMessage(_) => {
                Err(Error::Mls("Unexpected commit message".to_string()))
            }
            ProcessedMessageContent::ExternalJoinProposalMessage(_) => {
                Err(Error::Mls("Unexpected external join proposal".to_string()))
            }
        }
    }

    /// Process a commit message from another member (e.g., when a new member is added).
    ///
    /// This is needed when other members add new participants to the group.
    /// The committer sends the commit message to all existing members so they
    /// can update their group state and epoch.
    ///
    /// # Errors
    ///
    /// Returns an error if processing the commit fails.
    pub fn process_commit(&mut self, commit_bytes: &[u8]) -> Result<()> {
        let message = MlsMessageIn::tls_deserialize_exact(commit_bytes)
            .map_err(|e| Error::Mls(format!("Failed to deserialize commit: {e:?}")))?;

        let processed = self
            .group
            .process_message(
                &self.crypto,
                message
                    .try_into_protocol_message()
                    .map_err(|_| Error::Mls("Expected protocol message".to_string()))?,
            )
            .map_err(|e| Error::Mls(format!("Failed to process commit: {e:?}")))?;

        // Capture the committer identity BEFORE into_content() consumes the
        // processed message — needed to enforce the removal policy below.
        let committer = credential_identity(processed.credential())?;

        let ProcessedMessageContent::StagedCommitMessage(staged_commit) = processed.into_content()
        else {
            return Err(Error::Mls("Expected commit message".to_string()));
        };

        // Receive-side policy (issue #31, the enforceable half): a commit that
        // removes any member is only honored if its committer is the group owner.
        // MLS itself does not enforce authorization, so a non-owner's Remove
        // commit is REJECTED here — not merged.
        let is_removal = staged_commit.remove_proposals().next().is_some();
        if is_removal && committer != self.owner_id {
            return Err(Error::Mls("removal commit from a non-owner is rejected".to_string()));
        }
        // The owner (leaf 0) is the anchor of the whole who-may-remove-whom
        // policy — owner_id is derived from leaf 0 at join. A commit that evicts
        // leaf 0 would orphan that identity, so it is REJECTED even if its
        // committer passed the owner check above (BasicCredential identities are
        // not unique, so a member can commit under the owner's identity string).
        // owner-succession / self-leave remain future work.
        if is_removal
            && staged_commit.remove_proposals().any(|p| p.remove_proposal().removed().u32() == 0)
        {
            return Err(Error::Mls("commit removes the group owner; rejected".to_string()));
        }

        // Merge the staged commit to update our group state.
        self.group
            .merge_staged_commit(&self.crypto, *staged_commit)
            .map_err(|e| Error::Mls(format!("Failed to merge staged commit: {e:?}")))
    }

    /// Derive the per-epoch `Ed25519` signing key for subscribe capabilities.
    ///
    /// Every current group member derives the SAME key from the MLS exporter
    /// secret (RFC 9420 §8.5); a non-member cannot. The secret — and therefore
    /// the key — changes every epoch, so a capability minted at an old epoch no
    /// longer matches the rotated anchor.
    fn derive_subscribe_keypair(&self) -> Result<ed25519_dalek::SigningKey> {
        let seed = self
            .group
            .export_secret(self.crypto.crypto(), SUBSCRIBE_EXPORTER_LABEL, b"", 32)
            .map_err(|e| Error::Mls(format!("Failed to export subscribe secret: {e:?}")))?;
        let seed32: [u8; 32] = seed
            .try_into()
            .map_err(|_| Error::Mls("Exported subscribe secret was not 32 bytes".to_string()))?;
        Ok(ed25519_dalek::SigningKey::from_bytes(&seed32))
    }

    /// `Ed25519` verifying key for THIS epoch, to register as the relay anchor.
    ///
    /// # Errors
    ///
    /// Returns an error if exporting the MLS secret fails.
    pub fn subscribe_verifying_key(&self) -> Result<[u8; 32]> {
        Ok(self.derive_subscribe_keypair()?.verifying_key().to_bytes())
    }

    /// Mint a capability naming `user_id` for `doc_id` at the current epoch, valid
    /// until `now + ttl`.
    ///
    /// `user_id` binds the capability to WHO may present it: it must be the same
    /// identity the minting member uses to `Identify` at the relay, because the
    /// relay verifies `cap.user_id` against the presenting connection's identified
    /// user id (a relay-layer identity, distinct from the MLS credential). This
    /// stops a capability minted for one member being replayed by another.
    ///
    /// `now_unix` is injected so the caller controls the clock (collab-core stays
    /// clock-agnostic for wasm and deterministic in tests).
    ///
    /// # Errors
    ///
    /// Returns an error if exporting the MLS secret fails.
    pub fn mint_subscribe_capability(
        &self,
        user_id: &str,
        doc_id: &str,
        now_unix: u64,
        ttl_secs: u64,
    ) -> Result<collab_proto::SubscribeCapability> {
        let keypair = self.derive_subscribe_keypair()?;
        let expiry = now_unix.saturating_add(ttl_secs);
        Ok(collab_proto::sign_subscribe_capability(&keypair, user_id, doc_id, self.epoch(), expiry))
    }

    /// Sign a `RegisterDocKey` self-proof for `doc_id` at the current epoch.
    ///
    /// The relay verifies this proof under [`Self::subscribe_verifying_key`] to
    /// confirm the registrant holds the current epoch's key (is a member) before
    /// anchoring the doc. Requiring the *current* epoch's own key is what lets a
    /// removal (#31) revoke a member's ability to rotate the anchor.
    ///
    /// # Errors
    ///
    /// Returns an error if exporting the MLS secret fails.
    pub fn sign_doc_key_proof(&self, doc_id: &str) -> Result<Vec<u8>> {
        let keypair = self.derive_subscribe_keypair()?;
        Ok(collab_proto::sign_doc_key_proof(&keypair, doc_id, self.epoch()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_group() {
        let (group, key_package) = MlsDocumentGroup::create("alice").unwrap();

        // Key package should be valid MLS data (not just zeros)
        assert!(!key_package.is_empty());
        assert!(key_package.len() > 100, "Key package should be substantial MLS data");

        // Key package should not be all zeros (placeholder check)
        assert!(key_package.iter().any(|&b| b != 0), "Key package should not be all zeros");

        assert_eq!(group.epoch(), 0);
        assert_eq!(group.user_id(), "alice");
    }

    #[test]
    fn test_join_group() {
        // Alice creates a group
        let (mut alice, _alice_kp) = MlsDocumentGroup::create("alice").unwrap();

        // Bob generates his key package (returns PendingMember now)
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let bob_kp = bob_pending.key_package().to_vec();

        // Alice adds Bob to the group
        let (commit, welcome) = alice.add_member(&bob_kp).unwrap();

        // Commit should be valid MLS data
        assert!(!commit.is_empty());
        assert!(commit.iter().any(|&b| b != 0), "Commit should not be all zeros");

        // Welcome should be valid MLS data
        assert!(!welcome.is_empty());
        assert!(welcome.iter().any(|&b| b != 0), "Welcome should not be all zeros");

        // Bob joins using the welcome message and his pending state
        let bob = bob_pending.join(&welcome).unwrap();
        assert_eq!(bob.user_id(), "bob");

        // Both should be at epoch 1 after the add
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);
    }

    #[test]
    fn test_encrypt_decrypt() {
        // Alice creates a group
        let (mut alice, _) = MlsDocumentGroup::create("alice").unwrap();

        // Bob generates key package and joins
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let bob_kp = bob_pending.key_package().to_vec();
        let (_commit, welcome) = alice.add_member(&bob_kp).unwrap();
        let mut bob = bob_pending.join(&welcome).unwrap();

        let plaintext = b"Hello, encrypted world!";

        // Alice encrypts
        let ciphertext = alice.encrypt(plaintext).unwrap();

        // Ciphertext should NOT contain plaintext
        assert!(
            !ciphertext.windows(plaintext.len()).any(|w| w == plaintext),
            "Ciphertext should not contain plaintext"
        );

        // Bob decrypts
        let decrypted = bob.decrypt(&ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_replay_is_rejected() {
        // Alice creates a group; Bob joins.
        let (mut alice, _) = MlsDocumentGroup::create("alice").unwrap();
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let bob_kp = bob_pending.key_package().to_vec();
        let (_commit, welcome) = alice.add_member(&bob_kp).unwrap();
        let mut bob = bob_pending.join(&welcome).unwrap();

        let ciphertext = alice.encrypt(b"replay me").unwrap();

        // First delivery succeeds.
        assert_eq!(bob.decrypt(&ciphertext).unwrap(), b"replay me");

        // Re-presenting the exact same ciphertext must be rejected as a replay,
        // not surfaced as a generic MLS error.
        let err = bob.decrypt(&ciphertext).unwrap_err();
        assert!(matches!(err, Error::Replay), "expected Error::Replay on replay, got {err:?}");
    }

    #[test]
    fn test_out_of_order_within_window_is_accepted() {
        // Replay protection must not break legitimate out-of-order delivery:
        // the MLS secret tree retains a bounded window of past generations.
        let (mut alice, _) = MlsDocumentGroup::create("alice").unwrap();
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let bob_kp = bob_pending.key_package().to_vec();
        let (_commit, welcome) = alice.add_member(&bob_kp).unwrap();
        let mut bob = bob_pending.join(&welcome).unwrap();

        let m1 = alice.encrypt(b"message-one").unwrap();
        let m2 = alice.encrypt(b"message-two").unwrap();

        // Deliver in reverse order; both must decrypt successfully.
        assert_eq!(bob.decrypt(&m2).unwrap(), b"message-two");
        assert_eq!(bob.decrypt(&m1).unwrap(), b"message-one");
    }

    #[test]
    fn test_cannot_decrypt_without_key() {
        // Alice creates a group and encrypts
        let (mut alice, _) = MlsDocumentGroup::create("alice").unwrap();

        // Bob joins the group
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let bob_kp = bob_pending.key_package().to_vec();
        let (_commit, welcome) = alice.add_member(&bob_kp).unwrap();
        let mut bob = bob_pending.join(&welcome).unwrap();

        let plaintext = b"Secret message";
        let ciphertext = alice.encrypt(plaintext).unwrap();

        // Carol is NOT in the group - she creates her own group
        let (mut carol, _) = MlsDocumentGroup::create("carol").unwrap();

        // Carol should NOT be able to decrypt
        let result = carol.decrypt(&ciphertext);
        assert!(result.is_err(), "Non-member should not be able to decrypt");

        // But Bob can still decrypt (sanity check)
        let decrypted = bob.decrypt(&ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    /// Build a real 2-member group (Alice + Bob) both settled at epoch 1.
    fn two_member_group() -> (MlsDocumentGroup, MlsDocumentGroup) {
        let (mut alice, _) = MlsDocumentGroup::create("alice").unwrap();
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let bob_kp = bob_pending.key_package().to_vec();
        let (_commit, welcome) = alice.add_member(&bob_kp).unwrap();
        let bob = bob_pending.join(&welcome).unwrap();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);
        (alice, bob)
    }

    const DOC_A: &str = "notes/alpha.md";
    const NOW: u64 = 1_000;
    const TTL: u64 = 300;

    #[test]
    fn mint_then_verify_round_trip_ok() {
        // GIVEN a 2-member group, WHEN Alice mints a capability for DOC_A at the
        // current epoch with now < expiry, THEN verify with her key returns Ok.
        let (alice, _bob) = two_member_group();
        let cap = alice.mint_subscribe_capability("alice", DOC_A, NOW, TTL).unwrap();

        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &cap,
                &alice.subscribe_verifying_key().unwrap(),
                "alice",
                DOC_A,
                alice.epoch(),
                NOW,
            ),
            Ok(())
        );
    }

    #[test]
    fn group_members_share_the_same_subscribe_key() {
        // Both members of the SAME group at the SAME epoch derive the SAME key
        // (they share the exporter secret) → any member can mint, any member can
        // verify another's capability.
        let (alice, bob) = two_member_group();

        assert_eq!(
            alice.subscribe_verifying_key().unwrap(),
            bob.subscribe_verifying_key().unwrap(),
            "co-members at the same epoch must derive the same capability key"
        );

        // Bob verifies Alice's capability (minted naming "alice") using his own
        // (identical) key and Alice's identity as the expected user.
        let cap = alice.mint_subscribe_capability("alice", DOC_A, NOW, TTL).unwrap();
        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &cap,
                &bob.subscribe_verifying_key().unwrap(),
                "alice",
                DOC_A,
                bob.epoch(),
                NOW,
            ),
            Ok(())
        );
    }

    #[test]
    fn non_member_key_rejects_capability() {
        // Trust boundary: a separate group's member (Carol) derives a DIFFERENT
        // key, so verifying Alice's capability against Carol's key fails.
        let (alice, _bob) = two_member_group();
        let (carol, _) = MlsDocumentGroup::create("carol").unwrap();

        assert_ne!(
            alice.subscribe_verifying_key().unwrap(),
            carol.subscribe_verifying_key().unwrap(),
            "a non-member must not share the capability key"
        );

        let cap = alice.mint_subscribe_capability("alice", DOC_A, NOW, TTL).unwrap();
        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &cap,
                &carol.subscribe_verifying_key().unwrap(),
                "alice",
                DOC_A,
                alice.epoch(),
                NOW,
            ),
            Err(collab_proto::CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn epoch_change_rotates_key_and_invalidates_old_capability() {
        // After Alice adds a member the epoch advances → new exporter secret →
        // new capability key. A capability minted at the old epoch no longer
        // verifies against the new epoch. (This is what makes #31 removal bite.)
        let (mut alice, _bob) = two_member_group();
        let key_before = alice.subscribe_verifying_key().unwrap();
        let cap_old = alice.mint_subscribe_capability("alice", DOC_A, NOW, TTL).unwrap();
        assert_eq!(cap_old.epoch, 1);

        // Add a third member → epoch 1 -> 2, exporter secret rotates.
        let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
        let carol_kp = carol_pending.key_package().to_vec();
        let (_commit, _welcome) = alice.add_member(&carol_kp).unwrap();
        assert_eq!(alice.epoch(), 2);

        let key_after = alice.subscribe_verifying_key().unwrap();
        assert_ne!(key_before, key_after, "epoch change must rotate the capability key");

        // The old-epoch capability fails against the new anchor epoch.
        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &cap_old,
                &key_after,
                "alice",
                DOC_A,
                alice.epoch(),
                NOW,
            ),
            Err(collab_proto::CapabilityError::EpochMismatch)
        );
    }

    /// Build a real 3-member group: Alice (owner, leaf 0), Bob, Carol, all
    /// settled at epoch 2. Returns them in that order.
    fn three_member_group() -> (MlsDocumentGroup, MlsDocumentGroup, MlsDocumentGroup) {
        let (mut alice, _) = MlsDocumentGroup::create("alice").unwrap();

        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let (_c, welcome) = alice.add_member(bob_pending.key_package()).unwrap();
        let mut bob = bob_pending.join(&welcome).unwrap();

        let carol_pending = MlsDocumentGroup::generate_key_package("carol").unwrap();
        let (commit, welcome) = alice.add_member(carol_pending.key_package()).unwrap();
        // Bob must process the add commit to reach epoch 2 alongside Alice.
        bob.process_commit(&commit).unwrap();
        let carol = carol_pending.join(&welcome).unwrap();

        assert_eq!(alice.epoch(), 2);
        assert_eq!(bob.epoch(), 2);
        assert_eq!(carol.epoch(), 2);
        (alice, bob, carol)
    }

    #[test]
    fn owner_flag_reflects_creator() {
        // Alice created the group → owner; Bob joined → not owner.
        let (alice, bob, carol) = three_member_group();
        assert!(alice.is_owner(), "creator must be the owner");
        assert!(!bob.is_owner(), "a joiner must not be the owner");
        assert!(!carol.is_owner(), "a joiner must not be the owner");
    }

    #[test]
    fn owner_removes_member_advances_epoch_and_rekeys() {
        // Scenario 1 (positive): Alice removes Carol; Bob processes the commit;
        // Alice & Bob advance to epoch 3 and can still message each other.
        let (mut alice, mut bob, _carol) = three_member_group();

        let commit = alice.remove_member("carol").unwrap();
        bob.process_commit(&commit).unwrap();

        assert_eq!(alice.epoch(), 3, "owner's epoch advances after removal");
        assert_eq!(bob.epoch(), 3, "remaining member's epoch advances after removal");

        // Alice -> Bob app message still round-trips at the new epoch.
        let msg = b"post-removal secret";
        let ct = alice.encrypt(msg).unwrap();
        assert_eq!(bob.decrypt(&ct).unwrap(), msg);
    }

    #[test]
    fn removed_member_cannot_decrypt_post_removal() {
        // Scenario 2 (THE acceptance test): after Carol processes the removal
        // commit (evicting herself), she cannot decrypt a message Alice sends at
        // the new epoch. Mutation-check: if remove_member were a no-op, Carol
        // would still share the epoch key and decrypt → this test goes RED.
        let (mut alice, mut bob, mut carol) = three_member_group();

        let commit = alice.remove_member("carol").unwrap();
        bob.process_commit(&commit).unwrap();
        // Carol processes her own removal; openmls evicts her (group inactive).
        // Our wrapper must surface this as Ok(()) or Err — never a panic.
        let _ = carol.process_commit(&commit);

        // Alice sends at the new epoch. Bob (still a member) decrypts fine.
        let msg = b"members-only after removal";
        let ct = alice.encrypt(msg).unwrap();
        assert_eq!(bob.decrypt(&ct).unwrap(), msg);

        // Carol must NOT be able to decrypt it.
        let carol_result = carol.decrypt(&ct);
        assert!(
            carol_result.is_err(),
            "removed member must not decrypt a post-removal message, got {carol_result:?}"
        );
    }

    #[test]
    fn non_owner_cannot_mint_removal() {
        // Scenario 3 (policy, mint side): Bob is not the owner, so his
        // remove_member is rejected before any commit is produced.
        let (_alice, mut bob, _carol) = three_member_group();
        let result = bob.remove_member("carol");
        assert!(result.is_err(), "a non-owner must not be able to remove a member");
    }

    #[test]
    fn receive_side_rejects_non_owner_removal() {
        // Scenario 4 (policy, receive side — the enforceable half): a Remove
        // commit whose committer is NOT the owner must be REJECTED by
        // process_commit (not merged), even though MLS itself would accept it.
        //
        // The safe API's mint guard makes a non-owner removal unmintable via
        // remove_member, so we reach past it to the raw group to hand-craft a
        // non-owner Remove commit, then assert the receive guard rejects it.
        let (mut alice, mut bob, _carol) = three_member_group();

        // Bob (non-owner) crafts a Remove commit evicting Carol via the raw group.
        let carol_leaf = bob
            .group
            .members()
            .find(|m| credential_identity(&m.credential).unwrap() == "carol")
            .map(|m| m.index)
            .expect("carol is a member");
        let (commit, _welcome, _gi) = bob
            .group
            .remove_members(&bob.crypto, &bob.signature_keys, &[carol_leaf])
            .expect("raw remove_members should mint a commit regardless of policy");
        // Bob does NOT merge; he just ships the commit. (openmls has no pending
        // commit lingering that would block Alice's processing.)
        bob.group.clear_pending_commit(bob.crypto.storage()).unwrap();
        let commit_bytes = commit.tls_serialize_detached().unwrap();

        let epoch_before = alice.epoch();
        let result = alice.process_commit(&commit_bytes);
        assert!(
            result.is_err(),
            "a Remove commit from a non-owner must be rejected, got {result:?}"
        );
        assert_eq!(alice.epoch(), epoch_before, "a rejected removal must not advance the epoch");
    }

    #[test]
    fn receive_side_rejects_removal_of_the_owner() {
        // Scenario 6 (defense-in-depth): a commit that REMOVES THE OWNER (leaf 0)
        // must be rejected even when its committer passes the non-owner check.
        // openmls forbids removing your own leaf (CannotRemoveSelf), so the real
        // owner can never mint a self-removal; the reachable threat is a member
        // that joined under the OWNER'S identity string ("alice") with a fresh
        // signature key (BasicCredential identities are not unique — only keys
        // are) and evicts leaf 0. That committer's identity == owner_id, so the
        // non-owner-committer guard PASSES and ONLY the new owner-target guard
        // catches it: RED before the guard (owner-eviction merges, epoch
        // advances), GREEN after.
        let (mut alice, _kp) = MlsDocumentGroup::create("alice").unwrap();

        // Bob joins as a normal member (leaf 1); he is the processor.
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let (_commit, welcome) = alice.add_member(bob_pending.key_package()).unwrap();
        let mut bob = bob_pending.join(&welcome).unwrap();

        // A spoofer joins claiming the OWNER'S identity ("alice") at leaf 2.
        let spoof_pending = MlsDocumentGroup::generate_key_package("alice").unwrap();
        let (add_commit, spoof_welcome) = alice.add_member(spoof_pending.key_package()).unwrap();
        bob.process_commit(&add_commit).unwrap();
        let mut spoofer = spoof_pending.join(&spoof_welcome).unwrap();
        assert_eq!(bob.owner_id, "alice", "processor's learned owner is the leaf-0 creator");

        // Spoofer crafts a raw Remove commit evicting leaf 0 (the real owner);
        // leaf 0 is not the spoofer's own leaf, so openmls mints it.
        let (commit, _welcome, _gi) = spoofer
            .group
            .remove_members(&spoofer.crypto, &spoofer.signature_keys, &[LeafNodeIndex::new(0)])
            .expect("removing a leaf that is not one's own must mint a commit");
        spoofer.group.clear_pending_commit(spoofer.crypto.storage()).unwrap();
        let commit_bytes = commit.tls_serialize_detached().unwrap();

        let epoch_before = bob.epoch();
        let result = bob.process_commit(&commit_bytes);
        assert!(
            result.is_err(),
            "a commit removing the owner (leaf 0) must be rejected, got {result:?}"
        );
        assert_eq!(
            bob.epoch(),
            epoch_before,
            "a rejected owner-removal must not advance the epoch"
        );
        assert!(
            bob.group.members().any(|m| m.index == LeafNodeIndex::new(0)),
            "the owner must remain a member after a rejected owner-removal"
        );
    }

    #[test]
    fn removed_member_subscribe_capability_dies_after_removal() {
        // Scenario 5 (#29 cross-cut): after Alice removes Carol the epoch
        // advances and the exporter secret rotates, so subscribe_verifying_key
        // changes. A capability Carol mints (at her last-known epoch/key) fails
        // verification against the new anchor; a capability Alice mints at the
        // new epoch verifies.
        let (mut alice, mut bob, mut carol) = three_member_group();

        let key_before = alice.subscribe_verifying_key().unwrap();
        // Carol mints a capability at the pre-removal epoch (epoch 2).
        let carol_cap = carol.mint_subscribe_capability("carol", DOC_A, NOW, TTL).unwrap();
        assert_eq!(carol_cap.epoch, 2);

        // Alice removes Carol; Bob & Carol process it.
        let commit = alice.remove_member("carol").unwrap();
        bob.process_commit(&commit).unwrap();
        let _ = carol.process_commit(&commit);

        // New epoch → rotated anchor key (owner re-registers it, same as add).
        let key_after = alice.subscribe_verifying_key().unwrap();
        assert_ne!(key_before, key_after, "removal must rotate the subscribe anchor key");
        let new_epoch = alice.epoch();

        // Carol's old-epoch capability fails against the new epoch/key.
        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &carol_cap, &key_after, "carol", DOC_A, new_epoch, NOW,
            ),
            Err(collab_proto::CapabilityError::EpochMismatch),
            "a removed member's stale-epoch capability must not verify against the new anchor"
        );

        // A remaining member (Bob) mints at the new epoch → verifies.
        let bob_cap = bob.mint_subscribe_capability("bob", DOC_A, NOW, TTL).unwrap();
        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &bob_cap, &key_after, "bob", DOC_A, new_epoch, NOW,
            ),
            Ok(()),
            "a remaining member's new-epoch capability must verify"
        );
    }

    #[test]
    fn test_bidirectional_encryption() {
        // Alice creates a group
        let (mut alice, _) = MlsDocumentGroup::create("alice").unwrap();

        // Bob joins
        let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
        let bob_kp = bob_pending.key_package().to_vec();
        let (_commit, welcome) = alice.add_member(&bob_kp).unwrap();
        let mut bob = bob_pending.join(&welcome).unwrap();

        // Alice sends to Bob
        let msg1 = b"Hello Bob!";
        let ciphertext1 = alice.encrypt(msg1).unwrap();
        let decrypted1 = bob.decrypt(&ciphertext1).unwrap();
        assert_eq!(decrypted1, msg1);

        // Bob sends to Alice
        let msg2 = b"Hello Alice!";
        let ciphertext2 = bob.encrypt(msg2).unwrap();
        let decrypted2 = alice.decrypt(&ciphertext2).unwrap();
        assert_eq!(decrypted2, msg2);
    }
}
