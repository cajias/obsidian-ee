//! Encrypted-at-rest persistence of MLS group state (issue #30).
//!
//! MLS group state lives only in memory (`OpenMlsRustCrypto`'s `MemoryStorage`).
//! A restart throws it away and forces a full re-join. This module snapshots the
//! in-memory storage (openmls auto-persists group state to it on every
//! commit/merge/create), seals it with AES-256-GCM under a caller-supplied key,
//! and restores it on startup — preserving the epoch, so no re-join is needed. A
//! snapshot whose epoch predates a known rotation falls back to a clean re-join.
//!
//! The at-rest key's provenance (OS keychain / passphrase) is the caller's
//! concern; this module takes a `&[u8; 32]` and does the AEAD. An all-zeros key
//! is rejected (fail-closed), mirroring the #27/#28 guards.

use crate::{Error, MlsDocumentGroup, Result};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

/// Snapshot format version. Bump on any layout change so an old snapshot is
/// rejected (`Err`) rather than mis-parsed.
pub const SNAPSHOT_VERSION: u8 = 1;

/// AES-GCM nonce length in bytes.
const NONCE_LEN: usize = 12;

/// Upper bound on the entry count a decoded snapshot may declare. An MLS group's
/// storage has far fewer entries; a larger declared count is a corrupt or hostile
/// snapshot and is rejected before the decode loop allocates.
const MAX_STORAGE_ENTRIES: u64 = 100_000;

fn corrupt<T: Into<String>>(msg: T) -> Error {
    Error::Encryption(format!("corrupt snapshot: {}", msg.into()))
}

/// True if `key` is entirely zero bytes — rejected as a placeholder key.
fn is_all_zeros(key: &[u8; 32]) -> bool {
    key.iter().all(|&b| b == 0)
}

/// Build the AEAD cipher from a fixed 32-byte key (avoids the deprecated
/// `GenericArray::from_slice`).
fn new_cipher(key: &[u8; 32]) -> Aes256Gcm {
    Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key))
}

// --- length-prefixed encoding helpers (checked, never panic on bad input) ---

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    // usize -> u32 widths are bounded: group_id/user_id/public key are all small.
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| corrupt("length overflow"))?;
        let slice = self.buf.get(self.pos..end).ok_or_else(|| corrupt("unexpected end"))?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| corrupt("bad u32"))?;
        Ok(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Result<u64> {
        let b: [u8; 8] = self.take(8)?.try_into().map_err(|_| corrupt("bad u64"))?;
        Ok(u64::from_le_bytes(b))
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

/// Serialize the `MemoryStorage.values` map ourselves.
///
/// `MemoryStorage::serialize` exists but is `#[cfg(feature = "test-utils")]`
/// gated, so it is unavailable in a normal build. Layout:
/// `count(u64) || (klen(u64) || k || vlen(u64) || v)*`.
fn encode_storage(crypto: &OpenMlsRustCrypto) -> Vec<u8> {
    // A poisoned lock still holds valid data; recover it rather than crash — a
    // snapshot may be taken during error recovery, so this path must be fail-soft.
    let values = crypto.storage().values.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut out = Vec::new();
    out.extend_from_slice(&(values.len() as u64).to_le_bytes());
    for (k, v) in values.iter() {
        out.extend_from_slice(&(k.len() as u64).to_le_bytes());
        out.extend_from_slice(k);
        out.extend_from_slice(&(v.len() as u64).to_le_bytes());
        out.extend_from_slice(v);
    }
    drop(values);
    out
}

/// Inverse of [`encode_storage`]: parse the length-prefixed map from `reader`
/// and write it into `crypto`'s storage.
fn decode_storage_into(reader: &mut Reader, crypto: &OpenMlsRustCrypto) -> Result<()> {
    let count = reader.u64()?;
    if count > MAX_STORAGE_ENTRIES {
        return Err(corrupt(format!(
            "storage entry count {count} exceeds cap {MAX_STORAGE_ENTRIES}"
        )));
    }
    let mut map = std::collections::HashMap::new();
    for _ in 0..count {
        let klen = usize::try_from(reader.u64()?).map_err(|_| corrupt("key too large"))?;
        let k = reader.take(klen)?.to_vec();
        let vlen = usize::try_from(reader.u64()?).map_err(|_| corrupt("value too large"))?;
        let v = reader.take(vlen)?.to_vec();
        map.insert(k, v);
    }
    // Recover a poisoned lock rather than crash (see `encode_storage`).
    let mut values =
        crypto.storage().values.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    *values = map;
    drop(values);
    Ok(())
}

impl MlsDocumentGroup {
    /// Snapshot and encrypt this group's full MLS state (group + signature keys).
    ///
    /// `key` is a 32-byte AES-256-GCM key; an all-zeros key is rejected. The
    /// output is `nonce(12) || ciphertext` with a fresh random nonce, so two
    /// snapshots of the same state differ and a caller cannot reuse a nonce.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encryption`] if the key is all-zeros or AEAD sealing
    /// fails, or [`Error::Mls`] if OS randomness for the nonce is unavailable.
    pub fn snapshot_encrypted(&self, key: &[u8; 32]) -> Result<Vec<u8>> {
        if is_all_zeros(key) {
            return Err(Error::Encryption("refusing all-zeros key".to_string()));
        }

        // Plaintext header: version || group_id || user_id || sig_scheme ||
        // sig_public_key || epoch || storage_blob. The signature scheme + public
        // key let restore reconstruct the keypair via `SignatureKeyPair::read`
        // (the private half stays inside the AEAD-sealed storage blob). The
        // epoch is redundant with the group state so a stale snapshot is
        // detectable without fully loading the group.
        let mut plaintext = Vec::new();
        plaintext.push(SNAPSHOT_VERSION);
        put_bytes(&mut plaintext, self.group_id_bytes());
        put_bytes(&mut plaintext, self.user_id().as_bytes());
        let scheme = self.signature_scheme() as u16;
        plaintext.extend_from_slice(&scheme.to_le_bytes());
        put_bytes(&mut plaintext, self.signature_public_key());
        plaintext.extend_from_slice(&self.epoch().to_le_bytes());
        plaintext.extend_from_slice(&encode_storage(self.crypto_provider()));

        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce_bytes)
            .map_err(|e| Error::Mls(format!("nonce RNG failed: {e:?}")))?;
        let cipher = new_cipher(key);
        let ciphertext = cipher
            .encrypt(&Nonce::from(nonce_bytes), plaintext.as_ref())
            .map_err(|_| Error::Encryption("AEAD seal failed".to_string()))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Restore a group from an encrypted snapshot.
    ///
    /// Returns:
    /// - `Ok(Some(group))` on success (epoch preserved from the loaded state);
    /// - `Ok(None)` if the snapshot's epoch is older than `min_epoch` (stale →
    ///   caller does a clean re-join) or `MlsGroup::load` finds no group;
    /// - `Err` on decrypt / parse / version / epoch-mismatch failure.
    ///
    /// `min_epoch` lets the caller reject a snapshot that predates a known
    /// rotation learned out of band.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encryption`] for an all-zeros key, AEAD auth failure
    /// (wrong key / tampered blob), or a truncated/corrupt/wrong-version blob,
    /// and [`Error::Mls`] if the loaded group's epoch disagrees with the header.
    pub fn restore_encrypted(
        snapshot: &[u8],
        key: &[u8; 32],
        min_epoch: u64,
    ) -> Result<Option<Self>> {
        if is_all_zeros(key) {
            return Err(Error::Encryption("refusing all-zeros key".to_string()));
        }

        // Split nonce; AEAD-decrypt (wrong key / tamper -> Err, never partial).
        let nonce_slice = snapshot.get(..NONCE_LEN).ok_or_else(|| corrupt("no nonce"))?;
        let nonce_bytes: [u8; NONCE_LEN] =
            nonce_slice.try_into().map_err(|_| corrupt("bad nonce"))?;
        let sealed = &snapshot[NONCE_LEN..];
        let cipher = new_cipher(key);
        let plaintext = cipher.decrypt(&Nonce::from(nonce_bytes), sealed).map_err(|_| {
            Error::Encryption("AEAD open failed (wrong key or corrupt)".to_string())
        })?;

        // Parse header.
        let mut reader = Reader::new(&plaintext);
        let version = reader.u8()?;
        if version != SNAPSHOT_VERSION {
            return Err(corrupt(format!("unsupported version {version}")));
        }
        let group_id = reader.bytes()?.to_vec();
        let user_id = String::from_utf8(reader.bytes()?.to_vec())
            .map_err(|_| corrupt("user_id not utf-8"))?;
        let scheme_raw =
            u16::from_le_bytes(reader.take(2)?.try_into().map_err(|_| corrupt("bad scheme"))?);
        let scheme = SignatureScheme::try_from(scheme_raw).map_err(corrupt)?;
        let public_key = reader.bytes()?.to_vec();
        let header_epoch = reader.u64()?;

        // Stale: predates a known rotation -> caller re-joins (not an error).
        if header_epoch < min_epoch {
            return Ok(None);
        }

        // Repopulate a fresh provider's storage from the sealed map, then load.
        let crypto = OpenMlsRustCrypto::default();
        decode_storage_into(&mut reader, &crypto)?;

        let group = MlsGroup::load(crypto.storage(), &GroupId::from_slice(&group_id))
            .map_err(|e| Error::Mls(format!("Failed to load group: {e:?}")))?;
        let Some(group) = group else {
            return Ok(None);
        };

        // Reconstruct the signature keypair from the repopulated storage. The
        // private key is not directly extractable (its accessor is test-gated),
        // so we reload it via the persisted store using the header's public key.
        let signature_keys = SignatureKeyPair::read(crypto.storage(), &public_key, scheme)
            .ok_or_else(|| corrupt("signature keypair missing from storage"))?;

        // Rebuild the credential, mirroring `create`.
        let credential = BasicCredential::new(user_id.as_bytes().to_vec());
        let credential_with_key = CredentialWithKey {
            credential: credential.into(),
            signature_key: signature_keys.public().into(),
        };

        // Defense: the loaded group's epoch must match the header.
        if group.epoch().as_u64() != header_epoch {
            return Err(Error::Mls(format!(
                "epoch mismatch: header {header_epoch}, loaded {}",
                group.epoch().as_u64()
            )));
        }

        Ok(Some(Self::from_parts(user_id, group, crypto, signature_keys, credential_with_key)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 2-member group at epoch 1: Alice (owner) and Bob (joined).
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

    const KEY: [u8; 32] = [7u8; 32];

    /// True if `needle` appears as a contiguous byte window in `haystack`
    /// (mirrors #28's `containsBytes`). Empty needles never "match".
    fn contains_window(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Test-only: seal `plaintext` under an arbitrary `key` with a fixed nonce,
    /// deliberately bypassing `snapshot_encrypted`'s all-zeros guard so we can
    /// forge blobs (all-zeros-keyed, or with a corrupt inner storage encoding)
    /// that the restore path must reject. Uses the same cipher construction as
    /// production so the forged blob is a real AEAD ciphertext.
    fn seal_with_key(plaintext: &[u8], key: &[u8; 32]) -> Vec<u8> {
        let nonce_bytes = [0u8; NONCE_LEN];
        let cipher = new_cipher(key);
        let ciphertext = cipher.encrypt(&Nonce::from(nonce_bytes), plaintext).unwrap();
        let mut out = nonce_bytes.to_vec();
        out.extend_from_slice(&ciphertext);
        out
    }

    // Scenario 1: round-trip resume (THE acceptance test).
    #[test]
    fn test_restore_resumes_session_and_round_trips_with_untouched_member() {
        let (mut alice, mut bob) = two_member_group();
        // Alice has content and is at epoch 1.
        let hello = alice.encrypt(b"hello").unwrap();
        assert_eq!(bob.decrypt(&hello).unwrap(), b"hello");

        let snapshot = alice.snapshot_encrypted(&KEY).unwrap();
        drop(alice); // simulate restart: original in-memory group is gone.

        let mut alice2 = MlsDocumentGroup::restore_encrypted(&snapshot, &KEY, 0).unwrap().unwrap();
        assert_eq!(alice2.epoch(), 1, "epoch preserved across restore");

        // Post-restore messages round-trip with the untouched other member.
        let ct = alice2.encrypt(b"after restore").unwrap();
        assert_eq!(bob.decrypt(&ct).unwrap(), b"after restore");
        let ct2 = bob.encrypt(b"reply").unwrap();
        assert_eq!(alice2.decrypt(&ct2).unwrap(), b"reply");
    }

    // Scenario 2: epoch preserved across an advance (epoch 2 after an add).
    #[test]
    fn test_epoch_preserved_across_advance() {
        let (mut alice, _bob) = two_member_group(); // epoch 1
        let carol = MlsDocumentGroup::generate_key_package("carol").unwrap();
        let (_c, _w) = alice.add_member(carol.key_package()).unwrap();
        assert_eq!(alice.epoch(), 2);

        let snapshot = alice.snapshot_encrypted(&KEY).unwrap();
        let restored = MlsDocumentGroup::restore_encrypted(&snapshot, &KEY, 0).unwrap().unwrap();
        assert_eq!(restored.epoch(), 2);
    }

    // Scenario 3: NEGATIVE — wrong key returns Err (never a partial group).
    #[test]
    fn test_wrong_key_is_rejected() {
        let (alice, _bob) = two_member_group();
        let snapshot = alice.snapshot_encrypted(&KEY).unwrap();

        let wrong = [9u8; 32];
        let result = MlsDocumentGroup::restore_encrypted(&snapshot, &wrong, 0);
        assert!(result.is_err(), "wrong key must be Err, not a partial group");
    }

    // Scenario 4: NEGATIVE — all-zeros key rejected on BOTH snapshot and restore.
    #[test]
    fn test_all_zeros_key_rejected_on_snapshot() {
        let (alice, _bob) = two_member_group();
        let zeros = [0u8; 32];
        assert!(alice.snapshot_encrypted(&zeros).is_err(), "all-zeros snapshot must be Err");
    }

    #[test]
    fn test_all_zeros_key_rejected_on_restore() {
        // Forge a blob that WOULD fully restore under the all-zeros key if the
        // guard were absent: take a real snapshot, recover its valid plaintext,
        // and re-seal that same plaintext UNDER the all-zeros key. Now the only
        // thing standing between all-zeros and a successful restore is the
        // up-front guard. Proven load-bearing by the mutation that deletes the
        // guard -> this blob then restores to Ok(Some) and the test goes RED.
        let (alice, _bob) = two_member_group();
        let real = alice.snapshot_encrypted(&KEY).unwrap();
        let nonce: [u8; NONCE_LEN] = real[..NONCE_LEN].try_into().unwrap();
        let plaintext = new_cipher(&KEY).decrypt(&Nonce::from(nonce), &real[NONCE_LEN..]).unwrap();

        let zeros = [0u8; 32];
        let blob = seal_with_key(&plaintext, &zeros);
        assert!(
            MlsDocumentGroup::restore_encrypted(&blob, &zeros, 0).is_err(),
            "all-zeros restore must be Err via the guard"
        );
    }

    // Scenario 5: stale epoch -> clean re-join signal (Ok(None), not Err).
    #[test]
    fn test_stale_epoch_returns_none() {
        let (alice, _bob) = two_member_group(); // epoch 1
        let snapshot = alice.snapshot_encrypted(&KEY).unwrap();

        let restored = MlsDocumentGroup::restore_encrypted(&snapshot, &KEY, 5).unwrap();
        assert!(restored.is_none(), "epoch 1 snapshot with min_epoch 5 must be Ok(None)");
    }

    // Scenario 6: NEGATIVE — corrupt / truncated / wrong-version -> Err, no panic.
    #[test]
    fn test_random_bytes_rejected() {
        let garbage = vec![0xABu8; 200];
        assert!(MlsDocumentGroup::restore_encrypted(&garbage, &KEY, 0).is_err());
    }

    #[test]
    fn test_one_byte_blob_rejected() {
        assert!(MlsDocumentGroup::restore_encrypted(&[1u8], &KEY, 0).is_err());
    }

    #[test]
    fn test_empty_blob_rejected() {
        assert!(MlsDocumentGroup::restore_encrypted(&[], &KEY, 0).is_err());
    }

    #[test]
    fn test_flipped_version_rejected() {
        // Build a real snapshot, decrypt, flip the version byte, re-seal with the
        // same key, and confirm restore rejects it (version check fires).
        let (alice, _bob) = two_member_group();
        let snapshot = alice.snapshot_encrypted(&KEY).unwrap();

        let cipher = new_cipher(&KEY);
        let nonce_bytes: [u8; NONCE_LEN] = snapshot[..NONCE_LEN].try_into().unwrap();
        let nonce = Nonce::from(nonce_bytes);
        let mut pt = cipher.decrypt(&nonce, &snapshot[NONCE_LEN..]).unwrap();
        pt[0] = 0xFF; // corrupt the version byte
        let resealed = cipher.encrypt(&nonce, pt.as_ref()).unwrap();
        let mut blob = nonce_bytes.to_vec();
        blob.extend_from_slice(&resealed);

        assert!(MlsDocumentGroup::restore_encrypted(&blob, &KEY, 0).is_err());
    }

    // Scenario 7: plaintext-at-rest — the sealed blob does not leak the
    // in-storage identity in the clear (mirrors #28's containsBytes check).
    //
    // The needle is the `user_id`, which openmls writes into the member's
    // credential/leaf and thus into `MemoryStorage` (and into the plaintext
    // header before AEAD). So the needle GENUINELY lives in the sealed region:
    // if `snapshot_encrypted` ever emitted plaintext, the needle would show up
    // in the clear. Proven load-bearing by mutation (see the plaintext-emit
    // mutation on `snapshot_encrypted`, which turns this RED).
    #[test]
    fn test_snapshot_does_not_leak_plaintext() {
        const NEEDLE: &[u8] = b"NEEDLE-USERID-abc123";
        let (mut alice, _) = MlsDocumentGroup::create("NEEDLE-USERID-abc123").unwrap();
        let bob = MlsDocumentGroup::generate_key_package("bob").unwrap();
        alice.add_member(bob.key_package()).unwrap();

        let snapshot = alice.snapshot_encrypted(&KEY).unwrap();
        assert!(
            !contains_window(&snapshot, NEEDLE),
            "sealed snapshot must not contain the user_id in the clear"
        );
    }

    // Scenario 8: parser hardening — a blob whose inner storage encoding is
    // corrupt (a `count` far larger than the entries present) must be REJECTED
    // with Err after AEAD decrypt succeeds, never a panic / OOM / hang. This
    // reaches `decode_storage_into`'s checked slicing, unreachable through a
    // wrong-key path. Sealed under the REAL key so decrypt passes and the inner
    // parser is what fails.
    #[test]
    fn test_corrupt_inner_storage_rejected() {
        // Valid header up to the storage blob, then a lying storage encoding:
        // a small count but zero entries follow -> the first entry read hits EOF
        // in the *inner* parser. The header must parse fully (a REAL signature
        // scheme, not a rejected placeholder) so header parsing succeeds and the
        // inner storage parser is what triggers the Err.
        let mut plaintext = Vec::new();
        plaintext.push(SNAPSHOT_VERSION);
        put_bytes(&mut plaintext, b"group-id"); // group_id
        put_bytes(&mut plaintext, b"alice"); // user_id
        plaintext.extend_from_slice(&(SignatureScheme::ED25519 as u16).to_le_bytes()); // sig scheme
        put_bytes(&mut plaintext, b"pubkey"); // sig public key
        plaintext.extend_from_slice(&0u64.to_le_bytes()); // epoch
        plaintext.extend_from_slice(&3u64.to_le_bytes()); // storage count, no entries follow

        let blob = seal_with_key(&plaintext, &KEY);
        let Err(err) = MlsDocumentGroup::restore_encrypted(&blob, &KEY, 0) else {
            panic!("corrupt inner storage encoding must be Err (not panic/OOM)");
        };
        assert!(
            err.to_string().contains("unexpected end"),
            "must fail inside the inner storage parser, not at header/scheme parse; got: {err}"
        );
    }

    // Scenario 9: resource bound — a snapshot whose declared storage `count`
    // exceeds MAX_STORAGE_ENTRIES is rejected BEFORE the decode loop allocates,
    // even though it AEAD-decrypts under the real key. Defense-in-depth
    // (CLAUDE.md resource-bounds): a huge count is a corrupt/hostile snapshot.
    #[test]
    fn test_storage_count_over_cap_rejected() {
        let mut plaintext = Vec::new();
        plaintext.push(SNAPSHOT_VERSION);
        put_bytes(&mut plaintext, b"group-id");
        put_bytes(&mut plaintext, b"alice");
        plaintext.extend_from_slice(&(SignatureScheme::ED25519 as u16).to_le_bytes());
        put_bytes(&mut plaintext, b"pubkey");
        plaintext.extend_from_slice(&0u64.to_le_bytes()); // epoch
                                                          // count just over the cap, with NO entries following: pre-cap this errors
                                                          // on take() EOF ("unexpected end"); post-cap it errors on the count check.
        plaintext.extend_from_slice(&(MAX_STORAGE_ENTRIES + 1).to_le_bytes());

        let blob = seal_with_key(&plaintext, &KEY);
        let Err(err) = MlsDocumentGroup::restore_encrypted(&blob, &KEY, 0) else {
            panic!("over-cap storage count must be Err (not OOM/hang)");
        };
        assert!(
            err.to_string().contains("exceeds cap"),
            "must be rejected by the entry-count cap, not a generic EOF; got: {err}"
        );
    }

    // Scenario 10: a poisoned storage RwLock must NOT crash snapshot/restore.
    // A snapshot may be taken during error recovery, so poison-recovery keeps it
    // fail-soft. Poison the lock, then exercise both the read (encode) and write
    // (decode) paths: with `.expect(...)` these panic; with poison-recovery they
    // return normally.
    #[test]
    fn test_poisoned_storage_lock_recovers_not_panics() {
        let crypto = OpenMlsRustCrypto::default();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = crypto.storage().values.write().unwrap();
            panic!("poison the lock");
        }));
        // Read path (encode_storage): must recover the guard, not panic.
        let _ = encode_storage(&crypto);
        // Write path (decode_storage_into): a valid empty map (count = 0).
        let buf = 0u64.to_le_bytes();
        let mut reader = Reader::new(&buf);
        decode_storage_into(&mut reader, &crypto).expect("empty map must decode after poison");
    }
}
