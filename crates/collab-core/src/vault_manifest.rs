//! CRDT-backed vault manifest for tracking all files in a collaborative vault.
//!
//! The [`VaultManifest`] is a Yrs document that uses a [`yrs::Map`] to record
//! every file path in the vault together with its *alive* / *deleted* state.
//! Because it is itself a CRDT, concurrent creates and deletes from different
//! peers converge automatically: the last-write-wins semantic of the Yrs Map
//! ensures that all peers eventually agree on the same set of file entries.
//!
//! # Deletion tombstones
//!
//! Files are never silently removed from the map — when a file is deleted its
//! entry is set to `false` (a tombstone). This prevents the "re-creation" race
//! where an `add_file` from a lagging peer would resurrect a file that another
//! peer already deleted.
//!
//! # Rename semantics
//!
//! A rename is modelled as `delete_file(old_path)` + `add_file(new_path)`.
//! Both operations are committed in a single Yrs transaction so the update
//! bytes are atomic.

use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, MapRef, Out, ReadTxn, Transact, Update};

use crate::{Error, Result};

/// The well-known document identifier used for the vault manifest.
///
/// All clients must subscribe to this ID to participate in vault-level sync.
pub const MANIFEST_DOC_ID: &str = "__vault_manifest__";

/// The name of the Yrs map inside the manifest document.
const MANIFEST_MAP_KEY: &str = "files";

/// CRDT-backed file registry for a collaborative vault.
///
/// Internally backed by a Yrs [`Doc`] with a single [`MapRef`] whose keys are
/// UTF-8 file paths and whose values are booleans:
/// - `true`  → the file is *alive* (exists in the vault)
/// - `false` → the file has been *deleted* (tombstone)
pub struct VaultManifest {
    doc: Doc,
    map: MapRef,
}

impl VaultManifest {
    /// Create a new, empty vault manifest.
    #[must_use]
    pub fn new() -> Self {
        let doc = Doc::new();
        let map = doc.get_or_insert_map(MANIFEST_MAP_KEY);
        Self { doc, map }
    }

    /// Mark `path` as alive in the manifest.
    ///
    /// If the path was previously deleted its tombstone is overwritten.
    pub fn add_file(&self, path: &str) {
        let mut txn = self.doc.transact_mut();
        self.map.insert(&mut txn, path, true);
    }

    /// Mark `path` as deleted (tombstone) in the manifest.
    ///
    /// If the path is not yet known it is added with a deleted state.
    pub fn delete_file(&self, path: &str) {
        let mut txn = self.doc.transact_mut();
        self.map.insert(&mut txn, path, false);
    }

    /// Rename `old_path` to `new_path` atomically.
    ///
    /// The old path becomes a tombstone and `new_path` becomes alive in a single
    /// transaction, so no intermediate state is visible to remote peers.
    pub fn rename_file(&self, old_path: &str, new_path: &str) {
        let mut txn = self.doc.transact_mut();
        self.map.insert(&mut txn, old_path, false);
        self.map.insert(&mut txn, new_path, true);
    }

    /// Return the paths of all *alive* (non-deleted) files.
    #[must_use]
    pub fn list_files(&self) -> Vec<String> {
        let txn = self.doc.transact();
        self.map
            .iter(&txn)
            .filter_map(|(key, value)| if is_alive(&value) { Some(key.to_string()) } else { None })
            .collect()
    }

    /// Return `true` if `path` is alive in the manifest.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        let txn = self.doc.transact();
        matches!(self.map.get(&txn, path), Some(Out::Any(yrs::Any::Bool(true))))
    }

    /// Return `true` if `path` has a deletion tombstone in the manifest.
    #[must_use]
    pub fn is_deleted(&self, path: &str) -> bool {
        let txn = self.doc.transact();
        matches!(self.map.get(&txn, path), Some(Out::Any(yrs::Any::Bool(false))))
    }

    /// Encode the *full* document state as a Yrs v1 update.
    ///
    /// Use this to initialise a new peer that has no prior knowledge of the manifest.
    #[must_use]
    pub fn encode_full_state(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    }

    /// Encode only the changes that have occurred *since* `state_vector`.
    ///
    /// `state_vector` should have been obtained from the remote peer via
    /// [`state_vector`](Self::state_vector).  If the vector is empty the result
    /// is identical to [`encode_full_state`](Self::encode_full_state).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Yrs`] if `state_vector` cannot be decoded.
    pub fn encode_update_since(&self, state_vector: &[u8]) -> Result<Vec<u8>> {
        let txn = self.doc.transact();
        let sv = if state_vector.is_empty() {
            yrs::StateVector::default()
        } else {
            yrs::StateVector::decode_v1(state_vector).map_err(|e| Error::Yrs(e.to_string()))?
        };
        Ok(txn.encode_state_as_update_v1(&sv))
    }

    /// Return the current state vector (for incremental sync).
    #[must_use]
    pub fn state_vector(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.state_vector().encode_v1()
    }

    /// Apply an update received from a remote peer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Yrs`] if `update` cannot be decoded or applied.
    pub fn apply_update(&self, update: &[u8]) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        txn.apply_update(Update::decode_v1(update).map_err(|e| Error::Yrs(e.to_string()))?)
            .map_err(|e| Error::Yrs(e.to_string()))
    }
}

impl Default for VaultManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns `true` for an `Out` value that represents a live file (`bool == true`).
const fn is_alive(value: &Out) -> bool {
    matches!(value, Out::Any(yrs::Any::Bool(true)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ── Basic add/delete/rename ─────────────────────────────────

    #[test]
    fn test_new_manifest_is_empty() {
        let m = VaultManifest::new();
        assert!(m.list_files().is_empty());
    }

    #[test]
    fn test_add_file_appears_in_list() {
        let m = VaultManifest::new();
        m.add_file("notes/hello.md");
        assert!(m.contains("notes/hello.md"));
        assert_eq!(m.list_files(), vec!["notes/hello.md".to_string()]);
    }

    #[test]
    fn test_delete_file_leaves_tombstone() {
        let m = VaultManifest::new();
        m.add_file("notes/hello.md");
        m.delete_file("notes/hello.md");

        assert!(!m.contains("notes/hello.md"));
        assert!(m.is_deleted("notes/hello.md"));
        assert!(m.list_files().is_empty());
    }

    #[test]
    fn test_re_add_after_delete_resurfaces_file() {
        let m = VaultManifest::new();
        m.add_file("notes/hello.md");
        m.delete_file("notes/hello.md");
        m.add_file("notes/hello.md");

        assert!(m.contains("notes/hello.md"));
        assert!(!m.is_deleted("notes/hello.md"));
        assert_eq!(m.list_files(), vec!["notes/hello.md".to_string()]);
    }

    #[test]
    fn test_rename_file() {
        let m = VaultManifest::new();
        m.add_file("old.md");
        m.rename_file("old.md", "new.md");

        assert!(!m.contains("old.md"));
        assert!(m.is_deleted("old.md"));
        assert!(m.contains("new.md"));
        assert_eq!(m.list_files(), vec!["new.md".to_string()]);
    }

    #[test]
    fn test_delete_unknown_file_creates_tombstone() {
        let m = VaultManifest::new();
        m.delete_file("ghost.md");
        assert!(!m.contains("ghost.md"));
        assert!(m.is_deleted("ghost.md"));
    }

    #[test]
    fn test_multiple_files() {
        let m = VaultManifest::new();
        m.add_file("a.md");
        m.add_file("b.md");
        m.add_file("c.md");
        m.delete_file("b.md");

        let mut files = m.list_files();
        files.sort();
        assert_eq!(files, vec!["a.md".to_string(), "c.md".to_string()]);
    }

    // ── State vector & incremental sync ────────────────────────

    #[test]
    fn test_state_vector_is_not_empty_after_mutation() {
        let m = VaultManifest::new();
        let sv_before = m.state_vector();
        m.add_file("notes/hello.md");
        let sv_after = m.state_vector();
        // The state vector should differ after a write.
        assert_ne!(sv_before, sv_after);
    }

    #[test]
    fn test_apply_update_merges_remote_files() {
        // Alice adds some files.
        let alice = VaultManifest::new();
        alice.add_file("alice.md");

        // Bob applies Alice's full state.
        let bob = VaultManifest::new();
        let update = alice.encode_full_state();
        bob.apply_update(&update).unwrap();

        assert!(bob.contains("alice.md"));
    }

    #[test]
    fn test_bidirectional_convergence() {
        // Both Alice and Bob start fresh and each add a file independently.
        let alice = VaultManifest::new();
        alice.add_file("alice.md");

        let bob = VaultManifest::new();
        bob.add_file("bob.md");

        // Exchange full states.
        alice.apply_update(&bob.encode_full_state()).unwrap();
        bob.apply_update(&alice.encode_full_state()).unwrap();

        // Both should now contain both files.
        let mut alice_files = alice.list_files();
        alice_files.sort();
        let mut bob_files = bob.list_files();
        bob_files.sort();

        assert_eq!(alice_files, vec!["alice.md".to_string(), "bob.md".to_string()]);
        assert_eq!(bob_files, vec!["alice.md".to_string(), "bob.md".to_string()]);
    }

    #[test]
    fn test_concurrent_delete_and_add_last_write_wins() {
        // Concurrent: Alice deletes "shared.md", Bob adds it at the same time.
        // Because Yrs Map is last-write-wins, the result depends on client ID /
        // clock ordering — but both converge to the *same* value.
        let alice = VaultManifest::new();
        alice.add_file("shared.md");
        let initial_update = alice.encode_full_state();

        let bob = VaultManifest::new();
        bob.apply_update(&initial_update).unwrap();

        // At this point both know about "shared.md".
        // Now they diverge: Alice deletes, Bob modifies (re-adds) concurrently.
        alice.delete_file("shared.md");
        bob.add_file("shared.md");

        // Merge both ways.
        alice.apply_update(&bob.encode_full_state()).unwrap();
        bob.apply_update(&alice.encode_full_state()).unwrap();

        // Convergence check: both agree on the same value for "shared.md".
        assert_eq!(
            alice.contains("shared.md"),
            bob.contains("shared.md"),
            "Alice and Bob must converge to the same state for 'shared.md'"
        );
    }

    #[test]
    fn test_incremental_update_since_state_vector() {
        // The correct incremental sync scenario: Bob syncs Alice's full state
        // first, then Alice adds more files, and Bob requests only the delta.
        let alice = VaultManifest::new();
        alice.add_file("first.md");

        // Bob receives Alice's full state (first sync).
        let bob = VaultManifest::new();
        bob.apply_update(&alice.encode_full_state()).unwrap();
        assert!(bob.contains("first.md"), "Bob should have first.md after initial sync");

        // Bob records his state vector to use for the next incremental request.
        let bob_sv = bob.state_vector();

        // Alice now adds "second.md".
        alice.add_file("second.md");

        // Alice generates an incremental update starting from Bob's state vector.
        let delta = alice.encode_update_since(&bob_sv).unwrap();

        // Bob applies only the incremental update.
        bob.apply_update(&delta).unwrap();

        assert!(bob.contains("first.md"), "Bob should still have first.md");
        assert!(bob.contains("second.md"), "Bob should gain second.md from the delta");
    }

    #[test]
    fn test_manifest_doc_id_constant() {
        assert_eq!(MANIFEST_DOC_ID, "__vault_manifest__");
    }
}
