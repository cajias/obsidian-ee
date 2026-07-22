//! Vault synchronization configuration and manager.
//!
//! [`VaultSyncConfig`] controls *which* files are included in a collaborative
//! session. [`VaultSyncManager`] acts as the coordination layer between the
//! file-system watcher, the document registry, and the vault manifest.

use std::collections::HashSet;
use std::path::Path;

use crate::registry::{DocumentRegistry, RegistryError};
use crate::vault_manifest::VaultManifest;
use crate::DocumentId;

/// Settings that control vault-wide synchronization.
///
/// # Defaults
///
/// ```rust
/// # use collab_core::VaultSyncConfig;
/// let cfg = VaultSyncConfig::default();
/// assert!(cfg.sync_folders.is_empty()); // sync everything
/// assert!(cfg.exclude_patterns.is_empty());
/// assert!(cfg.sync_deletions);
/// assert!(cfg.extensions.contains("md"));
/// ```
#[derive(Debug, Clone)]
pub struct VaultSyncConfig {
    /// Restrict sync to these vault-relative folder paths.
    ///
    /// When empty *all* folders are synced (subject to `exclude_patterns`).
    pub sync_folders: Vec<String>,

    /// Glob-style patterns for paths that should **not** be synced.
    ///
    /// Patterns are matched against vault-relative paths.
    /// Example: `[".obsidian/*", "templates/*"]`
    ///
    /// Note: full glob evaluation is deferred to the caller; this field is
    /// available for configuration storage and inspection.
    pub exclude_patterns: Vec<String>,

    /// File extensions that are eligible for sync (without the leading dot).
    ///
    /// Defaults to `{"md"}`.
    pub extensions: HashSet<String>,

    /// Whether to propagate file deletions to remote peers.
    ///
    /// When `true` a [`VaultSyncManager::handle_deleted`] call will tombstone
    /// the file in the manifest. When `false` deletions are silently ignored and
    /// remote peers keep their copy.
    pub sync_deletions: bool,

    /// Whether to propagate file renames to remote peers.
    ///
    /// When `true` a rename event calls [`VaultSyncManager::handle_renamed`].
    pub sync_renames: bool,
}

impl Default for VaultSyncConfig {
    fn default() -> Self {
        let mut extensions = HashSet::new();
        extensions.insert("md".to_string());
        Self {
            sync_folders: Vec::new(),
            exclude_patterns: Vec::new(),
            extensions,
            sync_deletions: true,
            sync_renames: true,
        }
    }
}

impl VaultSyncConfig {
    /// Return `true` if `path` (vault-relative) should be included in sync.
    ///
    /// A path is included when:
    /// 1. Its extension matches [`extensions`](Self::extensions).
    /// 2. If `sync_folders` is non-empty, the path is a child of one of the
    ///    listed folders (`"<folder>/..."`) or exactly equals a listed entry
    ///    (single-file allowlisting).
    /// 3. None of the `exclude_patterns` match the path as a prefix.
    #[must_use]
    pub fn should_sync(&self, path: &str) -> bool {
        // 1. Extension filter.
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
        if !self.extensions.contains(ext) {
            return false;
        }

        // 2. Folder allowlist (empty = allow all). Match children via a `/`
        // separator so `"work"` does not also match `"work-notes/..."`.
        let folder_allowed = self.sync_folders.is_empty()
            || self
                .sync_folders
                .iter()
                .any(|folder| path == folder || path.starts_with(&format!("{folder}/")));
        if !folder_allowed {
            return false;
        }

        // 3. Exclusion patterns (prefix match for now). A trailing `/*` is
        // stripped for simple directory exclusions.
        let excluded = self
            .exclude_patterns
            .iter()
            .any(|pattern| path.starts_with(pattern.trim_end_matches("/*")));
        !excluded
    }

    /// Derive a `DocumentId` from a vault-relative file path.
    ///
    /// The document ID *is* the full relative path (extension included), so it
    /// is injective: distinct files never collide on a `DocumentId`. This lets
    /// the deletion path match tombstoned manifest entries directly, with no
    /// extension guessing. For example `"notes/meeting.md"` maps to
    /// `"notes/meeting.md"`.
    #[must_use]
    pub fn doc_id_for_path(path: &str) -> DocumentId {
        path.to_string()
    }
}

/// Coordinates the vault manifest, document registry, and local file events.
///
/// `VaultSyncManager` is the application-level bridge between the filesystem
/// watcher and the collaboration engine. It processes [`VaultEvent`]-equivalent
/// notifications and:
///
/// - Updates the [`VaultManifest`] (so remote peers learn about the change).
/// - Creates or closes entries in the [`DocumentRegistry`].
///
/// The manager does **not** perform I/O (no file reads/writes, no network calls).
/// Callers are responsible for reading file content and sending the manifest
/// update bytes over the wire.
pub struct VaultSyncManager {
    manifest: VaultManifest,
    registry: DocumentRegistry,
    config: VaultSyncConfig,
}

/// The outcome of handling a vault event.
///
/// Callers should inspect this to decide what to send over the network.
#[derive(Debug, Clone)]
pub struct SyncAction {
    /// The vault-relative path that was affected.
    pub path: String,
    /// The kind of action taken.
    pub kind: SyncActionKind,
    /// Encoded manifest update bytes to broadcast to remote peers.
    ///
    /// Empty when no manifest update was necessary (e.g. filtered out).
    pub manifest_update: Vec<u8>,
}

/// The type of sync action performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncActionKind {
    /// A new document was registered (file created).
    FileCreated,
    /// An existing document was closed (file deleted).
    FileDeleted,
    /// A document was renamed (old closed, new opened).
    FileRenamed {
        /// The new vault-relative path.
        new_path: String,
    },
    /// The file was ignored (outside sync scope).
    Ignored,
}

impl VaultSyncManager {
    /// Create a new sync manager with the given configuration.
    #[must_use]
    pub fn new(config: VaultSyncConfig) -> Self {
        Self { manifest: VaultManifest::new(), registry: DocumentRegistry::new(), config }
    }

    /// Return a reference to the vault manifest.
    #[must_use]
    pub const fn manifest(&self) -> &VaultManifest {
        &self.manifest
    }

    /// Return a reference to the document registry.
    #[must_use]
    pub const fn registry(&self) -> &DocumentRegistry {
        &self.registry
    }

    /// Return a reference to the sync configuration.
    #[must_use]
    pub const fn config(&self) -> &VaultSyncConfig {
        &self.config
    }

    /// Handle a *local* file-creation event.
    ///
    /// If `path` is within scope, registers a new document in the registry and
    /// marks the file alive in the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if the document cannot be created (e.g. it
    /// already exists).
    pub fn handle_created(&mut self, path: &str) -> Result<SyncAction, RegistryError> {
        if !self.config.should_sync(path) {
            return Ok(SyncAction {
                path: path.to_string(),
                kind: SyncActionKind::Ignored,
                manifest_update: Vec::new(),
            });
        }

        let doc_id = VaultSyncConfig::doc_id_for_path(path);
        // Idempotent: open only if the document does not already exist.
        if self.registry.get(&doc_id).is_none() {
            self.registry.create(&doc_id)?;
        }
        self.manifest.add_file(path);

        Ok(SyncAction {
            path: path.to_string(),
            kind: SyncActionKind::FileCreated,
            manifest_update: self.manifest.encode_full_state(),
        })
    }

    /// Handle a *local* file-deletion event.
    ///
    /// If `path` is within scope and `sync_deletions` is enabled, closes the
    /// document in the registry and tombstones the file in the manifest.
    pub fn handle_deleted(&mut self, path: &str) -> SyncAction {
        if !self.config.should_sync(path) || !self.config.sync_deletions {
            return SyncAction {
                path: path.to_string(),
                kind: SyncActionKind::Ignored,
                manifest_update: Vec::new(),
            };
        }

        let doc_id = VaultSyncConfig::doc_id_for_path(path);
        self.registry.close_any(&doc_id);
        self.manifest.delete_file(path);

        SyncAction {
            path: path.to_string(),
            kind: SyncActionKind::FileDeleted,
            manifest_update: self.manifest.encode_full_state(),
        }
    }

    /// Handle a *local* file-rename event.
    ///
    /// If both paths are within scope and `sync_renames` is enabled, closes the
    /// old document, opens a new one, and performs an atomic rename in the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if the new document cannot be created.
    pub fn handle_renamed(
        &mut self,
        old_path: &str,
        new_path: &str,
    ) -> Result<SyncAction, RegistryError> {
        if !self.config.sync_renames
            || !self.config.should_sync(old_path)
            || !self.config.should_sync(new_path)
        {
            return Ok(SyncAction {
                path: old_path.to_string(),
                kind: SyncActionKind::Ignored,
                manifest_update: Vec::new(),
            });
        }

        let old_doc_id = VaultSyncConfig::doc_id_for_path(old_path);
        let new_doc_id = VaultSyncConfig::doc_id_for_path(new_path);

        self.registry.close_any(&old_doc_id);
        if self.registry.get(&new_doc_id).is_none() {
            self.registry.create(&new_doc_id)?;
        }
        self.manifest.rename_file(old_path, new_path);

        Ok(SyncAction {
            path: old_path.to_string(),
            kind: SyncActionKind::FileRenamed { new_path: new_path.to_string() },
            manifest_update: self.manifest.encode_full_state(),
        })
    }

    /// Apply a manifest update received from a remote peer.
    ///
    /// After merging the CRDT update, computes which files are now *alive* in
    /// the merged manifest but not yet open in the local registry, and opens
    /// them. Files that are *deleted* in the manifest are closed locally.
    ///
    /// Returns the list of file paths that were newly registered (callers can
    /// use these to subscribe to the corresponding document IDs on the relay).
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if the update bytes are malformed.
    pub fn apply_remote_manifest(&mut self, update: &[u8]) -> Result<Vec<String>, RegistryError> {
        self.manifest
            .apply_update(update)
            .map_err(|e| RegistryError::InvalidState(e.to_string()))?;

        // Open documents for alive, in-scope files not yet in the registry.
        // The doc_id *is* the path, so an unknown doc_id means an unopened file.
        let to_open: Vec<String> = self
            .manifest
            .list_files()
            .into_iter()
            .filter(|path| self.config.should_sync(path))
            .filter(|path| {
                self.registry.get(path).is_none() && self.registry.get_encrypted(path).is_none()
            })
            .collect();

        let mut newly_registered = Vec::with_capacity(to_open.len());
        for path in to_open {
            self.registry.create(&path)?;
            newly_registered.push(path);
        }

        // Close documents whose files were deleted remotely.
        // The doc_id *is* the vault-relative path, so it matches the tombstone
        // key directly — no extension guessing required.
        let to_remove: Vec<String> = self
            .registry
            .list()
            .into_iter()
            .filter(|doc_id| self.manifest.is_deleted(doc_id))
            .cloned()
            .collect();

        for doc_id in to_remove {
            self.registry.close_any(&doc_id);
        }

        Ok(newly_registered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ── VaultSyncConfig::should_sync ────────────────────────────

    #[test]
    fn test_default_config_syncs_markdown() {
        let cfg = VaultSyncConfig::default();
        assert!(cfg.should_sync("notes/hello.md"));
        assert!(cfg.should_sync("README.md"));
    }

    #[test]
    fn test_default_config_ignores_non_md() {
        let cfg = VaultSyncConfig::default();
        assert!(!cfg.should_sync("image.png"));
        assert!(!cfg.should_sync("data.json"));
        assert!(!cfg.should_sync("settings.cfg"));
    }

    #[test]
    fn test_sync_folders_filter() {
        let cfg = VaultSyncConfig { sync_folders: vec!["work".to_string()], ..Default::default() };

        assert!(cfg.should_sync("work/project.md"));
        assert!(cfg.should_sync("work/meeting.md"));
        assert!(!cfg.should_sync("personal/diary.md"));
        assert!(!cfg.should_sync("README.md"));
        // A sibling folder that merely shares the prefix must NOT match:
        // "work" must not swallow "work-notes/..." or "workspace/...".
        assert!(!cfg.should_sync("work-notes/secret.md"));
        assert!(!cfg.should_sync("workspace/private.md"));
    }

    #[test]
    fn test_exclude_patterns_filter() {
        let cfg = VaultSyncConfig {
            exclude_patterns: vec![".obsidian/*".to_string(), "templates/*".to_string()],
            ..Default::default()
        };

        assert!(!cfg.should_sync(".obsidian/config.md"));
        assert!(!cfg.should_sync("templates/daily.md"));
        assert!(cfg.should_sync("notes/hello.md"));
    }

    #[test]
    fn test_custom_extension() {
        let mut cfg = VaultSyncConfig::default();
        cfg.extensions.insert("canvas".to_string());

        assert!(cfg.should_sync("board.canvas"));
        assert!(cfg.should_sync("notes.md"));
        assert!(!cfg.should_sync("image.png"));
    }

    #[test]
    fn test_doc_id_for_path_is_full_path() {
        assert_eq!(VaultSyncConfig::doc_id_for_path("notes/hello.md"), "notes/hello.md");
        assert_eq!(VaultSyncConfig::doc_id_for_path("README.md"), "README.md");
        assert_eq!(VaultSyncConfig::doc_id_for_path("a/b/c.md"), "a/b/c.md");
    }

    #[test]
    fn test_doc_id_is_injective_across_extensions() {
        // Two files sharing a stem but differing by extension must map to
        // distinct doc_ids so they never share a CRDT document.
        let md = VaultSyncConfig::doc_id_for_path("notes/a.md");
        let canvas = VaultSyncConfig::doc_id_for_path("notes/a.canvas");
        assert_ne!(md, canvas);
    }

    // ── VaultSyncManager lifecycle ───────────────────────────────

    #[test]
    fn test_handle_created_registers_document() {
        let mut mgr = VaultSyncManager::new(VaultSyncConfig::default());
        let action = mgr.handle_created("notes/hello.md").unwrap();

        assert_eq!(action.kind, SyncActionKind::FileCreated);
        assert!(!action.manifest_update.is_empty());
        assert!(mgr.manifest().contains("notes/hello.md"));
        assert!(mgr.registry().get("notes/hello.md").is_some());
    }

    #[test]
    fn test_handle_created_ignores_non_md() {
        let mut mgr = VaultSyncManager::new(VaultSyncConfig::default());
        let action = mgr.handle_created("image.png").unwrap();

        assert_eq!(action.kind, SyncActionKind::Ignored);
        assert!(action.manifest_update.is_empty());
        assert!(!mgr.manifest().contains("image.png"));
    }

    #[test]
    fn test_handle_deleted_closes_document() {
        let mut mgr = VaultSyncManager::new(VaultSyncConfig::default());
        mgr.handle_created("notes/hello.md").unwrap();
        let action = mgr.handle_deleted("notes/hello.md");

        assert_eq!(action.kind, SyncActionKind::FileDeleted);
        assert!(!action.manifest_update.is_empty());
        assert!(mgr.manifest().is_deleted("notes/hello.md"));
        assert!(mgr.registry().get("notes/hello.md").is_none());
    }

    #[test]
    fn test_handle_deleted_ignores_when_sync_deletions_disabled() {
        let cfg = VaultSyncConfig { sync_deletions: false, ..Default::default() };
        let mut mgr = VaultSyncManager::new(cfg);
        mgr.handle_created("notes/hello.md").unwrap();
        let action = mgr.handle_deleted("notes/hello.md");

        assert_eq!(action.kind, SyncActionKind::Ignored);
        // Document should still be open.
        assert!(mgr.registry().get("notes/hello.md").is_some());
    }

    #[test]
    fn test_handle_renamed() {
        let mut mgr = VaultSyncManager::new(VaultSyncConfig::default());
        mgr.handle_created("old.md").unwrap();

        let action = mgr.handle_renamed("old.md", "new.md").unwrap();

        assert_eq!(action.kind, SyncActionKind::FileRenamed { new_path: "new.md".to_string() });
        assert!(mgr.manifest().is_deleted("old.md"));
        assert!(mgr.manifest().contains("new.md"));
        assert!(mgr.registry().get("old.md").is_none());
        assert!(mgr.registry().get("new.md").is_some());
    }

    #[test]
    fn test_apply_remote_manifest_opens_new_documents() {
        // "Alice" has two files.
        let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
        alice.handle_created("shared.md").unwrap();
        alice.handle_created("alice-only.md").unwrap();

        // "Bob" receives Alice's manifest.
        let mut bob = VaultSyncManager::new(VaultSyncConfig::default());
        let manifest_bytes = alice.manifest().encode_full_state();
        let newly_registered = bob.apply_remote_manifest(&manifest_bytes).unwrap();

        // Bob should now have both documents open.
        assert_eq!(newly_registered.len(), 2);
        assert!(bob.registry().get("shared.md").is_some());
        assert!(bob.registry().get("alice-only.md").is_some());
    }

    #[test]
    fn test_apply_remote_manifest_closes_deleted_documents() {
        // Alice creates "shared.md" first, then Bob syncs from Alice (deterministic history).
        let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
        alice.handle_created("shared.md").unwrap();

        // Bob receives Alice's initial manifest — now both share the same Yrs history.
        let mut bob = VaultSyncManager::new(VaultSyncConfig::default());
        let initial_bytes = alice.manifest().encode_full_state();
        bob.apply_remote_manifest(&initial_bytes).unwrap();
        assert!(bob.registry().get("shared.md").is_some(), "Bob should have shared after init");

        // Alice now deletes "shared.md" and sends the updated manifest.
        alice.handle_deleted("shared.md");

        let manifest_bytes = alice.manifest().encode_full_state();
        let newly_registered = bob.apply_remote_manifest(&manifest_bytes).unwrap();

        // No new registrations expected.
        assert!(newly_registered.is_empty());
        // Bob should no longer have the document — Alice's later deletion wins.
        assert!(
            bob.registry().get("shared.md").is_none(),
            "Bob should close shared after remote delete"
        );
    }

    #[test]
    fn test_custom_extension_no_collision_with_md_stem() {
        // Two files sharing a stem but differing by synced extension must each
        // get their own document — the second create must NOT be skipped.
        let mut cfg = VaultSyncConfig::default();
        cfg.extensions.insert("canvas".to_string());
        let mut mgr = VaultSyncManager::new(cfg);

        let md = mgr.handle_created("notes/a.md").unwrap();
        let canvas = mgr.handle_created("notes/a.canvas").unwrap();

        assert_eq!(md.kind, SyncActionKind::FileCreated);
        assert_eq!(canvas.kind, SyncActionKind::FileCreated);
        assert!(mgr.registry().get("notes/a.md").is_some());
        assert!(mgr.registry().get("notes/a.canvas").is_some());

        // Deleting one must not close the other.
        mgr.handle_deleted("notes/a.md");
        assert!(mgr.registry().get("notes/a.md").is_none());
        assert!(mgr.registry().get("notes/a.canvas").is_some());
    }

    #[test]
    fn test_apply_remote_manifest_closes_non_md_extension() {
        // Regression: remote deletion of a non-md file must close the local
        // document. The old `.md`-guessing reconstruction never matched here.
        let mut cfg = VaultSyncConfig::default();
        cfg.extensions.insert("canvas".to_string());
        let mut alice = VaultSyncManager::new(cfg.clone());
        alice.handle_created("board.canvas").unwrap();

        let mut bob = VaultSyncManager::new(cfg);
        bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();
        assert!(bob.registry().get("board.canvas").is_some(), "Bob should open the canvas");

        alice.handle_deleted("board.canvas");
        bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();
        assert!(
            bob.registry().get("board.canvas").is_none(),
            "Bob should close the canvas on remote delete"
        );
    }
}
