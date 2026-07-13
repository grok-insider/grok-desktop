use std::{path::Path, time::Duration};

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

    /// Reads bounded raw bytes for UTF-8 or base64 tool projection.
    async fn read_bytes(&self, path: &Path, max_bytes: u64)
    -> Result<Vec<u8>, HostFilesystemError>;
}

/// Capability-rooted Host filesystem mutations owned by an infrastructure adapter.
#[async_trait]
pub trait HostFilesystemWriter: Send + Sync {
    /// Atomically replaces one exact file with bounded UTF-8 content.
    async fn write_text(&self, path: &Path, content: String) -> Result<(), HostFilesystemError>;
}

/// Fully validated process request passed only after exact user approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProcessRequest {
    /// Executable and exact argument vector; no implicit shell is inserted.
    pub argv: Vec<String>,
    /// Enrolled starting directory.
    pub cwd: String,
    /// Bounded wall-clock limit.
    pub timeout: Duration,
}

/// Bounded process completion projection returned to the Work model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProcessOutput {
    /// Platform exit code when the process exited normally.
    pub exit_code: Option<i32>,
    /// Retained standard output, decoded lossily and bounded with stderr.
    pub stdout: String,
    /// Retained standard error, decoded lossily and bounded with stdout.
    pub stderr: String,
    /// Whether output beyond the shared retention cap was discarded.
    pub truncated: bool,
}

/// Sanitized process execution category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostProcessErrorKind {
    /// The request violated the closed execution contract.
    Invalid,
    /// The starting directory was outside enrolled roots.
    Denied,
    /// The process could not be started or observed.
    Unavailable,
    /// The process was killed after its wall-clock limit elapsed.
    TimedOut,
    /// Cancellation interrupted a process that may already have changed the host.
    Interrupted,
}

/// Path- and secret-free Host process failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct HostProcessError {
    /// Stable failure category.
    pub kind: HostProcessErrorKind,
    /// Stable explanation safe for model and renderer surfaces.
    pub message: String,
}

/// Exact, bounded process execution behind daemon policy and approval gates.
#[async_trait]
pub trait HostProcessExecutor: Send + Sync {
    /// Resolves executable and working-directory identities before approval.
    async fn validate(
        &self,
        request: HostProcessRequest,
    ) -> Result<HostProcessRequest, HostProcessError>;

    /// Executes one request, killing the complete process tree on timeout or cancellation.
    async fn execute(
        &self,
        request: HostProcessRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<HostProcessOutput, HostProcessError>;
}
