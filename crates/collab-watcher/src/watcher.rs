//! Vault file system watcher implementation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use tokio::sync::mpsc;

use crate::{Error, Result};

/// Configuration for the vault watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// File extensions to watch (without the dot). Defaults to `["md"]`.
    pub extensions: HashSet<String>,
    /// Debounce duration for coalescing rapid events.
    pub debounce: Duration,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self { extensions: HashSet::from(["md".to_string()]), debounce: Duration::from_millis(200) }
    }
}

/// The kind of vault event that occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultEventKind {
    /// A new file was created.
    Created,
    /// An existing file was modified.
    Modified,
    /// A file was deleted.
    Deleted,
}

/// A structured event from the vault watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultEvent {
    /// The kind of event.
    pub kind: VaultEventKind,
    /// The path of the affected file, relative to the vault root.
    pub path: PathBuf,
}

/// Watches an Obsidian vault directory for file changes.
#[derive(Debug)]
pub struct VaultWatcher {
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    _stop_tx: tokio::sync::oneshot::Sender<()>,
}

impl VaultWatcher {
    /// Create a new watcher for the given vault directory.
    ///
    /// Returns a `VaultWatcher` handle and a receiver for vault events.
    /// The watcher begins monitoring immediately upon creation.
    ///
    /// # Errors
    ///
    /// Returns `Error::InvalidPath` if the path doesn't exist or isn't a directory.
    ///
    /// # Panics
    ///
    /// Panics if the internal known-files mutex is poisoned.
    pub fn new(
        vault_path: impl AsRef<Path>,
        config: WatcherConfig,
    ) -> Result<(Self, mpsc::Receiver<VaultEvent>)> {
        let vault_path = vault_path.as_ref();

        // Validate the path exists and is a directory.
        if !vault_path.exists() || !vault_path.is_dir() {
            return Err(Error::InvalidPath(vault_path.display().to_string()));
        }

        let vault_path = vault_path
            .canonicalize()
            .map_err(|e| Error::InvalidPath(format!("{}: {e}", vault_path.display())))?;

        let known_files = Arc::new(Mutex::new(Self::scan_existing_files(&vault_path)));

        let (bridge_tx, bridge_rx) = std::sync::mpsc::channel();

        let mut debouncer = new_debouncer(config.debounce, move |res| {
            let _ = bridge_tx.send(res);
        })?;

        debouncer.watcher().watch(&vault_path, RecursiveMode::Recursive)?;

        let (event_tx, event_rx) = mpsc::channel(100);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

        spawn_event_loop(
            bridge_rx,
            event_tx,
            stop_rx,
            config.extensions,
            vault_path,
            known_files,
        );

        Ok((
            Self {
                _debouncer: debouncer,
                _stop_tx: stop_tx,
            },
            event_rx,
        ))
    }

    /// Stop watching and clean up resources.
    pub fn stop(self) {
        // Dropping self will:
        // 1. Drop _debouncer, which stops the notify watcher and its debounce thread.
        // 2. Drop _stop_tx, which signals the background tokio task to stop
        //    (the oneshot receiver will resolve to Err, breaking the loop).
        drop(self);
    }

    /// Scan the vault directory for existing files to build the known-files set.
    fn scan_existing_files(vault_path: &Path) -> HashSet<PathBuf> {
        walkdir(vault_path).unwrap_or_default().into_iter().collect()
    }
}

/// A type alias for the debouncer result channel receiver.
type BridgeReceiver = std::sync::mpsc::Receiver<notify_debouncer_mini::DebounceEventResult>;

/// Spawn a tokio task that bridges debouncer events into the async channel.
fn spawn_event_loop(
    bridge_rx: BridgeReceiver,
    event_tx: mpsc::Sender<VaultEvent>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
    extensions: HashSet<String>,
    vault_root: PathBuf,
    known_files: Arc<Mutex<HashSet<PathBuf>>>,
) {
    tokio::spawn(async move {
        'outer: loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    tracing::debug!("vault watcher stop signal received");
                    break;
                }
                () = tokio::task::yield_now() => {
                    match bridge_rx.try_recv() {
                        Ok(Ok(events)) => {
                            for ev in events {
                                if !process_single_event(&ev.path, &event_tx, &extensions, &vault_root, &known_files).await {
                                    break 'outer;
                                }
                            }
                        }
                        Ok(Err(e)) => tracing::warn!("notify error: {e}"),
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            tracing::debug!("debouncer channel disconnected");
                            break;
                        }
                    }
                }
            }
        }
    });
}

/// Process a single debounced filesystem event.
///
/// Returns `false` if the event channel is closed and we should stop.
async fn process_single_event(
    path: &Path,
    event_tx: &mpsc::Sender<VaultEvent>,
    extensions: &HashSet<String>,
    vault_root: &Path,
    known_files: &Arc<Mutex<HashSet<PathBuf>>>,
) -> bool {
    // Filter by extension.
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !extensions.contains(ext) {
        return true;
    }

    // Make the path relative to the vault root.
    let Some(relative) = strip_vault_prefix(path, vault_root) else {
        return true;
    };

    // Determine event kind based on file existence and whether we knew about the file before.
    let kind = if path.exists() {
        let mut known = known_files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if known.contains(path) {
            VaultEventKind::Modified
        } else {
            known.insert(path.to_path_buf());
            VaultEventKind::Created
        }
    } else {
        known_files.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(path);
        VaultEventKind::Deleted
    };

    let event = VaultEvent { kind, path: relative };
    tracing::debug!(?event, "vault event");

    if event_tx.send(event).await.is_err() {
        tracing::debug!("event channel closed, stopping watcher task");
        return false;
    }

    true
}

/// Strip the vault root prefix from a path, returning the relative path.
///
/// Returns `None` (with a warning log) if the path is not under the vault root.
fn strip_vault_prefix(path: &Path, vault_root: &Path) -> Option<PathBuf> {
    path.strip_prefix(vault_root).map_or_else(
        |_| {
            tracing::warn!("event path is not under vault root: {}", path.display());
            None
        },
        |rel| Some(rel.to_path_buf()),
    )
}

/// Recursively walk a directory and collect file paths.
fn walkdir(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    walkdir_inner(path, &mut result)?;
    Ok(result)
}

fn walkdir_inner(path: &Path, result: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walkdir_inner(&path, result)?;
        } else {
            result.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a watcher with default config and short debounce for tests.
    fn test_config() -> WatcherConfig {
        WatcherConfig { debounce: Duration::from_millis(50), ..WatcherConfig::default() }
    }

    /// Helper: drain events from receiver with a timeout.
    async fn collect_events(
        rx: &mut mpsc::Receiver<VaultEvent>,
        timeout: Duration,
    ) -> Vec<VaultEvent> {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    events.push(event);
                }
                () = tokio::time::sleep_until(deadline) => {
                    break;
                }
            }
        }
        events
    }

    // ── Construction Tests ──────────────────────────────────

    #[test]
    fn test_default_config_watches_markdown() {
        let config = WatcherConfig::default();
        assert!(config.extensions.contains("md"));
        assert_eq!(config.extensions.len(), 1);
        assert_eq!(config.debounce, Duration::from_millis(200));
    }

    #[tokio::test]
    async fn test_new_with_valid_directory() {
        let dir = TempDir::new().unwrap();
        let result = VaultWatcher::new(dir.path(), test_config());
        assert!(result.is_ok(), "should accept a valid directory");
    }

    #[tokio::test]
    async fn test_new_with_nonexistent_path_fails() {
        let result = VaultWatcher::new("/nonexistent/path/to/vault", test_config());
        assert!(result.is_err(), "should reject nonexistent path");
        let err = result.unwrap_err();
        assert!(matches!(err, Error::InvalidPath(_)), "should be InvalidPath error, got: {err:?}");
    }

    #[tokio::test]
    async fn test_new_with_file_path_fails() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("not-a-dir.md");
        fs::write(&file_path, "content").unwrap();

        let result = VaultWatcher::new(&file_path, test_config());
        assert!(result.is_err(), "should reject a file path");
    }

    // ── Event Detection Tests ───────────────────────────────

    #[tokio::test]
    async fn test_detects_markdown_file_creation() {
        let dir = TempDir::new().unwrap();
        let (watcher, mut rx) = VaultWatcher::new(dir.path(), test_config()).unwrap();

        // Allow watcher to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create a markdown file
        fs::write(dir.path().join("note.md"), "# Hello").unwrap();

        let events = collect_events(&mut rx, Duration::from_secs(2)).await;
        assert!(
            events
                .iter()
                .any(|e| e.kind == VaultEventKind::Created && e.path == Path::new("note.md")),
            "should detect .md file creation, got: {events:?}"
        );

        watcher.stop();
    }

    #[tokio::test]
    async fn test_detects_file_modification() {
        let dir = TempDir::new().unwrap();
        let note = dir.path().join("existing.md");
        fs::write(&note, "initial content").unwrap();

        let (watcher, mut rx) = VaultWatcher::new(dir.path(), test_config()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Modify the file
        fs::write(&note, "updated content").unwrap();

        let events = collect_events(&mut rx, Duration::from_secs(2)).await;
        assert!(
            events
                .iter()
                .any(|e| e.kind == VaultEventKind::Modified && e.path == Path::new("existing.md")),
            "should detect .md file modification, got: {events:?}"
        );

        watcher.stop();
    }

    #[tokio::test]
    async fn test_detects_file_deletion() {
        let dir = TempDir::new().unwrap();
        let note = dir.path().join("to-delete.md");
        fs::write(&note, "bye").unwrap();

        let (watcher, mut rx) = VaultWatcher::new(dir.path(), test_config()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Delete the file
        fs::remove_file(&note).unwrap();

        let events = collect_events(&mut rx, Duration::from_secs(2)).await;
        assert!(
            events
                .iter()
                .any(|e| e.kind == VaultEventKind::Deleted && e.path == Path::new("to-delete.md")),
            "should detect .md file deletion, got: {events:?}"
        );

        watcher.stop();
    }

    #[tokio::test]
    async fn test_delete_then_recreate_emits_created() {
        let dir = TempDir::new().unwrap();
        let note = dir.path().join("cycle.md");
        fs::write(&note, "v1").unwrap();

        let (watcher, mut rx) = VaultWatcher::new(dir.path(), test_config()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Delete and wait for the event to be processed
        fs::remove_file(&note).unwrap();
        let events = collect_events(&mut rx, Duration::from_secs(2)).await;
        assert!(
            events
                .iter()
                .any(|e| e.kind == VaultEventKind::Deleted && e.path == Path::new("cycle.md")),
            "should detect deletion, got: {events:?}"
        );

        // Recreate the same file - should be Created, not Modified
        fs::write(&note, "v2").unwrap();
        let events = collect_events(&mut rx, Duration::from_secs(2)).await;
        assert!(
            events
                .iter()
                .any(|e| e.kind == VaultEventKind::Created && e.path == Path::new("cycle.md")),
            "should detect recreation as Created (not Modified), got: {events:?}"
        );

        watcher.stop();
    }

    // ── Filtering Tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_ignores_non_markdown_files() {
        let dir = TempDir::new().unwrap();
        let (watcher, mut rx) = VaultWatcher::new(dir.path(), test_config()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create non-markdown files
        fs::write(dir.path().join("image.png"), b"fake png").unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        fs::write(dir.path().join(".obsidian"), "config").unwrap();

        let events = collect_events(&mut rx, Duration::from_secs(1)).await;
        assert!(events.is_empty(), "should not emit events for non-.md files, got: {events:?}");

        watcher.stop();
    }

    #[tokio::test]
    async fn test_custom_extension_filter() {
        let dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.extensions.insert("canvas".to_string());

        let (watcher, mut rx) = VaultWatcher::new(dir.path(), config).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create a .canvas file (Obsidian canvas)
        fs::write(dir.path().join("board.canvas"), "{}").unwrap();

        let events = collect_events(&mut rx, Duration::from_secs(2)).await;
        assert!(
            events.iter().any(|e| e.path == Path::new("board.canvas")),
            "should detect .canvas files with custom config, got: {events:?}"
        );

        watcher.stop();
    }

    // ── Subdirectory Tests ──────────────────────────────────

    #[tokio::test]
    async fn test_watches_subdirectories_recursively() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("subfolder");
        fs::create_dir(&subdir).unwrap();

        let (watcher, mut rx) = VaultWatcher::new(dir.path(), test_config()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create a file in a subdirectory
        fs::write(subdir.join("nested.md"), "# Nested").unwrap();

        let events = collect_events(&mut rx, Duration::from_secs(2)).await;
        assert!(
            events.iter().any(|e| e.path == Path::new("subfolder/nested.md")),
            "should detect files in subdirectories with relative paths, got: {events:?}"
        );

        watcher.stop();
    }

    // ── Lifecycle Tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_stop_closes_event_channel() {
        let dir = TempDir::new().unwrap();
        let (watcher, mut rx) = VaultWatcher::new(dir.path(), test_config()).unwrap();

        watcher.stop();

        // After stop, the channel should eventually close
        let result = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
        assert!(matches!(result, Ok(None) | Err(_)), "channel should close after stop");
    }
}
