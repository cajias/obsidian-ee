//! E2E tests for the [`VaultWatcher`] filesystem event detection system.
//!
//! These tests verify that the [`VaultWatcher`] correctly detects and reports
//! file system events (creation, modification, deletion) through its async
//! event channel. Unlike unit tests in the collab-watcher crate, these
//! tests exercise the public API from an external consumer's perspective
//! and test integration with other collaboration components.
//!
//! # Test Categories
//!
//! ## Event Detection (no Docker required)
//! - File creation, modification, and deletion detection
//! - Full create-modify-delete lifecycle
//! - Extension filtering (default `.md` and custom)
//! - Recursive subdirectory watching
//!
//! ## Integration with Document Registry
//! - VaultEvent-driven document lifecycle management
//! - File content to CRDT document mapping

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use collab_core::DocumentRegistry;
use collab_watcher::{VaultEvent, VaultEventKind, VaultWatcher, WatcherConfig};
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Maximum time to wait for a single vault event before failing.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Time to let the watcher and filesystem settle after setup or mutations.
const SETTLE_TIME: Duration = Duration::from_millis(300);

/// Receive the next event from the watcher channel, or panic with a
/// descriptive message if the channel closes or the timeout expires.
async fn recv_event(rx: &mut mpsc::Receiver<VaultEvent>) -> VaultEvent {
    timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for vault event")
        .expect("vault event channel closed unexpectedly")
}

/// Drain all available events within a time window.
async fn drain_events(rx: &mut mpsc::Receiver<VaultEvent>, window: Duration) -> Vec<VaultEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, rx.recv()).await {
            Ok(Some(event)) => events.push(event),
            _ => break,
        }
    }
    events
}

/// Assert that no events arrive within a time window.
async fn assert_no_events(rx: &mut mpsc::Receiver<VaultEvent>, window: Duration) {
    let events = drain_events(rx, window).await;
    assert!(events.is_empty(), "expected no events but received {}: {events:?}", events.len());
}

// ---------------------------------------------------------------------------
// File Creation Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_detects_markdown_file_creation() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    tokio::fs::write(vault.path().join("hello.md"), "# Hello World").await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created);
    assert_eq!(event.path, PathBuf::from("hello.md"));

    watcher.stop();
}

#[tokio::test]
async fn test_creation_reports_relative_path_not_absolute() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    tokio::fs::write(vault.path().join("test.md"), "content").await.unwrap();

    let event = recv_event(&mut rx).await;
    assert!(event.path.is_relative(), "expected relative path, got absolute: {:?}", event.path);
    assert!(
        !event.path.to_string_lossy().contains(vault.path().to_string_lossy().as_ref()),
        "event path should not contain the vault root"
    );

    watcher.stop();
}

#[tokio::test]
async fn test_detects_creation_in_nested_subdirectory() {
    let vault = TempDir::new().unwrap();
    tokio::fs::create_dir_all(vault.path().join("daily/2024/01")).await.unwrap();

    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    tokio::fs::write(vault.path().join("daily/2024/01/journal.md"), "Daily note").await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created);
    assert_eq!(event.path, PathBuf::from("daily/2024/01/journal.md"));

    watcher.stop();
}

// ---------------------------------------------------------------------------
// File Modification Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_detects_file_modification() {
    let vault = TempDir::new().unwrap();

    // Pre-create a file so the watcher's known-files set includes it.
    std::fs::write(vault.path().join("existing.md"), "original").unwrap();

    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    // Modify the existing file.
    tokio::fs::write(vault.path().join("existing.md"), "modified content").await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Modified);
    assert_eq!(event.path, PathBuf::from("existing.md"));

    watcher.stop();
}

#[tokio::test]
async fn test_distinguishes_creation_from_modification() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    // First write to a new file = Created.
    tokio::fs::write(vault.path().join("evolving.md"), "v1").await.unwrap();
    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created, "first write to a new file should be Created");

    // Let the debounce window close.
    sleep(SETTLE_TIME).await;

    // Second write to the same file = Modified.
    tokio::fs::write(vault.path().join("evolving.md"), "v2").await.unwrap();
    let event = recv_event(&mut rx).await;
    assert_eq!(
        event.kind,
        VaultEventKind::Modified,
        "second write to an existing file should be Modified"
    );

    watcher.stop();
}

// ---------------------------------------------------------------------------
// File Deletion Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_detects_file_deletion() {
    let vault = TempDir::new().unwrap();

    // Pre-create a file.
    std::fs::write(vault.path().join("doomed.md"), "goodbye").unwrap();

    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    tokio::fs::remove_file(vault.path().join("doomed.md")).await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Deleted);
    assert_eq!(event.path, PathBuf::from("doomed.md"));

    watcher.stop();
}

#[tokio::test]
async fn test_full_lifecycle_create_modify_delete() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    let file = vault.path().join("lifecycle.md");

    // Create
    tokio::fs::write(&file, "born").await.unwrap();
    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created);
    assert_eq!(event.path, PathBuf::from("lifecycle.md"));

    sleep(SETTLE_TIME).await;

    // Modify
    tokio::fs::write(&file, "lived").await.unwrap();
    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Modified);
    assert_eq!(event.path, PathBuf::from("lifecycle.md"));

    sleep(SETTLE_TIME).await;

    // Delete
    tokio::fs::remove_file(&file).await.unwrap();
    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Deleted);
    assert_eq!(event.path, PathBuf::from("lifecycle.md"));

    watcher.stop();
}

// ---------------------------------------------------------------------------
// Filtering Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_ignores_non_watched_extensions() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    // Create files with non-markdown extensions.
    tokio::fs::write(vault.path().join("image.png"), b"fake png").await.unwrap();
    tokio::fs::write(vault.path().join("data.json"), "{}").await.unwrap();
    tokio::fs::write(vault.path().join("readme.txt"), "hello").await.unwrap();

    // None of these should produce events.
    assert_no_events(&mut rx, Duration::from_secs(1)).await;

    // But a .md file should.
    tokio::fs::write(vault.path().join("real.md"), "# Real Note").await.unwrap();
    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created);
    assert_eq!(event.path, PathBuf::from("real.md"));

    watcher.stop();
}

#[tokio::test]
async fn test_custom_extension_filter() {
    let vault = TempDir::new().unwrap();
    let config = WatcherConfig {
        extensions: HashSet::from(["txt".to_string(), "org".to_string()]),
        ..WatcherConfig::default()
    };
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), config).unwrap();
    sleep(SETTLE_TIME).await;

    // .md should be ignored with custom config.
    tokio::fs::write(vault.path().join("note.md"), "# Ignored").await.unwrap();
    assert_no_events(&mut rx, Duration::from_millis(500)).await;

    // .txt should be detected.
    tokio::fs::write(vault.path().join("note.txt"), "detected").await.unwrap();
    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created);
    assert_eq!(event.path, PathBuf::from("note.txt"));

    watcher.stop();
}

// ---------------------------------------------------------------------------
// Advanced Scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multiple_file_creations_all_reported() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    let file_names: Vec<String> = (0..5).map(|i| format!("note_{i}.md")).collect();

    for name in &file_names {
        tokio::fs::write(vault.path().join(name), format!("content of {name}")).await.unwrap();
        // Space out writes to avoid debounce coalescing.
        sleep(Duration::from_millis(50)).await;
    }

    // Allow time for all debounced events to arrive.
    let events = drain_events(&mut rx, Duration::from_secs(3)).await;

    let created_paths: HashSet<PathBuf> = events
        .iter()
        .filter(|e| e.kind == VaultEventKind::Created)
        .map(|e| e.path.clone())
        .collect();

    for name in &file_names {
        assert!(
            created_paths.contains(&PathBuf::from(name)),
            "missing Created event for {name}. Got events: {events:?}"
        );
    }

    watcher.stop();
}

#[tokio::test]
async fn test_watches_dynamically_created_subdirectory() {
    let vault = TempDir::new().unwrap();

    // Start watcher BEFORE creating the subdirectory.
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    // Create a new subdirectory after the watcher is running.
    tokio::fs::create_dir(vault.path().join("new_folder")).await.unwrap();
    sleep(SETTLE_TIME).await;

    // Create a file inside the dynamically created subdirectory.
    tokio::fs::write(vault.path().join("new_folder/dynamic.md"), "# Dynamic").await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created);
    assert_eq!(event.path, PathBuf::from("new_folder/dynamic.md"));

    watcher.stop();
}

// ---------------------------------------------------------------------------
// Integration: VaultEvents → DocumentRegistry
// ---------------------------------------------------------------------------

/// RED: Vault events should drive document lifecycle in the registry.
///
/// When a file is created in the vault, we should be able to read its
/// content, load it into a CRDT document via the registry, and retrieve
/// the content back. This tests the file-content-to-document pipeline.
#[tokio::test]
async fn test_vault_events_drive_document_registry_lifecycle() {
    let vault = TempDir::new().unwrap();
    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    let mut registry = DocumentRegistry::new();

    // ── Step 1: Create a file and receive the event ──────────────
    let file_content = "# Meeting Notes\n\nAction items go here.";
    tokio::fs::write(vault.path().join("meeting.md"), file_content).await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Created);

    // Derive a document ID from the vault-relative path.
    let doc_id = event.path.to_string_lossy().replace(".md", "");

    // ── Step 2: Load file content into the document registry ─────
    // Read file content as text, create a CRDT document, then insert via
    // the Yrs transaction API. registry.open() is for restoring serialised
    // Yrs state — not raw file text.
    let content = tokio::fs::read_to_string(vault.path().join(&event.path)).await.unwrap();

    let doc = registry.create(&doc_id).unwrap();
    doc.insert(0, &content);

    // ── Step 3: Verify round-trip content fidelity ───────────────
    let doc = registry.get(&doc_id).expect("document should be in registry");
    assert_eq!(doc.get_content(), file_content, "document content should match the original file");

    watcher.stop();
}

/// RED: File modification events should update the corresponding document.
#[tokio::test]
async fn test_file_modification_updates_document_content() {
    let vault = TempDir::new().unwrap();
    let original = "# Draft\n\nFirst version.";
    std::fs::write(vault.path().join("draft.md"), original).unwrap();

    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    let mut registry = DocumentRegistry::new();

    // Pre-register the document with initial content via create + insert.
    let doc = registry.create("draft").unwrap();
    doc.insert(0, original);
    assert_eq!(doc.get_content(), original);

    // Modify the file on disk.
    let updated = "# Draft\n\nSecond version with edits.";
    tokio::fs::write(vault.path().join("draft.md"), updated).await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Modified);

    // Read updated content from disk.
    let new_content = tokio::fs::read_to_string(vault.path().join(&event.path)).await.unwrap();

    // In the GREEN phase, a VaultEventProcessor would compute a diff and
    // apply it as a CRDT operation. For now, we naively replace content.
    // This tests the expectation: after a Modified event, the registry
    // document should reflect the file's current content.
    let doc = registry.get_mut("draft").unwrap();
    let current_len: u32 = doc.get_content().len().try_into().unwrap();
    doc.delete(0, current_len);
    doc.insert(0, &new_content);

    assert_eq!(
        doc.get_content(),
        updated,
        "document content should match updated file after processing Modified event"
    );

    watcher.stop();
}

/// RED: File deletion events should close documents in the registry.
#[tokio::test]
async fn test_file_deletion_closes_document_in_registry() {
    let vault = TempDir::new().unwrap();
    std::fs::write(vault.path().join("temp.md"), "temporary").unwrap();

    let (watcher, mut rx) = VaultWatcher::new(vault.path(), WatcherConfig::default()).unwrap();
    sleep(SETTLE_TIME).await;

    let mut registry = DocumentRegistry::new();
    registry.create("temp").unwrap();
    assert!(registry.get("temp").is_some(), "document should exist before deletion");

    // Delete the file.
    tokio::fs::remove_file(vault.path().join("temp.md")).await.unwrap();

    let event = recv_event(&mut rx).await;
    assert_eq!(event.kind, VaultEventKind::Deleted);

    // Process the deletion: close the document in the registry.
    let doc_id = event.path.to_string_lossy().replace(".md", "");
    let closed = registry.close(&doc_id);
    assert!(
        closed.is_some(),
        "closing document '{doc_id}' after Deleted event should return the document"
    );
    assert!(
        registry.get(&doc_id).is_none(),
        "document should no longer exist in registry after deletion"
    );

    watcher.stop();
}
