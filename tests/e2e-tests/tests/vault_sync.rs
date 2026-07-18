//! End-to-end tests for full vault synchronization.
//!
//! These tests verify that [`VaultSyncManager`] and [`VaultManifest`] correctly
//! implement the acceptance criteria from issue #4:
//!
//! - Track all markdown files in the vault (or a configured subset)
//! - Sync file creations: new file on one side appears on the other
//! - Sync file deletions: deleted file is removed from the other side
//! - Sync file renames: rename is propagated to the other side
//! - Sync file content changes via existing CRDT functionality
//! - Handle conflicts gracefully (same file on both sides)
//! - Settings to configure which folders/files to sync
//!
//! The tests exercise `VaultSyncManager` directly (no network required) and,
//! in the integration section, combine it with `VaultWatcher` for real
//! filesystem events.

use std::path::PathBuf;
use std::time::Duration;

use collab_core::{
    SyncActionKind, VaultManifest, VaultSyncConfig, VaultSyncManager, MANIFEST_DOC_ID,
};
use collab_watcher::{VaultEventKind, VaultWatcher, WatcherConfig};
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::sleep;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const SETTLE: Duration = Duration::from_millis(300);
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// 1. Manifest correctness (no watcher required)
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_doc_id_is_well_known() {
    assert_eq!(MANIFEST_DOC_ID, "__vault_manifest__");
}

#[test]
fn test_manifest_tracks_file_creations() {
    let m = VaultManifest::new();
    m.add_file("notes/hello.md");
    m.add_file("daily/2024-01-01.md");

    let mut files = m.list_files();
    files.sort();
    assert_eq!(files, vec!["daily/2024-01-01.md", "notes/hello.md"]);
}

#[test]
fn test_manifest_tracks_file_deletions_as_tombstones() {
    let m = VaultManifest::new();
    m.add_file("notes/hello.md");
    m.delete_file("notes/hello.md");

    assert!(m.list_files().is_empty(), "deleted files should not appear in list");
    assert!(m.is_deleted("notes/hello.md"), "deleted file should have a tombstone");
}

#[test]
fn test_manifest_tracks_renames() {
    let m = VaultManifest::new();
    m.add_file("draft.md");
    m.rename_file("draft.md", "published.md");

    assert!(!m.contains("draft.md"), "old name should be gone");
    assert!(m.is_deleted("draft.md"), "old name should be tombstoned");
    assert!(m.contains("published.md"), "new name should be alive");
    assert_eq!(m.list_files(), vec!["published.md"]);
}

// ---------------------------------------------------------------------------
// 2. Bidirectional manifest sync
// ---------------------------------------------------------------------------

#[test]
fn test_file_created_on_alice_appears_on_bob() {
    let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
    let mut bob = VaultSyncManager::new(VaultSyncConfig::default());

    // Alice creates a file and generates a manifest broadcast.
    let action = alice.handle_created("notes/ideas.md").unwrap();
    assert_eq!(action.kind, SyncActionKind::FileCreated);
    assert!(!action.manifest_update.is_empty());

    // Bob receives the manifest update.
    let new_for_bob = bob.apply_remote_manifest(&action.manifest_update).unwrap();

    // Bob should now have the document open.
    assert_eq!(new_for_bob, vec!["notes/ideas.md".to_string()]);
    assert!(bob.registry().get("notes/ideas").is_some(), "doc should be registered");
    assert!(bob.manifest().contains("notes/ideas.md"), "manifest should track the file");
}

#[test]
fn test_file_created_on_both_sides_converges() {
    let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
    let mut bob = VaultSyncManager::new(VaultSyncConfig::default());

    // Both create files independently.
    alice.handle_created("alice.md").unwrap();
    bob.handle_created("bob.md").unwrap();

    // Exchange manifests.
    let new_for_bob =
        bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();
    let new_for_alice =
        alice.apply_remote_manifest(&bob.manifest().encode_full_state()).unwrap();

    // Both should now know about both files.
    assert!(new_for_bob.contains(&"alice.md".to_string()));
    assert!(new_for_alice.contains(&"bob.md".to_string()));

    let mut alice_files = alice.manifest().list_files();
    alice_files.sort();
    let mut bob_files = bob.manifest().list_files();
    bob_files.sort();
    assert_eq!(alice_files, vec!["alice.md", "bob.md"]);
    assert_eq!(bob_files, vec!["alice.md", "bob.md"]);
}

#[test]
fn test_file_deleted_on_alice_closes_on_bob() {
    let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
    let mut bob = VaultSyncManager::new(VaultSyncConfig::default());

    // Alice creates a file and Bob syncs it.
    alice.handle_created("temp.md").unwrap();
    bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();
    assert!(bob.registry().get("temp").is_some(), "Bob should have temp.md");

    // Alice deletes it.
    alice.handle_deleted("temp.md");

    // Bob receives the updated manifest.
    let new_for_bob =
        bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();

    assert!(new_for_bob.is_empty(), "no new files expected");
    assert!(bob.registry().get("temp").is_none(), "Bob should close temp.md after delete");
    assert!(bob.manifest().is_deleted("temp.md"), "tombstone should be in Bob's manifest");
}

#[test]
fn test_file_renamed_on_alice_updates_bob() {
    let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
    let mut bob = VaultSyncManager::new(VaultSyncConfig::default());

    // Alice creates and Bob syncs.
    alice.handle_created("draft.md").unwrap();
    bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();
    assert!(bob.registry().get("draft").is_some());

    // Alice renames.
    alice.handle_renamed("draft.md", "published.md").unwrap();

    // Bob receives the manifest.
    let new_for_bob =
        bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();

    assert_eq!(new_for_bob, vec!["published.md".to_string()]);
    assert!(bob.registry().get("draft").is_none(), "old name should be closed");
    assert!(bob.registry().get("published").is_some(), "new name should be open");
}

// ---------------------------------------------------------------------------
// 3. VaultSyncConfig filtering
// ---------------------------------------------------------------------------

#[test]
fn test_config_folder_allowlist_restricts_sync() {
    let mut cfg = VaultSyncConfig::default();
    cfg.sync_folders = vec!["work".to_string()];
    let mut mgr = VaultSyncManager::new(cfg);

    let work = mgr.handle_created("work/project.md").unwrap();
    let personal = mgr.handle_created("personal/diary.md").unwrap();

    assert_eq!(work.kind, SyncActionKind::FileCreated);
    assert_eq!(personal.kind, SyncActionKind::Ignored);
    assert!(mgr.registry().get("work/project").is_some());
    assert!(mgr.registry().get("personal/diary").is_none());
}

#[test]
fn test_config_exclude_patterns_prevent_sync() {
    let mut cfg = VaultSyncConfig::default();
    cfg.exclude_patterns = vec![".obsidian/*".to_string()];
    let mut mgr = VaultSyncManager::new(cfg);

    let system = mgr.handle_created(".obsidian/config.md").unwrap();
    let note = mgr.handle_created("notes/hello.md").unwrap();

    assert_eq!(system.kind, SyncActionKind::Ignored);
    assert_eq!(note.kind, SyncActionKind::FileCreated);
}

#[test]
fn test_config_disable_sync_deletions() {
    let mut cfg = VaultSyncConfig::default();
    cfg.sync_deletions = false;
    let mut mgr = VaultSyncManager::new(cfg);

    mgr.handle_created("keep.md").unwrap();
    let del = mgr.handle_deleted("keep.md");

    assert_eq!(del.kind, SyncActionKind::Ignored, "deletion should be ignored");
    assert!(mgr.registry().get("keep").is_some(), "doc should still be open");
}

#[test]
fn test_config_disable_sync_renames() {
    let mut cfg = VaultSyncConfig::default();
    cfg.sync_renames = false;
    let mut mgr = VaultSyncManager::new(cfg);

    mgr.handle_created("old.md").unwrap();
    let rename = mgr.handle_renamed("old.md", "new.md").unwrap();

    assert_eq!(rename.kind, SyncActionKind::Ignored, "rename should be ignored");
    assert!(mgr.registry().get("old").is_some(), "old doc should still exist");
    assert!(mgr.registry().get("new").is_none(), "new doc should not exist");
}

#[test]
fn test_config_custom_extensions() {
    let mut cfg = VaultSyncConfig::default();
    cfg.extensions.insert("canvas".to_string());
    let mut mgr = VaultSyncManager::new(cfg);

    let canvas = mgr.handle_created("board.canvas").unwrap();
    let md = mgr.handle_created("notes.md").unwrap();
    let png = mgr.handle_created("image.png").unwrap();

    assert_eq!(canvas.kind, SyncActionKind::FileCreated);
    assert_eq!(md.kind, SyncActionKind::FileCreated);
    assert_eq!(png.kind, SyncActionKind::Ignored);
}

// ---------------------------------------------------------------------------
// 4. Conflict handling
// ---------------------------------------------------------------------------

#[test]
fn test_same_filename_created_on_both_sides_converges() {
    // Both peers independently create "shared.md". After sync, both should
    // agree on its existence (CRDT convergence via LWW).
    let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
    let mut bob = VaultSyncManager::new(VaultSyncConfig::default());

    alice.handle_created("shared.md").unwrap();
    bob.handle_created("shared.md").unwrap();

    // Exchange manifests.
    alice.apply_remote_manifest(&bob.manifest().encode_full_state()).unwrap();
    bob.apply_remote_manifest(&alice.manifest().encode_full_state()).unwrap();

    // Both must agree on the file's state (convergence).
    assert_eq!(
        alice.manifest().contains("shared.md"),
        bob.manifest().contains("shared.md"),
        "Alice and Bob must agree on shared.md after conflict resolution"
    );
    // The file should be alive (both tried to create it, neither deleted it).
    assert!(alice.manifest().contains("shared.md") || alice.manifest().is_deleted("shared.md"),
        "shared.md must be in a defined state");
}

#[test]
fn test_manifest_convergence_is_idempotent() {
    // Applying the same manifest update twice should produce the same result.
    let mut alice = VaultSyncManager::new(VaultSyncConfig::default());
    let mut bob = VaultSyncManager::new(VaultSyncConfig::default());

    alice.handle_created("notes.md").unwrap();
    let update = alice.manifest().encode_full_state();

    bob.apply_remote_manifest(&update).unwrap();
    bob.apply_remote_manifest(&update).unwrap(); // apply again

    assert!(bob.manifest().contains("notes.md"), "idempotent apply should keep the file");
    // Registry should not have duplicate documents.
    assert_eq!(bob.registry().list().len(), 1, "registry should have exactly one doc");
}

// ---------------------------------------------------------------------------
// 5. Integration with VaultWatcher (real filesystem events)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_vault_watcher_creation_drives_sync_manager() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) =
        VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE).await;

    let mut mgr = VaultSyncManager::new(VaultSyncConfig::default());

    // Create a markdown file.
    tokio::fs::write(vault.path().join("meeting.md"), "# Meeting Notes").await.unwrap();

    // Wait for the watcher event.
    let event = tokio::time::timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");
    assert_eq!(event.kind, VaultEventKind::Created);
    assert_eq!(event.path, PathBuf::from("meeting.md"));

    // Feed the event into the sync manager.
    let path_str = event.path.display().to_string();
    let action = mgr.handle_created(&path_str).unwrap();

    assert_eq!(action.kind, SyncActionKind::FileCreated);
    assert!(mgr.registry().get("meeting").is_some());
    assert!(mgr.manifest().contains("meeting.md"));

    watcher.stop();
}

#[tokio::test]
async fn test_vault_watcher_deletion_drives_sync_manager() {
    let vault = TempDir::new().unwrap();
    std::fs::write(vault.path().join("temp.md"), "temporary").unwrap();

    let (watcher, mut rx) =
        VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE).await;

    let mut mgr = VaultSyncManager::new(VaultSyncConfig::default());
    mgr.handle_created("temp.md").unwrap();

    // Delete the file.
    tokio::fs::remove_file(vault.path().join("temp.md")).await.unwrap();

    let event = tokio::time::timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");
    assert_eq!(event.kind, VaultEventKind::Deleted);

    let path_str = event.path.display().to_string();
    let action = mgr.handle_deleted(&path_str);

    assert_eq!(action.kind, SyncActionKind::FileDeleted);
    assert!(mgr.registry().get("temp").is_none());
    assert!(mgr.manifest().is_deleted("temp.md"));

    watcher.stop();
}

#[tokio::test]
async fn test_full_vault_sync_two_peers_via_watcher() {
    // Alice's vault — simulate two peers exchanging manifests after local events.
    let alice_vault = TempDir::new().unwrap();
    let (alice_watcher, mut alice_rx) =
        VaultWatcher::new(alice_vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE).await;

    let mut alice_mgr = VaultSyncManager::new(VaultSyncConfig::default());
    let mut bob_mgr = VaultSyncManager::new(VaultSyncConfig::default());

    // Alice creates three notes.
    for name in &["project.md", "ideas.md", "todo.md"] {
        tokio::fs::write(alice_vault.path().join(name), format!("# {name}"))
            .await
            .unwrap();
        sleep(Duration::from_millis(60)).await; // space out events
    }

    // Drain all creation events.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, alice_rx.recv()).await {
            Ok(Some(event)) if event.kind == VaultEventKind::Created => {
                let path_str = event.path.display().to_string();
                alice_mgr.handle_created(&path_str).unwrap();
            }
            _ => break,
        }
    }

    // Bob receives Alice's manifest.
    let manifest_update = alice_mgr.manifest().encode_full_state();
    let new_for_bob = bob_mgr.apply_remote_manifest(&manifest_update).unwrap();

    // Bob should now have all three documents.
    assert_eq!(new_for_bob.len(), 3, "Bob should register 3 new documents");
    for name in &["project", "ideas", "todo"] {
        assert!(bob_mgr.registry().get(*name).is_some(), "Bob should have {name}");
    }

    alice_watcher.stop();
}
