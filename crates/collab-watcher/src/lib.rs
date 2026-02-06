//! File system watcher for Obsidian vault events.
//!
//! Monitors an Obsidian vault directory for file changes and emits
//! structured events via a tokio channel. Filters for relevant files
//! (`.md` by default) and debounces rapid filesystem events.

mod watcher;

pub use watcher::{VaultEvent, VaultEventKind, VaultWatcher, WatcherConfig};

/// Errors that can occur during vault watching.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The watched path does not exist or is not a directory.
    #[error("invalid vault path: {0}")]
    InvalidPath(String),

    /// The underlying file watcher failed.
    #[error("watcher error: {0}")]
    Watcher(#[from] notify::Error),

    /// The event channel was closed.
    #[error("event channel closed")]
    ChannelClosed,
}

/// Crate-level result type.
pub type Result<T> = std::result::Result<T, Error>;
