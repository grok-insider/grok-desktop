//! Private immutable artifact-object storage and local-open adapters.
//!
//! The adapter never persists or returns a host path. Linux publication and
//! opening are descriptor-held; unsupported platforms remain fail-closed.

use async_trait::async_trait;
use grok_application::{
    ArtifactContentPublication, ArtifactContentPurge, ArtifactContentRetention,
    ArtifactContentStatus, ArtifactContentStore, ArtifactImportFailureCode, ArtifactOpenError,
    ArtifactOpenFailureCode, ArtifactOpener, ArtifactRetentionFailureCode, PreparedArtifactContent,
    SelectedSourcePath,
};
use grok_domain::{ArtifactId, ArtifactVersion, UnixMillis};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(all(test, target_os = "linux"))]
mod linux_ga_qualification;

#[cfg(target_os = "linux")]
pub use linux::LinuxArtifactContent;

/// Fail-closed content/open adapter used when the current runtime has no
/// qualified platform implementation.
#[derive(Debug, Default)]
pub struct UnavailableArtifactContent;

#[async_trait]
impl ArtifactContentStore for UnavailableArtifactContent {
    async fn prepare_import_content(
        &self,
        _source: &SelectedSourcePath,
        _artifact_id: &ArtifactId,
        _content_version: u32,
        _media_type: &str,
        _max_bytes: u64,
        _deadline_unix_ms: UnixMillis,
    ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode> {
        Err(ArtifactImportFailureCode::ContentStoreUnavailable)
    }

    async fn publish_content(
        &self,
        _content: &ArtifactVersion,
        _deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPublication, ArtifactImportFailureCode> {
        Err(ArtifactImportFailureCode::ContentStoreUnavailable)
    }

    async fn content_status(
        &self,
        _content: &ArtifactVersion,
        _deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentStatus, ArtifactImportFailureCode> {
        Err(ArtifactImportFailureCode::ContentStoreUnavailable)
    }

    async fn discard_prepared_content(
        &self,
        _content: &ArtifactVersion,
    ) -> Result<(), ArtifactImportFailureCode> {
        Err(ArtifactImportFailureCode::ContentStoreUnavailable)
    }

    async fn discard_reserved_content(
        &self,
        _artifact_id: &ArtifactId,
        _content_version: u32,
    ) -> Result<(), ArtifactImportFailureCode> {
        Err(ArtifactImportFailureCode::ContentStoreUnavailable)
    }
}

#[async_trait]
impl ArtifactOpener for UnavailableArtifactContent {
    async fn open_artifact(
        &self,
        _content: &ArtifactVersion,
        _deadline_unix_ms: UnixMillis,
    ) -> Result<(), ArtifactOpenError> {
        Err(ArtifactOpenError::Known(
            ArtifactOpenFailureCode::PlatformUnavailable,
        ))
    }
}

#[async_trait]
impl ArtifactContentRetention for UnavailableArtifactContent {
    async fn purge_content(
        &self,
        _content: &ArtifactVersion,
        _deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPurge, ArtifactRetentionFailureCode> {
        Err(ArtifactRetentionFailureCode::ContentStoreUnavailable)
    }
}
