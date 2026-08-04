//! Per-document subscribe authorization capability (issue #29).
//!
//! A [`SubscribeCapability`] proves current-epoch membership of a document's MLS
//! group without revealing any group secret. A member mints one by signing a
//! canonical byte string with an `Ed25519` key derived from the group's per-epoch
//! exporter secret (the minting side lives in `collab-core`). The relay — a
//! zero-knowledge router that never holds group state — verifies it against a
//! public `Ed25519` verifying key alone via [`verify_subscribe_capability`].
//!
//! Verification is `Ed25519`-only: this crate never depends on MLS/openmls.

use serde::{Deserialize, Serialize};

/// Domain-separation prefix mixed into the signed bytes so a `SubscribeCapability`
/// signature can never be confused with any other `Ed25519` signature this project
/// produces.
const LABEL_MSG: &[u8] = b"obsidian-ee/subscribe-capability/v1";

/// Domain-separation prefix for a `RegisterDocKey` self-proof (issue #29). A
/// distinct label from [`LABEL_MSG`] guarantees a capability signature can never
/// be replayed as a registration proof (or vice-versa) even for the same
/// `(doc_id, epoch)`.
const REGISTER_LABEL: &[u8] = b"obsidian-ee/register-doc-key/v1";

/// Domain-separation prefix for an anchor-rotation continuity proof (issue #29,
/// PR #73 review). A rotation proof is signed by the CURRENT anchor key over the
/// NEW `{epoch, public_key}`; a distinct label keeps it from ever doubling as a
/// self-proof ([`REGISTER_LABEL`]) or a capability ([`LABEL_MSG`]).
const ROTATE_LABEL: &[u8] = b"obsidian-ee/anchor-rotation/v1";

/// A subscription capability: proves current-epoch membership of `doc_id`'s
/// group AND names the subscriber it authorizes.
///
/// The `signature` is `Ed25519` over the canonical bytes described in
/// [`signing_bytes`]. Binding `user_id` into those bytes makes the capability
/// name WHO it authorizes: a capability minted for Alice cannot be replayed by
/// Eve within its TTL, because the relay checks `cap.user_id` against the
/// presenting connection's LOCALLY-trusted identified user id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeCapability {
    /// The subscriber this capability authorizes. The relay rejects it unless
    /// this equals the presenting connection's identified user id — so a bearer
    /// token minted for one member cannot be replayed by another.
    pub user_id: String,
    /// Document the capability authorizes subscription to.
    pub doc_id: String,
    /// MLS epoch the capability was minted at; must equal the relay's anchor epoch.
    pub epoch: u64,
    /// Expiry as seconds since the Unix epoch; the relay rejects if `now > expiry`.
    pub expiry_unix: u64,
    /// `Ed25519` signature over [`signing_bytes`].
    pub signature: Vec<u8>,
}

/// Reasons [`verify_subscribe_capability`] rejects a capability. Each variant is a
/// distinct rejection cause so callers (and tests) can tell them apart.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapabilityError {
    /// `cap.user_id` did not match the expected (presenting) user id — a
    /// capability minted for one member presented by another.
    #[error("capability user_id does not match expected user_id")]
    UserIdMismatch,
    /// `cap.doc_id` did not match the expected document id.
    #[error("capability doc_id does not match expected doc_id")]
    DocIdMismatch,
    /// `cap.epoch` did not match the expected (anchor) epoch.
    #[error("capability epoch does not match expected epoch")]
    EpochMismatch,
    /// `now_unix` is past `cap.expiry_unix`.
    #[error("capability has expired")]
    Expired,
    /// The supplied verifying-key bytes are not a valid `Ed25519` public key.
    #[error("verifying key is not a valid Ed25519 public key")]
    InvalidVerifyingKey,
    /// `cap.signature` is not exactly 64 bytes.
    #[error("capability signature is not a 64-byte Ed25519 signature")]
    MalformedSignature,
    /// The signature did not verify over the canonical bytes under the key.
    #[error("Ed25519 signature verification failed")]
    SignatureVerificationFailed,
}

/// Canonical bytes an `Ed25519` signature covers, in order:
///
/// ```text
/// LABEL_MSG || (user_id.len() as u32 le) || user_id bytes
///           || (doc_id.len() as u32 le) || doc_id bytes
///           || epoch (u64 le) || expiry_unix (u64 le)
/// ```
///
/// `user_id` and `doc_id` are each length-prefixed so `(user_id, doc_id, epoch,
/// expiry)` can never be re-parsed ambiguously (e.g. a trailing slice of one
/// field masquerading as the next). Binding `user_id` names WHO the capability
/// authorizes, so it cannot be replayed by another subscriber.
fn signing_bytes(user_id: &str, doc_id: &str, epoch: u64, expiry_unix: u64) -> Vec<u8> {
    let user = user_id.as_bytes();
    let doc = doc_id.as_bytes();
    // A user_id/doc_id longer than u32::MAX (~4 GiB) is not real; fail loudly
    // rather than silently truncate the length prefix.
    let user_len = u32::try_from(user.len()).expect("user_id length exceeds u32::MAX");
    let doc_len = u32::try_from(doc.len()).expect("doc_id length exceeds u32::MAX");
    let mut out = Vec::with_capacity(LABEL_MSG.len() + 4 + user.len() + 4 + doc.len() + 16);
    out.extend_from_slice(LABEL_MSG);
    out.extend_from_slice(&user_len.to_le_bytes());
    out.extend_from_slice(user);
    out.extend_from_slice(&doc_len.to_le_bytes());
    out.extend_from_slice(doc);
    out.extend_from_slice(&epoch.to_le_bytes());
    out.extend_from_slice(&expiry_unix.to_le_bytes());
    out
}

/// Mint a capability naming `user_id` for `doc_id` at `epoch`, valid until
/// `expiry_unix`.
///
/// Lives in `collab-proto` (not `collab-core`) so the minting and verifying sides
/// share the exact byte layout in [`signing_bytes`]. The `user_id` is the minting
/// member's own identity; the relay later checks it against the presenting
/// connection so the capability cannot be replayed as someone else.
#[must_use]
pub fn sign_subscribe_capability(
    signing_key: &ed25519_dalek::SigningKey,
    user_id: &str,
    doc_id: &str,
    epoch: u64,
    expiry_unix: u64,
) -> SubscribeCapability {
    use ed25519_dalek::Signer;
    let signature = signing_key.sign(&signing_bytes(user_id, doc_id, epoch, expiry_unix));
    SubscribeCapability {
        user_id: user_id.to_owned(),
        doc_id: doc_id.to_owned(),
        epoch,
        expiry_unix,
        signature: signature.to_bytes().to_vec(),
    }
}

/// Verify a subscription capability. Pure `Ed25519`; the relay is not a group member.
///
/// Returns `Ok(())` iff **all** hold: the signature verifies over
/// [`signing_bytes`] under `verifying_key`, `cap.user_id == expected_user_id`,
/// `cap.doc_id == expected_doc_id`, `cap.epoch == expected_epoch`, and
/// `now_unix <= cap.expiry_unix`.
///
/// The caller MUST pass the LOCALLY-trusted `expected_user_id`/`expected_doc_id`
/// /`expected_epoch` (the presenting connection's identified user id, the
/// subscribe target, and the relay's stored anchor epoch) — never a value taken
/// from the inbound frame. Passing `cap.user_id` as `expected_user_id` would
/// defeat the replay binding.
///
/// # Errors
///
/// Returns the matching [`CapabilityError`] variant for each distinct failure:
/// [`CapabilityError::UserIdMismatch`], [`CapabilityError::DocIdMismatch`],
/// [`CapabilityError::EpochMismatch`], [`CapabilityError::Expired`],
/// [`CapabilityError::InvalidVerifyingKey`],
/// [`CapabilityError::MalformedSignature`], or
/// [`CapabilityError::SignatureVerificationFailed`].
pub fn verify_subscribe_capability(
    cap: &SubscribeCapability,
    verifying_key: &[u8; 32],
    expected_user_id: &str,
    expected_doc_id: &str,
    expected_epoch: u64,
    now_unix: u64,
) -> Result<(), CapabilityError> {
    if cap.user_id != expected_user_id {
        return Err(CapabilityError::UserIdMismatch);
    }
    if cap.doc_id != expected_doc_id {
        return Err(CapabilityError::DocIdMismatch);
    }
    if cap.epoch != expected_epoch {
        return Err(CapabilityError::EpochMismatch);
    }
    if now_unix > cap.expiry_unix {
        return Err(CapabilityError::Expired);
    }

    let key = ed25519_dalek::VerifyingKey::from_bytes(verifying_key)
        .map_err(|_| CapabilityError::InvalidVerifyingKey)?;
    let sig_bytes: [u8; 64] =
        cap.signature.as_slice().try_into().map_err(|_| CapabilityError::MalformedSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);

    // Recompute the canonical bytes from the capability's OWN fields; a signature
    // is bound to the (user_id, doc_id, epoch, expiry) it was minted for, so
    // swapping any field without re-signing fails here.
    let message = signing_bytes(&cap.user_id, &cap.doc_id, cap.epoch, cap.expiry_unix);
    key.verify_strict(&message, &signature)
        .map_err(|_| CapabilityError::SignatureVerificationFailed)
}

/// Canonical bytes a `RegisterDocKey` self-proof covers:
///
/// ```text
/// REGISTER_LABEL || (doc_id.len() as u32 le) || doc_id bytes || epoch (u64 le)
/// ```
///
/// The registrant's `Ed25519` public key is NOT mixed in: the proof is *verified
/// under* that public key, so binding it is redundant. Length-prefixing `doc_id`
/// keeps `(doc_id, epoch)` unambiguous, matching [`signing_bytes`].
fn register_proof_bytes(doc_id: &str, epoch: u64) -> Vec<u8> {
    let doc = doc_id.as_bytes();
    let doc_len = u32::try_from(doc.len()).expect("doc_id length exceeds u32::MAX");
    let mut out = Vec::with_capacity(REGISTER_LABEL.len() + 4 + doc.len() + 8);
    out.extend_from_slice(REGISTER_LABEL);
    out.extend_from_slice(&doc_len.to_le_bytes());
    out.extend_from_slice(doc);
    out.extend_from_slice(&epoch.to_le_bytes());
    out
}

/// Sign a `RegisterDocKey` self-proof with the epoch's signing key (issue #29).
///
/// Proves the registrant holds the private half of the `public_key` being
/// registered for `(doc_id, epoch)`. It does NOT prove group membership: the
/// relay is zero-knowledge and cannot verify membership. Anchor trust is TOFU
/// (first registrant wins). Where removal (#31) bites is the *subscribe* path,
/// not here: after a rekey a removed member's stale-epoch capability no longer
/// matches the rotated anchor. See [`verify_doc_key_proof`] for the full trust
/// model.
#[must_use]
pub fn sign_doc_key_proof(
    signing_key: &ed25519_dalek::SigningKey,
    doc_id: &str,
    epoch: u64,
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    signing_key.sign(&register_proof_bytes(doc_id, epoch)).to_bytes().to_vec()
}

/// Verify a `RegisterDocKey` self-proof under `public_key`. Pure `Ed25519`.
///
/// Returns `Ok(())` iff `proof` is a valid `Ed25519` signature of
/// [`register_proof_bytes`] under `public_key`. This proves only *possession of
/// the epoch keypair being registered* — NOT group membership. The relay is a
/// zero-knowledge router with no group state and no identity system, so it
/// cannot verify that the registrant is actually a member. Anchor trust is TOFU
/// (first registrant wins), the same trust model as first-Identify-wins for
/// `user_id`. A removed member's stale-epoch *capability* stops verifying after
/// rotation (that is where #31 bites — the subscribe path), but the relay cannot
/// prevent a non-member from registering an anchor for a doc no one has claimed
/// yet.
///
/// # Errors
///
/// [`CapabilityError::InvalidVerifyingKey`] if `public_key` is not a valid
/// `Ed25519` point, [`CapabilityError::MalformedSignature`] if `proof` is not 64
/// bytes, or [`CapabilityError::SignatureVerificationFailed`] if it does not
/// verify.
pub fn verify_doc_key_proof(
    doc_id: &str,
    epoch: u64,
    public_key: &[u8; 32],
    proof: &[u8],
) -> Result<(), CapabilityError> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(public_key)
        .map_err(|_| CapabilityError::InvalidVerifyingKey)?;
    let sig_bytes: [u8; 64] = proof.try_into().map_err(|_| CapabilityError::MalformedSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    key.verify_strict(&register_proof_bytes(doc_id, epoch), &signature)
        .map_err(|_| CapabilityError::SignatureVerificationFailed)
}

/// Canonical bytes an anchor-rotation continuity proof covers:
///
/// ```text
/// ROTATE_LABEL || (doc_id.len() as u32 le) || doc_id bytes
///              || new_epoch (u64 le) || new_public_key (32 bytes)
/// ```
///
/// Unlike [`register_proof_bytes`], the NEW `public_key` IS mixed in: the proof
/// is verified under the CURRENT (stored) anchor key, so binding the new key is
/// what ties the current key holder to the specific key they are rotating to.
fn rotation_proof_bytes(doc_id: &str, new_epoch: u64, new_public_key: &[u8; 32]) -> Vec<u8> {
    let doc = doc_id.as_bytes();
    let doc_len = u32::try_from(doc.len()).expect("doc_id length exceeds u32::MAX");
    let mut out = Vec::with_capacity(ROTATE_LABEL.len() + 4 + doc.len() + 8 + 32);
    out.extend_from_slice(ROTATE_LABEL);
    out.extend_from_slice(&doc_len.to_le_bytes());
    out.extend_from_slice(doc);
    out.extend_from_slice(&new_epoch.to_le_bytes());
    out.extend_from_slice(new_public_key);
    out
}

/// Sign an anchor-rotation continuity proof with the CURRENT anchor's signing key
/// (issue #29, PR #73 review).
///
/// A rotation (an anchor already exists for the doc) must carry this proof so the
/// relay can verify it against the stored `anchor.verifying_key`, tying the
/// rotation to possession of the CURRENT anchor key — not just a monotonically
/// higher epoch. `new_epoch`/`new_public_key` are the anchor being rotated TO.
///
/// This RAISES the bar from "any identified client can overwrite an anchor by
/// picking a higher epoch" to "only a holder of the current anchor key can". It
/// is NOT full membership proof: a member present at epoch N holds `key_N` and can
/// forge a rotation to N+1 until the group naturally rekeys past their knowledge.
#[must_use]
pub fn sign_anchor_rotation(
    current_signing_key: &ed25519_dalek::SigningKey,
    doc_id: &str,
    new_epoch: u64,
    new_public_key: &[u8; 32],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    current_signing_key
        .sign(&rotation_proof_bytes(doc_id, new_epoch, new_public_key))
        .to_bytes()
        .to_vec()
}

/// Verify an anchor-rotation continuity proof under the CURRENT anchor key. Pure
/// `Ed25519`.
///
/// Returns `Ok(())` iff `proof` is a valid `Ed25519` signature of
/// [`rotation_proof_bytes`] under `current_verifying_key` (the relay's STORED
/// anchor key for the doc). This proves the rotation was authorized by a holder
/// of the current anchor key. See [`sign_anchor_rotation`] for the honest limits
/// of what this proves (not membership).
///
/// # Errors
///
/// [`CapabilityError::InvalidVerifyingKey`] if `current_verifying_key` is not a
/// valid `Ed25519` point, [`CapabilityError::MalformedSignature`] if `proof` is
/// not 64 bytes, or [`CapabilityError::SignatureVerificationFailed`] if it does
/// not verify.
pub fn verify_anchor_rotation(
    doc_id: &str,
    new_epoch: u64,
    new_public_key: &[u8; 32],
    current_verifying_key: &[u8; 32],
    proof: &[u8],
) -> Result<(), CapabilityError> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(current_verifying_key)
        .map_err(|_| CapabilityError::InvalidVerifyingKey)?;
    let sig_bytes: [u8; 64] = proof.try_into().map_err(|_| CapabilityError::MalformedSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    key.verify_strict(&rotation_proof_bytes(doc_id, new_epoch, new_public_key), &signature)
        .map_err(|_| CapabilityError::SignatureVerificationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    const USER_A: &str = "alice";
    const USER_B: &str = "bob";
    const DOC_A: &str = "notes/alpha.md";
    const DOC_B: &str = "notes/beta.md";
    const EPOCH: u64 = 7;
    const EXPIRY: u64 = 1_000;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn vk_bytes(k: &SigningKey) -> [u8; 32] {
        k.verifying_key().to_bytes()
    }

    #[test]
    fn round_trip_verifies_ok() {
        // Arrange
        let signer = key(1);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);

        // Act
        let result =
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_A, DOC_A, EPOCH, EXPIRY - 1);

        // Assert
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn now_equal_to_expiry_is_ok() {
        let signer = key(1);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        // now == expiry is allowed (rejection is now > expiry).
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_A, DOC_A, EPOCH, EXPIRY),
            Ok(())
        );
    }

    #[test]
    fn wrong_expected_user_id_rejected() {
        // A capability minted for USER_A presented as USER_B is rejected: the
        // relay passes the presenting connection's identified user id as
        // expected_user_id, so a bearer token cannot be replayed as someone else.
        let signer = key(1);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_B, DOC_A, EPOCH, EXPIRY - 1),
            Err(CapabilityError::UserIdMismatch)
        );
    }

    #[test]
    fn tampered_user_id_field_rejected() {
        // Swapping the user_id field to match expected_user_id (so the equality
        // check passes) makes verification recompute the message over the new
        // user_id, and the signature — bound to USER_A — no longer matches.
        let signer = key(1);
        let mut cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        cap.user_id = USER_B.to_owned();
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_B, DOC_A, EPOCH, EXPIRY - 1),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn wrong_verifying_key_rejected() {
        // Arrange: sign with key 1, verify with key 2's public key.
        let signer = key(1);
        let other = key(2);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);

        // Act / Assert
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&other), USER_A, DOC_A, EPOCH, EXPIRY - 1),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn wrong_expected_doc_id_rejected() {
        let signer = key(1);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_A, DOC_B, EPOCH, EXPIRY - 1),
            Err(CapabilityError::DocIdMismatch)
        );
    }

    #[test]
    fn wrong_expected_epoch_rejected() {
        let signer = key(1);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        assert_eq!(
            verify_subscribe_capability(
                &cap,
                &vk_bytes(&signer),
                USER_A,
                DOC_A,
                EPOCH + 1,
                EXPIRY - 1
            ),
            Err(CapabilityError::EpochMismatch)
        );
    }

    #[test]
    fn expired_rejected() {
        let signer = key(1);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        // now_unix > expiry_unix
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_A, DOC_A, EPOCH, EXPIRY + 1),
            Err(CapabilityError::Expired)
        );
    }

    #[test]
    fn flipped_signature_byte_rejected() {
        let signer = key(1);
        let mut cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        cap.signature[0] ^= 0x01;
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_A, DOC_A, EPOCH, EXPIRY - 1),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn cross_doc_replay_rejected() {
        // A capability minted for DOC_A cannot be reused for DOC_B by swapping the
        // doc_id field: the signature is bound to DOC_A's bytes. Setting doc_id to
        // DOC_B (so the doc_id equality check passes) makes verification recompute
        // the message over DOC_B and the signature no longer matches.
        let signer = key(1);
        let mut cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        cap.doc_id = DOC_B.to_owned();
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_A, DOC_B, EPOCH, EXPIRY - 1),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn malformed_signature_length_rejected() {
        let signer = key(1);
        let mut cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        cap.signature.truncate(10);
        assert_eq!(
            verify_subscribe_capability(&cap, &vk_bytes(&signer), USER_A, DOC_A, EPOCH, EXPIRY - 1),
            Err(CapabilityError::MalformedSignature)
        );
    }

    #[test]
    fn doc_key_proof_round_trip_ok() {
        let signer = key(1);
        let proof = sign_doc_key_proof(&signer, DOC_A, EPOCH);
        assert_eq!(verify_doc_key_proof(DOC_A, EPOCH, &vk_bytes(&signer), &proof), Ok(()));
    }

    #[test]
    fn doc_key_proof_wrong_key_rejected() {
        // A proof signed by key 1 must not verify under key 2's public key: only
        // the epoch's own key holder can register that epoch's anchor.
        let signer = key(1);
        let other = key(2);
        let proof = sign_doc_key_proof(&signer, DOC_A, EPOCH);
        assert_eq!(
            verify_doc_key_proof(DOC_A, EPOCH, &vk_bytes(&other), &proof),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn doc_key_proof_wrong_epoch_rejected() {
        // A proof for epoch E must not verify for epoch E+1: epoch is bound into
        // the signed bytes, so a stale-epoch proof cannot forge a rotation.
        let signer = key(1);
        let proof = sign_doc_key_proof(&signer, DOC_A, EPOCH);
        assert_eq!(
            verify_doc_key_proof(DOC_A, EPOCH + 1, &vk_bytes(&signer), &proof),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn doc_key_proof_wrong_doc_rejected() {
        let signer = key(1);
        let proof = sign_doc_key_proof(&signer, DOC_A, EPOCH);
        assert_eq!(
            verify_doc_key_proof(DOC_B, EPOCH, &vk_bytes(&signer), &proof),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn doc_key_proof_malformed_length_rejected() {
        let signer = key(1);
        let mut proof = sign_doc_key_proof(&signer, DOC_A, EPOCH);
        proof.truncate(10);
        assert_eq!(
            verify_doc_key_proof(DOC_A, EPOCH, &vk_bytes(&signer), &proof),
            Err(CapabilityError::MalformedSignature)
        );
    }

    #[test]
    fn capability_signature_is_not_a_valid_register_proof() {
        // Domain separation: a SubscribeCapability signature (LABEL_MSG) must not
        // double as a RegisterDocKey proof (REGISTER_LABEL) for the same key.
        let signer = key(1);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);
        assert_eq!(
            verify_doc_key_proof(DOC_A, EPOCH, &vk_bytes(&signer), &cap.signature),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn serde_round_trip_then_verify_ok() {
        let signer = key(3);
        let cap = sign_subscribe_capability(&signer, USER_A, DOC_A, EPOCH, EXPIRY);

        let json = serde_json::to_string(&cap).expect("serialize");
        let restored: SubscribeCapability = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            verify_subscribe_capability(
                &restored,
                &vk_bytes(&signer),
                USER_A,
                DOC_A,
                EPOCH,
                EXPIRY - 1
            ),
            Ok(())
        );
    }

    #[test]
    fn anchor_rotation_round_trip_ok() {
        // The current anchor key signs a rotation to a NEW epoch + key; verifying
        // under the current key succeeds.
        let current = key(1);
        let new_key = vk_bytes(&key(2));
        let proof = sign_anchor_rotation(&current, DOC_A, EPOCH + 1, &new_key);
        assert_eq!(
            verify_anchor_rotation(DOC_A, EPOCH + 1, &new_key, &vk_bytes(&current), &proof),
            Ok(())
        );
    }

    #[test]
    fn anchor_rotation_wrong_current_key_rejected() {
        // A rotation proof signed by an ATTACKER key (not the current anchor key)
        // must not verify under the current anchor key — this is the teeth of the
        // continuity check: you cannot rotate without holding the current key.
        let current = key(1);
        let attacker = key(9);
        let new_key = vk_bytes(&key(2));
        let proof = sign_anchor_rotation(&attacker, DOC_A, EPOCH + 1, &new_key);
        assert_eq!(
            verify_anchor_rotation(DOC_A, EPOCH + 1, &new_key, &vk_bytes(&current), &proof),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn anchor_rotation_tampered_new_epoch_rejected() {
        // The new_epoch is bound into the signed bytes: verifying against a
        // different epoch than signed fails.
        let current = key(1);
        let new_key = vk_bytes(&key(2));
        let proof = sign_anchor_rotation(&current, DOC_A, EPOCH + 1, &new_key);
        assert_eq!(
            verify_anchor_rotation(DOC_A, EPOCH + 2, &new_key, &vk_bytes(&current), &proof),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn anchor_rotation_tampered_new_key_rejected() {
        // The new_public_key is bound in: a proof authorizing rotation to key A
        // cannot be reused to rotate to attacker key B.
        let current = key(1);
        let key_a = vk_bytes(&key(2));
        let key_b = vk_bytes(&key(3));
        let proof = sign_anchor_rotation(&current, DOC_A, EPOCH + 1, &key_a);
        assert_eq!(
            verify_anchor_rotation(DOC_A, EPOCH + 1, &key_b, &vk_bytes(&current), &proof),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn anchor_rotation_malformed_length_rejected() {
        let current = key(1);
        let new_key = vk_bytes(&key(2));
        let mut proof = sign_anchor_rotation(&current, DOC_A, EPOCH + 1, &new_key);
        proof.truncate(10);
        assert_eq!(
            verify_anchor_rotation(DOC_A, EPOCH + 1, &new_key, &vk_bytes(&current), &proof),
            Err(CapabilityError::MalformedSignature)
        );
    }

    #[test]
    fn register_proof_is_not_a_valid_rotation_proof() {
        // Domain separation: a RegisterDocKey self-proof (REGISTER_LABEL) must not
        // double as an anchor-rotation proof (ROTATE_LABEL) for the same key.
        let current = key(1);
        let new_key = vk_bytes(&key(2));
        let self_proof = sign_doc_key_proof(&current, DOC_A, EPOCH + 1);
        assert_eq!(
            verify_anchor_rotation(DOC_A, EPOCH + 1, &new_key, &vk_bytes(&current), &self_proof),
            Err(CapabilityError::SignatureVerificationFailed)
        );
    }
}
