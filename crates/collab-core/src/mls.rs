//! MLS (Messaging Layer Security) group operations for E2E encryption.

use crate::{Error, Result};
use openmls::framing::errors::{MessageDecryptionError, SecretTreeError};
use openmls::prelude::tls_codec::{Deserialize, Serialize};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

/// The ciphersuite to use for MLS operations.
const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

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
    /// The MLS group.
    group: MlsGroup,
    /// The crypto provider.
    crypto: OpenMlsRustCrypto,
    /// The signature key pair.
    signature_keys: SignatureKeyPair,
    /// The credential with key.
    _credential_with_key: CredentialWithKey,
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

        Ok(MlsDocumentGroup {
            user_id: self.user_id,
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

        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged_commit) => {
                // Merge the staged commit to update our group state
                self.group
                    .merge_staged_commit(&self.crypto, *staged_commit)
                    .map_err(|e| Error::Mls(format!("Failed to merge staged commit: {e:?}")))?;
                Ok(())
            }
            _ => Err(Error::Mls("Expected commit message".to_string())),
        }
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
