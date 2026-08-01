//! wasm-bindgen surface for vault manifest sync (issue #32).
//!
//! Thin owning wrapper over collab-core's `VaultSyncManager`. Like the MLS
//! surface (see `mls.rs`) it wraps pure collab-core value types and surfaces
//! failures as real JS `Error`s via `reg_err`; no sync logic lives here.
//!
//! The manifest CRDT bytes this produces are transported encrypted under a
//! dedicated MLS group on [`manifest_doc_id`] — the crypto is the caller's
//! (`collab-client.ts`) job, mirroring how file docs get their own group.

use collab_core::{
    RegistryError, SyncAction, SyncActionKind, VaultSyncConfig, VaultSyncManager, MANIFEST_DOC_ID,
    MAX_MANIFEST_UPDATE_BYTES,
};
use wasm_bindgen::prelude::*;

#[cfg(test)]
use collab_core::MAX_NEW_PATHS_PER_APPLY;

/// The well-known manifest document id, exposed so TypeScript can assert it
/// matches the Rust constant instead of hard-coding the string.
#[wasm_bindgen]
pub fn manifest_doc_id() -> String {
    MANIFEST_DOC_ID.to_string()
}

fn reg_err(e: RegistryError) -> JsError {
    JsError::new(&e.to_string())
}

/// JS-friendly view of a `collab_core::SyncAction`.
///
/// `kind` is one of `"created" | "deleted" | "renamed" | "ignored"`. `new_path`
/// is set only for a rename. `manifest_update` is the CRDT broadcast to send to
/// peers, and is empty when the event was ignored (outside sync scope).
/// (The source path is not exposed: the caller passed it in, so echoing it back
/// would be dead surface.)
#[wasm_bindgen]
pub struct WasmSyncAction {
    kind: String,
    new_path: Option<String>,
    manifest_update: Vec<u8>,
}

impl WasmSyncAction {
    fn from_action(action: SyncAction) -> Self {
        let (kind, new_path) = match action.kind {
            SyncActionKind::FileCreated => ("created", None),
            SyncActionKind::FileDeleted => ("deleted", None),
            SyncActionKind::FileRenamed { new_path } => ("renamed", Some(new_path)),
            SyncActionKind::Ignored => ("ignored", None),
        };
        Self { kind: kind.to_string(), new_path, manifest_update: action.manifest_update }
    }
}

#[wasm_bindgen]
impl WasmSyncAction {
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn new_path(&self) -> Option<String> {
        self.new_path.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn manifest_update(&self) -> Vec<u8> {
        self.manifest_update.clone()
    }
}

/// wasm-bindgen wrapper around collab-core's `VaultSyncManager`.
#[wasm_bindgen]
pub struct WasmVaultSync(VaultSyncManager);

impl WasmVaultSync {
    /// Apply a remote manifest update.
    ///
    /// Both bounds are manifest invariants enforced inside
    /// `collab_core::VaultSyncManager::apply_remote_manifest` itself, so every
    /// caller (this binding, e2e tests, a future CLI) gets them "for free":
    /// the byte bound ([`MAX_MANIFEST_UPDATE_BYTES`]) rejects anything larger
    /// than a legitimate relay frame before any decode work, and the new-path
    /// count bound (`MAX_NEW_PATHS_PER_APPLY`) is checked against a scratch
    /// copy *before* the CRDT merge. This binding additionally re-checks the
    /// byte bound as a fast path, rejecting an oversized frame before it ever
    /// crosses the wasm boundary.
    ///
    /// The `#[wasm_bindgen]` `apply_remote_manifest` delegates here and maps the
    /// error to a JS `Error`; this variant lets native tests assert the failure
    /// path without constructing a `JsError` (which panics off-wasm).
    pub(crate) fn apply_remote_manifest_internal(
        &mut self,
        update: &[u8],
    ) -> Result<Vec<String>, String> {
        if update.len() > MAX_MANIFEST_UPDATE_BYTES {
            return Err(format!(
                "manifest update of {} bytes exceeds the {MAX_MANIFEST_UPDATE_BYTES}-byte cap",
                update.len()
            ));
        }
        self.0.apply_remote_manifest(update).map_err(|e| e.to_string())
    }
}

#[wasm_bindgen]
impl WasmVaultSync {
    /// Build a sync manager. Extension filtering keeps collab-core's default
    /// (`md`); callers control folders, exclusions, and deletion/rename policy.
    #[wasm_bindgen(constructor)]
    pub fn new(
        sync_folders: Vec<String>,
        exclude_patterns: Vec<String>,
        sync_deletions: bool,
        sync_renames: bool,
    ) -> Self {
        let config = VaultSyncConfig {
            sync_folders,
            exclude_patterns,
            sync_deletions,
            sync_renames,
            ..VaultSyncConfig::default()
        };
        Self(VaultSyncManager::new(config))
    }

    /// Handle a local file-creation event.
    ///
    /// # Errors
    /// Returns a JS `Error` if the document cannot be registered.
    pub fn handle_created(&mut self, path: String) -> Result<WasmSyncAction, JsError> {
        self.0.handle_created(&path).map(WasmSyncAction::from_action).map_err(reg_err)
    }

    /// Handle a local file-deletion event (infallible in core).
    pub fn handle_deleted(&mut self, path: String) -> WasmSyncAction {
        WasmSyncAction::from_action(self.0.handle_deleted(&path))
    }

    /// Handle a local file-rename event.
    ///
    /// # Errors
    /// Returns a JS `Error` if the renamed document cannot be registered.
    pub fn handle_renamed(
        &mut self,
        old_path: String,
        new_path: String,
    ) -> Result<WasmSyncAction, JsError> {
        self.0
            .handle_renamed(&old_path, &new_path)
            .map(WasmSyncAction::from_action)
            .map_err(reg_err)
    }

    /// Apply a manifest update from a remote peer; returns newly-registered paths.
    ///
    /// # Errors
    /// Returns a JS `Error` if the update bytes are malformed, oversized, or
    /// announce more new paths than the per-apply cap.
    pub fn apply_remote_manifest(&mut self, update: &[u8]) -> Result<Vec<String>, JsError> {
        self.apply_remote_manifest_internal(update).map_err(|e| JsError::new(&e))
    }

    /// The alive (non-deleted) file paths currently in the manifest.
    pub fn list_files(&self) -> Vec<String> {
        self.0.manifest().list_files()
    }
}

#[cfg(test)]
mod tests {
    //! Native tests for the `WasmVaultSync` wrapper. These exercise the pure
    //! collab-core logic through the binding; the error path is asserted via the
    //! `_internal` variant so no `JsError` is constructed (that panics off-wasm).
    use super::*;

    fn sync() -> WasmVaultSync {
        WasmVaultSync::new(Vec::new(), Vec::new(), true, true)
    }

    #[test]
    fn test_created_produces_update_and_syncs_to_peer() {
        let mut alice = sync();
        let action = alice.handle_created("notes/ideas.md".to_string()).unwrap();
        assert_eq!(action.kind(), "created");
        assert!(action.new_path().is_none());
        assert!(!action.manifest_update().is_empty());

        // A fresh peer that only sees the manifest bytes registers the file.
        let mut bob = sync();
        let newly = bob.apply_remote_manifest(&action.manifest_update()).unwrap();
        assert_eq!(newly, vec!["notes/ideas.md".to_string()]);
        assert!(bob.list_files().contains(&"notes/ideas.md".to_string()));
    }

    #[test]
    fn test_deleted_tombstones() {
        let mut mgr = sync();
        mgr.handle_created("notes/ideas.md".to_string()).unwrap();
        let action = mgr.handle_deleted("notes/ideas.md".to_string());
        assert_eq!(action.kind(), "deleted");
        assert!(action.new_path().is_none());
        assert!(!action.manifest_update().is_empty());
        assert!(!mgr.list_files().contains(&"notes/ideas.md".to_string()));
    }

    #[test]
    fn test_renamed_is_atomic() {
        let mut mgr = sync();
        mgr.handle_created("old.md".to_string()).unwrap();
        let action = mgr.handle_renamed("old.md".to_string(), "new.md".to_string()).unwrap();
        assert_eq!(action.kind(), "renamed");
        assert_eq!(action.new_path(), Some("new.md".to_string()));
        assert!(!action.manifest_update().is_empty());
        let files = mgr.list_files();
        assert!(files.contains(&"new.md".to_string()));
        assert!(!files.contains(&"old.md".to_string()));
    }

    #[test]
    fn test_apply_malformed_manifest_errs_without_panic() {
        let mut mgr = sync();
        let res = mgr.apply_remote_manifest_internal(&[0xFF, 0x00, 0x01]);
        assert!(res.is_err(), "malformed manifest bytes must error, not panic");
    }

    #[test]
    fn test_oversized_manifest_update_rejected() {
        // A VALID but oversized update (giant path) built from a source instance:
        // the byte cap must reject it before any decode work.
        let mut src = sync();
        let giant_path = format!("{}.md", "a".repeat(MAX_MANIFEST_UPDATE_BYTES));
        let action = src.handle_created(giant_path).unwrap();
        assert!(action.manifest_update().len() > MAX_MANIFEST_UPDATE_BYTES);

        let mut peer = sync();
        let res = peer.apply_remote_manifest_internal(&action.manifest_update());
        assert!(res.is_err(), "update over MAX_MANIFEST_UPDATE_BYTES must be rejected");
        assert!(peer.list_files().is_empty(), "rejected update must not register paths");
    }

    #[test]
    fn test_path_count_bomb_rejected_whole() {
        // One update announcing more than MAX_NEW_PATHS_PER_APPLY files must be
        // rejected as a whole (fail closed), never silently truncated.
        let mut src = sync();
        for i in 0..=MAX_NEW_PATHS_PER_APPLY {
            src.handle_created(format!("f{i}.md")).unwrap();
        }
        let full_state = src.0.manifest().encode_full_state();

        let mut peer = sync();
        let res = peer.apply_remote_manifest_internal(&full_state);
        assert!(res.is_err(), "path-count bomb must be rejected whole, not truncated");
        // The invariant this test is named for: rejection must mean the CRDT
        // merge never happened at all, not just that the registry create loop
        // stopped partway through. If the manifest is non-empty here, the
        // update was applied in full and only *reported* as failed.
        assert!(
            peer.list_files().is_empty(),
            "rejected path-count bomb must leave the peer's manifest untouched"
        );

        // A legitimate follow-up update must still apply cleanly — proving the
        // rejected bomb left no partial/corrupted state behind.
        let mut good_src = sync();
        let action = good_src.handle_created("fine.md".to_string()).unwrap();
        let newly = peer.apply_remote_manifest(&action.manifest_update()).unwrap();
        assert_eq!(newly, vec!["fine.md".to_string()]);
    }

    #[test]
    fn test_out_of_scope_create_is_ignored() {
        let mut mgr = WasmVaultSync::new(vec!["work".to_string()], Vec::new(), true, true);
        let action = mgr.handle_created("personal/diary.md".to_string()).unwrap();
        assert_eq!(action.kind(), "ignored");
        assert!(action.manifest_update().is_empty());
    }

    #[test]
    fn test_manifest_doc_id_matches_core_const() {
        assert_eq!(manifest_doc_id(), "__vault_manifest__");
    }
}
