# Issue #30 — Persist MLS group state across restart (encrypted at rest)

Date: 2026-07-30
Branch: `feat/30-persist-group-state` (off main)
Status: design (implement per this)

## Problem

MLS group state lives only in memory (`OpenMlsRustCrypto`'s in-memory
`MemoryStorage`). A restart throws it away and forces a full re-join. #30 persists
the state encrypted at rest and resumes on startup without re-joining, preserving
the epoch; a too-old persisted epoch falls back to a clean re-join.

## Verified primitives (openmls 0.7.4 / openmls_rust_crypto 0.4.4)

- `MemoryStorage` has a **public field** `pub values: RwLock<HashMap<Vec<u8>,
  Vec<u8>>>` — this is the snapshot surface we use.
- ⚠️ `MemoryStorage::serialize`/`deserialize`/`Clone` EXIST but are gated behind
  `#[cfg(feature = "test-utils")]`, so they are NOT available in a normal build —
  DO NOT use them. Instead serialize the `pub values` HashMap ourselves: read the
  `RwLock`, iterate the `HashMap<Vec<u8>, Vec<u8>>`, and length-prefix-encode the
  `(k, v)` entries (or `serde`-encode the `HashMap` directly — serde_json is a
  workspace dep, but the keys/values are arbitrary bytes so use a bincode-free
  manual length-prefixed encoding, or serialize as `Vec<(Vec<u8>, Vec<u8>)>` with
  serde_json using base64/byte-array handling; simplest: a manual
  `u64 count || (u64 klen || u64 vlen || k || v)*` matching the test-utils layout,
  in our OWN code so it's not feature-gated). This is the sync snapshot the issue
  wants; the async StorageProvider is explicitly NOT used.
- openmls auto-persists group state to `provider.storage()` on every
  commit/merge/create — so the in-memory storage is ALWAYS current; we don't hook
  each mutation, we just snapshot the storage.
- The signature keys are stored in the SAME `MemoryStorage`
  (`signature_keys.store(crypto.storage())` in create/PendingMember), so
  serializing the storage captures them too.
- `MlsGroup::load(storage, group_id) -> Result<Option<MlsGroup>>` reconstructs a
  group from a storage provider. `group.group_id() -> &GroupId` gives the id.
- CAVEAT: `OpenMlsRustCrypto` has NO public constructor from a `MemoryStorage`
  (its `key_store` field is private). BUT `MemoryStorage.values` is `pub`. So
  restore = `OpenMlsRustCrypto::default()`, then repopulate its
  `storage().values` from the deserialized map (public field, interior
  mutability), then `MlsGroup::load`.

This is exactly the issue's "snapshot/restore the in-memory MemoryStorage, NOT an
async StorageProvider" — the trait stays sync; we serialize the concrete backend.

## At-rest encryption

Add `aes-gcm` to `crates/collab-core/Cargo.toml` (+ workspace deps if not already
there — it's in Cargo.lock via collab-wasm). It is NOT currently a collab-core dep,
so ADD it. `chacha20poly1305` is also in-tree if aes-gcm proves awkward; prefer
aes-gcm for consistency with the (now-removed-from-wasm) prior AES path and the MLS
ciphersuite's AES-128-GCM. Encrypt the snapshot blob with a caller-supplied 32-byte
key + random nonce (nonce prepended), AES-256-GCM. Native-only path (the CLI/plugin
data-dir), so no wasm RNG concern — use `aes_gcm` with an OS-random nonce
(`getrandom` or `aes_gcm::aead::OsRng`); if collab-core must stay wasm-buildable,
gate the nonce source or take the nonce/rng from the caller. VERIFY
`cargo build -p collab-core --target wasm32-unknown-unknown --features wasm-clock`
still passes after adding aes-gcm (aes-gcm is pure-Rust and wasm-safe; the concern
is only the RNG for the nonce — if OsRng doesn't build on wasm, have the caller
pass a 12-byte nonce, or gate the snapshot API to native).
The key's PROVENANCE (OS keychain vs user passphrase) is the CALLER's concern
(CLI/plugin) and out of scope here — collab-core takes a `&[u8; 32]` key and does
AEAD. Reject an all-zeros key (fail-closed, per CLAUDE.md — same rule as #27/#28).

## collab-core API (new module: crates/collab-core/src/persistence.rs)

```rust
/// Versioned, encrypted-at-rest snapshot of a group's MLS state.
/// Layout (plaintext, before AEAD): version(u8) || group_id_len(u32) || group_id
///   || user_id_len || user_id || owner_id_len || owner_id || epoch(u64)
///   || memory_storage_serialize(...)
/// The epoch is stored in the header (redundant with the group state) so a stale
/// snapshot can be detected WITHOUT fully loading the group.
pub const SNAPSHOT_VERSION: u8 = 1;

impl MlsDocumentGroup {
    /// Snapshot + encrypt this group's full MLS state (group + signature keys).
    /// `key` is a 32-byte AEAD key (all-zeros rejected). AEAD = AES-256-GCM,
    /// nonce prepended. Deterministic-free (random nonce) so callers can't reuse.
    pub fn snapshot_encrypted(&self, key: &[u8; 32]) -> Result<Vec<u8>>;

    /// Restore a group from an encrypted snapshot. Returns:
    /// - Ok(Some(group)) on success (epoch preserved from the loaded state);
    /// - Ok(None) if the snapshot's epoch is older than `min_epoch` (stale →
    ///   caller does a clean re-join) OR MlsGroup::load finds no group;
    /// - Err on decrypt/parse/version failure (corrupt → caller re-joins).
    /// `min_epoch` lets the caller reject a snapshot that predates a known
    /// rotation (e.g. learned out-of-band that the group is now at epoch N).
    pub fn restore_encrypted(snapshot: &[u8], key: &[u8; 32], min_epoch: u64)
        -> Result<Option<Self>>;
    // impl: AEAD-decrypt; parse header; check version == SNAPSHOT_VERSION (else Err);
    //   if header.epoch < min_epoch -> Ok(None) [stale, fail to clean re-join];
    //   crypto = OpenMlsRustCrypto::default();
    //   ms = MemoryStorage::deserialize(&mut &blob[..])?;
    //   { let mut v = crypto.storage().values.write().unwrap();
    //     *v = ms.values.into_inner().unwrap(); }
    //   group = MlsGroup::load(crypto.storage(), &GroupId::from_slice(gid))?
    //             .ok_or -> Ok(None);
    //   signature_keys = SignatureKeyPair::read(crypto.storage(), group's own
    //             signature public key, ciphersuite) -> reload from storage;
    //   reconstruct MlsDocumentGroup { user_id, owner_id, group, crypto,
    //             signature_keys, _credential_with_key }.
    //   VERIFY group.epoch() == header.epoch (defense: header must match state).
}
```

The signature-key reload is the fiddly part: `SignatureKeyPair::read(storage,
public_key_bytes, signature_scheme)` re-reads the stored keypair. The public key is
recoverable from the group's own leaf (own_leaf_node's signature_key) after
`MlsGroup::load`. Read the openmls_basic_credential `SignatureKeyPair::read` API and
wire it; if `read` needs the public key, get it from the loaded group's own leaf
credential. If this proves genuinely blocked, the fallback is to ALSO stash the
signature keypair's serialized form in the snapshot header and reconstruct it — but
prefer reloading from the persisted storage (it's already there).

## wasm

The plugin (#30's Obsidian target) will call this via the wasm surface eventually,
but wiring the plugin data-dir persistence is a SEPARATE concern (like #28's plugin
rewire). For #30 scope: implement + test in collab-core (native), and add the wasm
pass-throughs `WasmEncryptedDocument::snapshot_encrypted(key)` /
`restore_encrypted(snapshot, key, min_epoch)` so the surface exists. Do NOT wire the
Obsidian plugin data-dir storage in this PR (YAGNI for the acceptance criteria;
note as follow-up). The CLI/watcher config-dir storage is likewise a thin caller —
add a minimal CLI persist/resume only if it fits cleanly; otherwise note as
follow-up and keep #30 to the collab-core mechanism + its tests.

## BDD scenarios → RED-first tests (collab-core)

1. **Round-trip resume (THE acceptance test):** GIVEN a 2-member group where Alice
   is at epoch 1 with content "hello", WHEN Alice snapshots (encrypted), drops the
   in-memory group, and `restore_encrypted` with the same key + min_epoch 0, THEN
   the restored group has epoch == 1 and can still `encrypt`/`decrypt` with Bob
   (the OTHER member, untouched) — i.e. a message Alice sends post-restore decrypts
   for Bob, and vice versa. NO re-join happened. Mutation-check: if restore loaded
   a fresh/empty group, the epoch would be 0 and the round-trip with Bob would
   fail → test RED.
2. **Epoch preserved across an advance:** snapshot at epoch 2 (after an add),
   restore, assert epoch == 2.
3. **NEGATIVE — wrong key:** `restore_encrypted` with a DIFFERENT key returns Err
   (AEAD auth fail), never a partial/garbage group.
4. **NEGATIVE — all-zeros key rejected** on BOTH snapshot and restore (fail-closed).
5. **Stale epoch → clean re-join signal:** a snapshot taken at epoch 1, restored
   with `min_epoch = 5`, returns `Ok(None)` (caller re-joins) — NOT an error, NOT a
   stale group.
6. **NEGATIVE — corrupt/truncated/wrong-version blob:** returns Err, no panic.
7. **Plaintext-at-rest check:** assert the encrypted snapshot does NOT contain the
   plaintext content "hello" bytes nor an obvious group-id substring in the clear
   (the blob is ciphertext). (Mirrors #28's "plaintext not a window of ciphertext".)

## docs/security.md

- "No key persistence: MLS group state is in-memory only. Restarting a client
  requires re-joining." → REPLACE: group state is persisted encrypted-at-rest
  (AES-256-GCM) and resumed on restart without re-join; the at-rest key provenance
  (OS keychain / passphrase) is the client's responsibility; a stale persisted
  epoch falls back to a clean re-join. Note the Obsidian/CLI data-dir wiring is
  follow-up.
- Roadmap: check off "Persist MLS group state for session resumption."

## Ponytail

No new mechanism where openmls already provides one: `MemoryStorage::serialize`
/`deserialize` do the snapshot. One crypto dep (aes-gcm/chacha, already in tree) for
the at-rest AEAD. No async StorageProvider (the issue explicitly forbids it; the
trait is sync and we serialize the concrete backend). Don't build the plugin/CLI
data-dir wiring now — collab-core mechanism + tests satisfy the acceptance criteria.
