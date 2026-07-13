use std::path::Path;

use async_trait::async_trait;
use thiserror::Error;

/// One bounded directory item returned to a Host Work model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDirectoryEntry {
    /// File name only; never an ambient path.
    pub name: String,
    /// Whether the item is a directory.
    pub is_directory: bool,
    /// File byte length when known.
    pub size: u64,
}

/// Sanitized Host filesystem error category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFilesystemErrorKind {
    /// Requested path is outside the enrolled capability roots.
    Denied,
    /// Input or file representation violates the bounded contract.
    Invalid,
    /// The operating-system capability is temporarily unavailable.
    Unavailable,
}

/// Sanitized Host filesystem error that never contains a local path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct HostFilesystemError {
    /// Stable failure category.
    pub kind: HostFilesystemErrorKind,
    /// Stable path-free explanation.
    pub message: String,
}

/// Capability-rooted Host filesystem reads owned by an infrastructure adapter.
#[async_trait]
pub trait HostFilesystemReader: Send + Sync {
    /// Lists one enrolled directory with deterministic bounds.
    async fn list(&self, path: &Path) -> Result<Vec<HostDirectoryEntry>, HostFilesystemError>;

    /// Reads one bounded UTF-8 file under an enrolled root.
    async fn read_text(&self, path: &Path) -> Result<String, HostFilesystemError>;
}
