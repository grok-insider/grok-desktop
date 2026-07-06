use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use grok_domain::{
    Artifact, ArtifactContentSummary, ArtifactId, ArtifactState, ArtifactVersion,
    MAX_ARTIFACT_CONTENT_VERSION, ProjectId, ProjectState, ThreadId, ThreadState, UnixMillis,
    validate_imported_file_name,
};
use thiserror::Error;

use crate::{
    ApplicationError, Clock, IdGenerator, MutationCommand, Page, StoreError, WorkspaceStore,
    mutations::mutation_command_bytes,
};

/// Maximum bytes accepted from one selected source file.
pub const MAX_ARTIFACT_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum committed artifact bytes owned by one project.
pub const MAX_PROJECT_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum committed artifact bytes across the local profile.
pub const MAX_GLOBAL_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Maximum live artifacts owned by one project.
pub const MAX_PROJECT_ARTIFACT_COUNT: u64 = 10_000;
/// Inner deadline for bounded source inspection, staging, and publication.
pub const ARTIFACT_IMPORT_IO_TIMEOUT_MS: u64 = 30_000;
/// Inner deadline for one platform open dispatch.
pub const ARTIFACT_OPEN_TIMEOUT_MS: u64 = 10_000;
/// Inner deadline for exact local-content purge during removal.
pub const ARTIFACT_REMOVAL_IO_TIMEOUT_MS: u64 = 30_000;
/// Maximum interrupted artifact operations handled by one recovery pass.
pub const MAX_ARTIFACT_RECOVERY_BATCH: usize = 100;
const ARTIFACT_IMPORT_SCOPE: &str = "import_artifact";
const ARTIFACT_OPEN_SCOPE: &str = "open_artifact";
const ARTIFACT_REMOVAL_SCOPE: &str = "remove_artifact";

/// Ephemeral native file selection which is never persisted or projected.
///
/// Its debug representation and every validation error are deliberately
/// redacted. The value must be dropped after one newly reserved import attempt.
pub struct SelectedSourcePath(PathBuf);

impl SelectedSourcePath {
    /// Accepts one absolute path selected by a trusted native picker.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the path is not absolute or exceeds the
    /// platform-independent input bound.
    pub fn new(path: PathBuf) -> Result<Self, ApplicationError> {
        let Some(text) = path.to_str() else {
            return Err(ApplicationError::InvalidInput(
                "selected source path is invalid".into(),
            ));
        };
        if !path.is_absolute()
            || text.is_empty()
            || text.len() > 32 * 1024
            || text.chars().any(char::is_control)
        {
            return Err(ApplicationError::InvalidInput(
                "selected source path is invalid".into(),
            ));
        }
        Ok(Self(path))
    }

    /// Borrows the path only for immediate use by a qualified content adapter.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Debug for SelectedSourcePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SelectedSourcePath([REDACTED])")
    }
}

/// Typed request for a native-selected local file import.
pub struct ImportArtifact {
    /// Existing active project.
    pub project_id: String,
    /// Optional open conversation association.
    pub thread_id: Option<String>,
    /// Portable user-visible basename.
    pub display_name: String,
    /// Bounded non-secret media type supplied by the trusted picker boundary.
    pub media_type: String,
    /// Ephemeral source path; never persisted or returned.
    pub source: SelectedSourcePath,
}

impl fmt::Debug for ImportArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportArtifact")
            .field("project_id", &self.project_id)
            .field("thread_id", &self.thread_id)
            .field("display_name", &self.display_name)
            .field("media_type", &self.media_type)
            .field("source", &"[REDACTED]")
            .finish()
    }
}

/// Typed request to open an exact immutable artifact version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenArtifact {
    /// Canonical artifact identifier.
    pub artifact_id: String,
    /// Exact content version selected by the caller.
    pub content_version: u32,
}

/// Typed request to tombstone and purge one exact current artifact version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveArtifact {
    /// Canonical artifact identifier.
    pub artifact_id: String,
    /// Exact current metadata revision observed by the caller.
    pub expected_revision: u64,
    /// Exact current content version observed by the caller.
    pub expected_content_version: u32,
}

/// Durable retention lifecycle for one immutable artifact version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactRetentionState {
    /// Bytes remain locally retained and no purge intent exists.
    Retained,
    /// Durable removal intent owns the bytes and may purge them idempotently.
    PurgePending,
    /// Exact local bytes are confirmed absent with directory durability.
    Purged,
}

/// Revisioned retention record for one immutable artifact version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRetentionRecord {
    /// Immutable content whose local retention is tracked.
    pub content: ArtifactVersion,
    /// Current retention lifecycle.
    pub state: ArtifactRetentionState,
    /// Optimistic retention revision.
    pub revision: u64,
    /// Time this version first became locally retained.
    pub created_at: UnixMillis,
    /// Last durable retention transition.
    pub updated_at: UnixMillis,
    /// Confirmed purge time, present exactly in `Purged`.
    pub purged_at: Option<UnixMillis>,
}

#[allow(clippy::missing_errors_doc)]
impl ArtifactRetentionRecord {
    /// Creates the initial retained record for immutable committed content.
    pub fn retained(content: ArtifactVersion) -> Result<Self, ApplicationError> {
        ArtifactVersion::restore(content.clone())?;
        let created_at = content.created_at;
        Ok(Self {
            content,
            state: ArtifactRetentionState::Retained,
            revision: 0,
            created_at,
            updated_at: created_at,
            purged_at: None,
        })
    }

    /// Validates a retention record restored from durable storage.
    pub fn restore(snapshot: Self) -> Result<Self, ApplicationError> {
        ArtifactVersion::restore(snapshot.content.clone())?;
        if snapshot.created_at != snapshot.content.created_at
            || snapshot.updated_at < snapshot.created_at
        {
            return Err(invalid_retention_record());
        }
        let valid = match snapshot.state {
            ArtifactRetentionState::Retained => {
                snapshot.revision == 0
                    && snapshot.updated_at == snapshot.created_at
                    && snapshot.purged_at.is_none()
            }
            ArtifactRetentionState::PurgePending => {
                snapshot.revision == 1 && snapshot.purged_at.is_none()
            }
            ArtifactRetentionState::Purged => {
                snapshot.revision == 2 && snapshot.purged_at == Some(snapshot.updated_at)
            }
        };
        if !valid {
            return Err(invalid_retention_record());
        }
        Ok(snapshot)
    }

    /// Binds this exact version to a durable removal intent.
    pub fn begin_purge(&mut self, now: UnixMillis) -> Result<(), ApplicationError> {
        if self.state != ArtifactRetentionState::Retained || now < self.updated_at {
            return Err(invalid_retention_record());
        }
        self.state = ArtifactRetentionState::PurgePending;
        self.revision = 1;
        self.updated_at = now;
        Ok(())
    }

    /// Records confirmed durable absence of this exact local object.
    pub fn record_purged(&mut self, now: UnixMillis) -> Result<(), ApplicationError> {
        if self.state != ArtifactRetentionState::PurgePending || now < self.updated_at {
            return Err(invalid_retention_record());
        }
        self.state = ArtifactRetentionState::Purged;
        self.revision = 2;
        self.updated_at = now;
        self.purged_at = Some(now);
        Ok(())
    }
}

/// Durable artifact-removal lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactRemovalState {
    /// Metadata is tombstoned while retained versions await confirmed purge.
    Pending,
    /// Every retention row is already `Purged`; the command is terminal.
    Committed,
}

/// Exact durable removal plan without a host or storage path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRemovalPlan {
    /// Canonical deleted metadata tombstone.
    pub artifact: Artifact,
    /// Current removal lifecycle.
    pub state: ArtifactRemovalState,
    /// Optimistic journal revision.
    pub revision: u64,
    /// Journal creation time.
    pub created_at: UnixMillis,
    /// Last durable transition time.
    pub updated_at: UnixMillis,
}

#[allow(clippy::missing_errors_doc)]
impl ArtifactRemovalPlan {
    /// Creates a pending plan from an atomically persisted tombstone.
    pub fn pending(artifact: Artifact) -> Result<Self, ApplicationError> {
        Artifact::restore(artifact.clone())?;
        if artifact.state != ArtifactState::Deleted || artifact.content.is_some() {
            return Err(invalid_removal_plan());
        }
        let created_at = artifact.updated_at;
        Ok(Self {
            artifact,
            state: ArtifactRemovalState::Pending,
            revision: 0,
            created_at,
            updated_at: created_at,
        })
    }

    /// Validates a removal plan restored from durable storage.
    pub fn restore(snapshot: Self) -> Result<Self, ApplicationError> {
        Artifact::restore(snapshot.artifact.clone())?;
        if snapshot.artifact.state != ArtifactState::Deleted
            || snapshot.artifact.content.is_some()
            || snapshot.artifact.updated_at != snapshot.created_at
            || snapshot.updated_at < snapshot.created_at
        {
            return Err(invalid_removal_plan());
        }
        let valid = match snapshot.state {
            ArtifactRemovalState::Pending => {
                snapshot.revision == 0 && snapshot.updated_at == snapshot.created_at
            }
            ArtifactRemovalState::Committed => snapshot.revision == 1,
        };
        if !valid {
            return Err(invalid_removal_plan());
        }
        Ok(snapshot)
    }

    /// Records that every version owned by this tombstone was durably purged.
    pub fn commit(&mut self, now: UnixMillis) -> Result<(), ApplicationError> {
        if self.state != ArtifactRemovalState::Pending || now < self.updated_at {
            return Err(invalid_removal_plan());
        }
        self.state = ArtifactRemovalState::Committed;
        self.revision = 1;
        self.updated_at = now;
        Ok(())
    }
}

/// Atomic resolution result for one exact removal command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactRemovalReservation {
    /// This call atomically tombstoned metadata and owns purge execution.
    NewlyPending(ArtifactRemovalPlan),
    /// Exact durable command replay; pending purge is safe to resume idempotently.
    ExactReplay(ArtifactRemovalPlan),
}

/// Read-only resolution of one exact path-free removal command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactRemovalResolution {
    /// No durable command exists for this exact tuple and idempotency key.
    Unknown,
    /// Metadata is already tombstoned while private-content purge remains pending.
    Pending {
        /// Canonical path-free metadata tombstone.
        artifact: Artifact,
    },
    /// Private-content purge and the durable removal command are committed.
    Committed {
        /// Canonical path-free metadata tombstone.
        artifact: Artifact,
    },
}

/// Durable import journal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactImportState {
    /// Intent is durable; no source I/O may have completed.
    Prepared,
    /// Immutable bytes and their digest are ready for idempotent publication.
    ContentReady,
    /// Bytes, version row, and available artifact metadata are committed.
    Committed,
    /// A stable terminal failure was recorded.
    Failed,
}

/// Stable terminal import failure safe to persist and project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactImportFailureCode {
    /// The selected source could not be opened safely.
    #[error("source_unavailable")]
    SourceUnavailable,
    /// Source identity changed while it was being inspected or copied.
    #[error("source_changed")]
    SourceChanged,
    /// The selected file exceeded 64 MiB.
    #[error("file_too_large")]
    FileTooLarge,
    /// Project committed bytes would exceed 1 GiB.
    #[error("project_byte_quota_exceeded")]
    ProjectByteQuotaExceeded,
    /// Global committed bytes would exceed 4 GiB.
    #[error("global_byte_quota_exceeded")]
    GlobalByteQuotaExceeded,
    /// Project live artifact count would exceed 10,000.
    #[error("project_count_quota_exceeded")]
    ProjectCountQuotaExceeded,
    /// A bounded inner content operation exceeded its deadline.
    #[error("deadline_exceeded")]
    DeadlineExceeded,
    /// Content identity or private-store invariants failed validation.
    #[error("integrity_failure")]
    IntegrityFailure,
    /// The private content store is unavailable.
    #[error("content_store_unavailable")]
    ContentStoreUnavailable,
    /// Startup observed a prepared import whose ephemeral source was gone.
    #[error("interrupted_before_content_ready")]
    InterruptedBeforeContentReady,
}

/// Exact durable import plan. It never contains a selected or storage path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactImportPlan {
    /// Canonical artifact snapshot owned by this command.
    pub artifact: Artifact,
    /// Current journal state.
    pub state: ArtifactImportState,
    /// Immutable content metadata once staging has completed.
    pub content: Option<ArtifactVersion>,
    /// Stable terminal code, present exactly in `Failed`.
    pub failure: Option<ArtifactImportFailureCode>,
    /// Optimistic journal revision.
    pub revision: u64,
    /// Journal creation time.
    pub created_at: UnixMillis,
    /// Last durable transition time.
    pub updated_at: UnixMillis,
}

#[allow(clippy::missing_errors_doc)]
impl ArtifactImportPlan {
    /// Creates the canonical prepared plan for a newly reserved artifact.
    ///
    /// # Errors
    ///
    /// Returns an integrity error unless the artifact is a pristine unavailable
    /// reservation.
    pub fn prepared(artifact: Artifact) -> Result<Self, ApplicationError> {
        Artifact::restore(artifact.clone()).map_err(ApplicationError::from)?;
        if artifact.state != ArtifactState::Unavailable {
            return Err(invalid_import_plan());
        }
        let now = artifact.created_at;
        Ok(Self {
            artifact,
            state: ArtifactImportState::Prepared,
            content: None,
            failure: None,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Validates a plan restored from durable storage.
    pub fn restore(snapshot: Self) -> Result<Self, ApplicationError> {
        Artifact::restore(snapshot.artifact.clone())?;
        if snapshot.updated_at < snapshot.created_at
            || snapshot.artifact.created_at != snapshot.created_at
        {
            return Err(invalid_import_plan());
        }
        if let Some(content) = &snapshot.content {
            ArtifactVersion::restore(content.clone())?;
            if content.artifact_id != snapshot.artifact.id || content.version != 1 {
                return Err(invalid_import_plan());
            }
        }
        let valid = match snapshot.state {
            ArtifactImportState::Prepared => {
                snapshot.artifact.state == ArtifactState::Unavailable
                    && snapshot.content.is_none()
                    && snapshot.failure.is_none()
                    && snapshot.revision == 0
                    && snapshot.updated_at == snapshot.created_at
            }
            ArtifactImportState::ContentReady => {
                snapshot.artifact.state == ArtifactState::Unavailable
                    && snapshot.content.is_some()
                    && snapshot.failure.is_none()
                    && snapshot.revision == 1
            }
            ArtifactImportState::Committed => {
                snapshot.artifact.state == ArtifactState::Available
                    && snapshot.failure.is_none()
                    && snapshot.revision == 2
                    && snapshot.content.as_ref().is_some_and(|version| {
                        snapshot.artifact.content.as_ref() == Some(&version.summary())
                    })
            }
            ArtifactImportState::Failed => {
                snapshot.artifact.state == ArtifactState::Unavailable
                    && snapshot.failure.is_some()
                    && matches!(snapshot.revision, 1 | 2)
                    && (snapshot.content.is_some() == (snapshot.revision == 2))
            }
        };
        if !valid {
            return Err(invalid_import_plan());
        }
        Ok(snapshot)
    }

    /// Transitions a prepared journal to content-ready.
    pub fn record_content_ready(
        &mut self,
        content: ArtifactVersion,
        now: UnixMillis,
    ) -> Result<(), ApplicationError> {
        if self.state != ArtifactImportState::Prepared
            || content.artifact_id != self.artifact.id
            || content.version != 1
            || now < self.updated_at
        {
            return Err(invalid_import_plan());
        }
        ArtifactVersion::restore(content.clone())?;
        self.state = ArtifactImportState::ContentReady;
        self.content = Some(content);
        self.revision = 1;
        self.updated_at = now;
        Ok(())
    }

    /// Transitions content-ready to the exact committed artifact snapshot.
    pub fn commit(&mut self, artifact: Artifact, now: UnixMillis) -> Result<(), ApplicationError> {
        let Some(content) = &self.content else {
            return Err(invalid_import_plan());
        };
        if self.state != ArtifactImportState::ContentReady
            || artifact.id != self.artifact.id
            || artifact.project_id != self.artifact.project_id
            || artifact.thread_id != self.artifact.thread_id
            || artifact.name != self.artifact.name
            || artifact.created_at != self.artifact.created_at
            || artifact.content.as_ref() != Some(&content.summary())
            || artifact.state != ArtifactState::Available
            || now < self.updated_at
        {
            return Err(invalid_import_plan());
        }
        Artifact::restore(artifact.clone())?;
        self.artifact = artifact;
        self.state = ArtifactImportState::Committed;
        self.revision = 2;
        self.updated_at = now;
        Ok(())
    }

    /// Records a stable failure without discarding immutable staging metadata.
    pub fn fail(
        &mut self,
        failure: ArtifactImportFailureCode,
        now: UnixMillis,
    ) -> Result<(), ApplicationError> {
        if !matches!(
            self.state,
            ArtifactImportState::Prepared | ArtifactImportState::ContentReady
        ) || now < self.updated_at
        {
            return Err(invalid_import_plan());
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(invalid_import_plan)?;
        self.state = ArtifactImportState::Failed;
        self.failure = Some(failure);
        self.updated_at = now;
        Ok(())
    }
}

/// Result of atomically resolving or reserving an exact import command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactImportReservation {
    /// This call durably reserved the command and may consume its source path.
    NewlyPrepared(ArtifactImportPlan),
    /// Exact fingerprint replay; callers must never consume the source again.
    ExactReplay(ArtifactImportPlan),
}

/// Atomic content-ready transition result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactContentReadyResult {
    /// Quotas still permit the exact staged content and its metadata is durable.
    ContentReady(ArtifactImportPlan),
    /// The plan remains Prepared so private bytes can be removed before terminalization.
    QuotaExceeded {
        /// Unchanged prepared plan retaining the global operation slot.
        plan: ArtifactImportPlan,
        /// Exact stable quota reason observed atomically.
        failure: ArtifactImportFailureCode,
    },
}

/// Durable open journal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactOpenState {
    /// Exact open intent is durable and no platform dispatch began.
    Prepared,
    /// Dispatch intent is durable; completion certainty may become unknown.
    Dispatching,
    /// The platform boundary confirmed the exact version was opened.
    Opened,
    /// A stable known failure was recorded.
    Failed,
    /// Dispatch was interrupted with unknown completion certainty.
    InterruptedNeedsReview,
}

/// Stable known open failure safe to persist and project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactOpenFailureCode {
    /// Exact immutable content is absent.
    #[error("content_unavailable")]
    ContentUnavailable,
    /// The platform open boundary is unavailable.
    #[error("platform_unavailable")]
    PlatformUnavailable,
    /// The bounded dispatch deadline elapsed before dispatch started.
    #[error("deadline_exceeded")]
    DeadlineExceeded,
    /// Content or platform identity validation failed.
    #[error("integrity_failure")]
    IntegrityFailure,
    /// A prepared command was interrupted before platform dispatch.
    #[error("interrupted_before_dispatch")]
    InterruptedBeforeDispatch,
}

/// Certainty-aware failure returned by the native open boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactOpenError {
    /// The adapter proved that no external open side effect occurred.
    Known(ArtifactOpenFailureCode),
    /// Dispatch may have reached the desktop; automatic replay is forbidden.
    OutcomeUnknown,
}

/// Exact durable open plan without any host or storage path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactOpenPlan {
    /// Exact immutable version selected by the command fingerprint.
    pub content: ArtifactVersion,
    /// Current journal state.
    pub state: ArtifactOpenState,
    /// Stable code, present exactly in `Failed`.
    pub failure: Option<ArtifactOpenFailureCode>,
    /// Optimistic journal revision.
    pub revision: u64,
    /// Journal creation time.
    pub created_at: UnixMillis,
    /// Last durable transition time.
    pub updated_at: UnixMillis,
}

#[allow(clippy::missing_errors_doc)]
impl ArtifactOpenPlan {
    /// Creates a prepared exact-version open plan.
    pub fn prepared(content: ArtifactVersion, now: UnixMillis) -> Result<Self, ApplicationError> {
        ArtifactVersion::restore(content.clone())?;
        Ok(Self {
            content,
            state: ArtifactOpenState::Prepared,
            failure: None,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Validates a plan restored from durable storage.
    pub fn restore(snapshot: Self) -> Result<Self, ApplicationError> {
        ArtifactVersion::restore(snapshot.content.clone())?;
        if snapshot.updated_at < snapshot.created_at {
            return Err(invalid_open_plan());
        }
        let valid = match snapshot.state {
            ArtifactOpenState::Prepared => {
                snapshot.revision == 0
                    && snapshot.failure.is_none()
                    && snapshot.updated_at == snapshot.created_at
            }
            ArtifactOpenState::Dispatching => snapshot.revision == 1 && snapshot.failure.is_none(),
            ArtifactOpenState::Opened | ArtifactOpenState::InterruptedNeedsReview => {
                snapshot.revision == 2 && snapshot.failure.is_none()
            }
            ArtifactOpenState::Failed => {
                matches!(snapshot.revision, 1 | 2) && snapshot.failure.is_some()
            }
        };
        if !valid {
            return Err(invalid_open_plan());
        }
        Ok(snapshot)
    }

    /// Persists dispatch intent before calling the platform opener.
    pub fn begin_dispatch(&mut self, now: UnixMillis) -> Result<(), ApplicationError> {
        self.transition_open(
            ArtifactOpenState::Prepared,
            ArtifactOpenState::Dispatching,
            None,
            now,
        )
    }

    /// Records known platform success.
    pub fn complete(&mut self, now: UnixMillis) -> Result<(), ApplicationError> {
        self.transition_open(
            ArtifactOpenState::Dispatching,
            ArtifactOpenState::Opened,
            None,
            now,
        )
    }

    /// Records a known failure before or after dispatch.
    pub fn fail(
        &mut self,
        failure: ArtifactOpenFailureCode,
        now: UnixMillis,
    ) -> Result<(), ApplicationError> {
        if !matches!(
            self.state,
            ArtifactOpenState::Prepared | ArtifactOpenState::Dispatching
        ) {
            return Err(invalid_open_plan());
        }
        let expected = self.state;
        self.transition_open(expected, ArtifactOpenState::Failed, Some(failure), now)
    }

    /// Records unknown completion certainty; it is never auto-replayed.
    pub fn interrupt(&mut self, now: UnixMillis) -> Result<(), ApplicationError> {
        self.transition_open(
            ArtifactOpenState::Dispatching,
            ArtifactOpenState::InterruptedNeedsReview,
            None,
            now,
        )
    }

    fn transition_open(
        &mut self,
        expected: ArtifactOpenState,
        next: ArtifactOpenState,
        failure: Option<ArtifactOpenFailureCode>,
        now: UnixMillis,
    ) -> Result<(), ApplicationError> {
        if self.state != expected || now < self.updated_at {
            return Err(invalid_open_plan());
        }
        self.revision = self.revision.checked_add(1).ok_or_else(invalid_open_plan)?;
        self.state = next;
        self.failure = failure;
        self.updated_at = now;
        Ok(())
    }
}

/// Result of atomically resolving or preparing an exact open command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactOpenReservation {
    /// This call durably prepared the command and may dispatch it once.
    NewlyPrepared(ArtifactOpenPlan),
    /// Exact fingerprint replay; callers must never dispatch it again.
    ExactReplay(ArtifactOpenPlan),
}

/// Bounded open result without a host or storage path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactOpenReceiptStatus {
    /// The platform confirmed the open request.
    Opened,
    /// A stable known failure was recorded.
    Failed,
    /// Completion certainty is unknown and requires explicit review.
    InterruptedNeedsReview,
}

/// Canonical receipt for one terminal open command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactOpenReceipt {
    /// Exact artifact opened or attempted.
    pub artifact_id: ArtifactId,
    /// Exact immutable version opened or attempted.
    pub content_version: u32,
    /// Terminal bounded status.
    pub status: ArtifactOpenReceiptStatus,
    /// Stable known failure only when status is `Failed`.
    pub failure: Option<ArtifactOpenFailureCode>,
}

/// Project/global usage read for early quota rejection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactQuotaUsage {
    /// Live artifacts in the selected project.
    pub project_artifact_count: u64,
    /// Committed bytes in the selected project.
    pub project_bytes: u64,
    /// Committed bytes in the local profile.
    pub global_bytes: u64,
}

/// Content metadata produced after bounded, identity-revalidated staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedArtifactContent {
    /// SHA-256 of the exact staged bytes.
    pub sha256: [u8; 32],
    /// Validated media type retained for the immutable version.
    pub media_type: String,
    /// Exact staged byte count.
    pub byte_size: u64,
}

/// Durable content visibility used by idempotent import recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactContentStatus {
    /// Private staging bytes exist and may be published idempotently.
    Prepared,
    /// Exact content is durably published.
    Published,
    /// Neither exact staged nor published content exists.
    Missing,
}

/// Result of one idempotent content publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactContentPublication {
    /// This call made the exact content visible.
    Published,
    /// Exact content was already visible.
    AlreadyPublished,
}

/// Result of one idempotent exact-version private-namespace purge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactContentPurge {
    /// This call unlinked and durably synchronized the exact private entry.
    Purged,
    /// The exact entry was already absent and that absence was synchronized.
    AlreadyAbsent,
}

/// Closed path-free failure returned by local-content retention adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactRetentionFailureCode {
    /// The bounded purge deadline elapsed.
    #[error("deadline_exceeded")]
    DeadlineExceeded,
    /// Exact object identity or private-store invariants failed validation.
    #[error("integrity_failure")]
    IntegrityFailure,
    /// The qualified private content store is unavailable.
    #[error("content_store_unavailable")]
    ContentStoreUnavailable,
}

/// Focused durable artifact and operation-journal boundary.
///
/// Implementations must enforce the four public quota constants atomically in
/// `reserve_import` and `mark_content_ready`. Project-count rejection happens
/// before intent persistence, and `reserve_import` must race-check the same cap
/// atomically (returning `StoreError::Conflict` without inserting a row). Byte
/// quota rejection after reservation persists a terminal `Failed` plan. Exact
/// idempotency begins only after durable reservation. Compound commit methods
/// are single database transactions.
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// Resolves an exact durable command before volatile lifecycle/quota checks.
    async fn resolve_import(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactImportPlan>, StoreError>;

    /// Atomically resolves an exact command or inserts artifact + prepared journal.
    async fn reserve_import(
        &self,
        artifact: Artifact,
        command: &MutationCommand,
    ) -> Result<ArtifactImportReservation, StoreError>;

    /// Atomically records immutable staged metadata or reports a quota failure
    /// while retaining the Prepared plan for required content cleanup.
    async fn mark_content_ready(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        content: ArtifactVersion,
        now: UnixMillis,
    ) -> Result<ArtifactContentReadyResult, StoreError>;

    /// Atomically inserts the version, makes the artifact available, and commits the journal.
    async fn commit_import(
        &self,
        artifact: Artifact,
        expected_artifact_revision: u64,
        expected_import_revision: u64,
        content: ArtifactVersion,
        now: UnixMillis,
    ) -> Result<ArtifactImportPlan, StoreError>;

    /// Atomically records a stable terminal import failure.
    async fn fail_import(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        failure: ArtifactImportFailureCode,
        now: UnixMillis,
    ) -> Result<ArtifactImportPlan, StoreError>;

    /// Lists a bounded stable set of `Prepared` and `ContentReady` journals.
    async fn list_incomplete_imports(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactImportPlan>, StoreError>;

    /// Loads one canonical artifact.
    async fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact, StoreError>;

    /// Lists artifacts in stable recent-update order.
    async fn list_artifacts(
        &self,
        project_id: &ProjectId,
        after: Option<&ArtifactId>,
        limit: usize,
    ) -> Result<Vec<Artifact>, StoreError>;

    /// Loads one exact immutable version.
    async fn get_artifact_version(
        &self,
        artifact_id: &ArtifactId,
        version: u32,
    ) -> Result<ArtifactVersion, StoreError>;

    /// Returns bounded current usage for early rejection; transitions still recheck atomically.
    async fn quota_usage(&self, project_id: &ProjectId) -> Result<ArtifactQuotaUsage, StoreError>;

    /// Resolves an exact durable open command before current lifecycle checks.
    async fn resolve_open(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactOpenPlan>, StoreError>;

    /// Atomically resolves an exact command or inserts a prepared open journal.
    async fn prepare_open(
        &self,
        content: ArtifactVersion,
        command: &MutationCommand,
        now: UnixMillis,
    ) -> Result<ArtifactOpenReservation, StoreError>;

    /// Atomically persists dispatching before any platform side effect.
    async fn mark_open_dispatching(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError>;

    /// Atomically records known successful platform completion.
    async fn complete_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError>;

    /// Atomically records a stable known platform failure.
    async fn fail_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        failure: ArtifactOpenFailureCode,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError>;

    /// Atomically marks dispatch completion certainty unknown; never replayed.
    async fn interrupt_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError>;

    /// Lists a bounded stable set of prepared and dispatching open journals.
    async fn list_incomplete_opens(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactOpenPlan>, StoreError>;

    /// Resolves an exact durable removal command before current lifecycle or
    /// platform-readiness checks.
    async fn resolve_removal(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactRemovalPlan>, StoreError>;

    /// Atomically resolves an exact command or tombstones the artifact,
    /// reserves every retained version for purge, and inserts a pending journal.
    async fn reserve_removal(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        expected_content_version: u32,
        command: &MutationCommand,
        now: UnixMillis,
    ) -> Result<ArtifactRemovalReservation, StoreError>;

    /// Lists a bounded stable set of exact versions awaiting purge in version order.
    async fn list_pending_removal_versions(
        &self,
        artifact_id: &ArtifactId,
        limit: usize,
    ) -> Result<Vec<ArtifactRetentionRecord>, StoreError>;

    /// Atomically records confirmed durable absence of one exact version.
    async fn mark_content_purged(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactRetentionRecord, StoreError>;

    /// Commits removal only when no retained or purge-pending version remains.
    async fn commit_removal(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactRemovalPlan, StoreError>;

    /// Lists a bounded stable set of pending removal journals.
    async fn list_incomplete_removals(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactRemovalPlan>, StoreError>;
}

/// Qualified private content store. It never accepts or returns a storage path.
///
/// `prepare_import_content` must revalidate source identity at open and EOF,
/// enforce `max_bytes` while streaming, and be cancellation-safe. Orphan private
/// staging objects are adapter-owned cleanup; application recovery never needs
/// the selected source again. Publication is content-addressed and idempotent.
#[async_trait]
pub trait ArtifactContentStore: Send + Sync {
    /// Streams one ephemeral source into private staging and returns exact metadata.
    #[allow(clippy::too_many_arguments)]
    async fn prepare_import_content(
        &self,
        source: &SelectedSourcePath,
        artifact_id: &ArtifactId,
        content_version: u32,
        media_type: &str,
        max_bytes: u64,
        deadline_unix_ms: UnixMillis,
    ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode>;

    /// Idempotently publishes exact prepared content.
    async fn publish_content(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPublication, ArtifactImportFailureCode>;

    /// Reconciles exact private content without publishing or deleting it.
    async fn content_status(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentStatus, ArtifactImportFailureCode>;

    /// Deletes unpublished staging bytes before a terminal journal transition.
    async fn discard_prepared_content(
        &self,
        content: &ArtifactVersion,
    ) -> Result<(), ArtifactImportFailureCode>;

    /// Deletes deterministic staging left before digest journaling.
    async fn discard_reserved_content(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
    ) -> Result<(), ArtifactImportFailureCode>;
}

/// Qualified platform boundary for opening exact private content.
///
/// Implementations derive object identity from the version fields and never
/// expose a path. Once called, cancellation has unknown side-effect certainty;
/// the application therefore records `InterruptedNeedsReview` on timeout.
#[async_trait]
pub trait ArtifactOpener: Send + Sync {
    /// Opens one exact immutable version through the native platform boundary.
    async fn open_artifact(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<(), ArtifactOpenError>;
}

/// Qualified exact-version private-content retention boundary.
///
/// Implementations derive object identity from immutable version fields and
/// never accept or expose a host path. A missing object is successful only
/// after its containing directory is synchronized. Cancellation before the
/// unlink linearization point must not remove bytes; cancellation afterward
/// completes reconciliation under adapter-owned serialization.
#[async_trait]
pub trait ArtifactContentRetention: Send + Sync {
    /// Idempotently removes one exact private namespace entry with directory durability.
    /// Already-open descriptors may outlive the unlink; physical erasure is not promised.
    async fn purge_content(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPurge, ArtifactRetentionFailureCode>;
}

/// Bounded startup import-recovery result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactImportRecoverySummary {
    /// Content-ready imports reconciled to committed.
    pub committed: usize,
    /// Prepared/missing imports moved to a stable failure.
    pub failed: usize,
    /// True when more incomplete rows remain.
    pub truncated: bool,
}

/// Bounded startup open-recovery result. No platform open is replayed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactOpenRecoverySummary {
    /// Prepared rows failed before dispatch.
    pub failed_before_dispatch: usize,
    /// Dispatching rows moved to explicit review.
    pub interrupted_needs_review: usize,
    /// True when more incomplete rows remain.
    pub truncated: bool,
}

/// Bounded startup removal-recovery result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactRemovalRecoverySummary {
    /// Pending tombstones whose retained versions were durably purged and committed.
    pub committed: usize,
    /// True when more pending removal journals remain.
    pub truncated: bool,
}

/// Daemon-owned artifact import/open coordinator.
pub struct ArtifactService {
    artifacts: Arc<dyn ArtifactStore>,
    content: Arc<dyn ArtifactContentStore>,
    opener: Arc<dyn ArtifactOpener>,
    retention: Option<Arc<dyn ArtifactContentRetention>>,
    workspace: Arc<dyn WorkspaceStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    import_io_timeout_ms: u64,
    open_timeout_ms: u64,
    removal_io_timeout_ms: u64,
    removal_execution: tokio::sync::Mutex<()>,
}

#[allow(clippy::missing_errors_doc)]
impl ArtifactService {
    /// Creates the coordinator from focused persistence, content, platform, and workspace ports.
    #[must_use]
    pub fn new(
        artifacts: Arc<dyn ArtifactStore>,
        content: Arc<dyn ArtifactContentStore>,
        opener: Arc<dyn ArtifactOpener>,
        workspace: Arc<dyn WorkspaceStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            artifacts,
            content,
            opener,
            retention: None,
            workspace,
            clock,
            ids,
            import_io_timeout_ms: ARTIFACT_IMPORT_IO_TIMEOUT_MS,
            open_timeout_ms: ARTIFACT_OPEN_TIMEOUT_MS,
            removal_io_timeout_ms: ARTIFACT_REMOVAL_IO_TIMEOUT_MS,
            removal_execution: tokio::sync::Mutex::new(()),
        }
    }

    /// Adds a qualified exact-version private-content retention boundary.
    #[must_use]
    pub fn with_content_retention(mut self, retention: Arc<dyn ArtifactContentRetention>) -> Self {
        self.retention = Some(retention);
        self
    }

    #[cfg(test)]
    fn with_inner_timeouts(mut self, import_io_timeout_ms: u64, open_timeout_ms: u64) -> Self {
        self.import_io_timeout_ms = import_io_timeout_ms;
        self.open_timeout_ms = open_timeout_ms;
        self.removal_io_timeout_ms = import_io_timeout_ms;
        self
    }

    /// Imports one native-selected file under an exact durable command.
    ///
    /// Exact replays never consume the selected source again. A replay of an
    /// incomplete journal waits for startup recovery rather than duplicating I/O.
    pub async fn import_artifact(
        &self,
        input: ImportArtifact,
        idempotency_key: &str,
    ) -> Result<Artifact, ApplicationError> {
        let (project_id, thread_id, command) = import_command(&input, idempotency_key)?;
        if let Some(plan) = self.artifacts.resolve_import(&command).await? {
            return replay_import(plan);
        }
        let project = self.workspace.get_project(&project_id).await?;
        if project.state != ProjectState::Active {
            return Err(ApplicationError::InvalidState("project is archived".into()));
        }
        if let Some(thread_id) = &thread_id {
            let thread = self.workspace.get_thread(thread_id).await?;
            if thread.project_id != project_id || thread.state != ThreadState::Open {
                return Err(ApplicationError::InvalidInput(
                    "artifact thread is not open in the selected project".into(),
                ));
            }
        }
        let now = self.clock.now();
        let artifact = Artifact::new_unavailable(
            ArtifactId::new(self.ids.generate("artifact"))?,
            project_id.clone(),
            thread_id,
            input.display_name,
            now,
        )?;
        if self
            .artifacts
            .quota_usage(&project_id)
            .await?
            .project_artifact_count
            >= MAX_PROJECT_ARTIFACT_COUNT
        {
            return import_failure_code(ArtifactImportFailureCode::ProjectCountQuotaExceeded);
        }
        match self.artifacts.reserve_import(artifact, &command).await? {
            ArtifactImportReservation::ExactReplay(plan) => replay_import(plan),
            ArtifactImportReservation::NewlyPrepared(plan) => {
                self.execute_new_import(plan, input.source, input.media_type)
                    .await
            }
        }
    }

    /// Resolves only a previously durable import command. Volatile platform
    /// readiness must be checked after this so terminal results remain replayable.
    pub async fn replay_import_if_known(
        &self,
        input: &ImportArtifact,
        idempotency_key: &str,
    ) -> Result<Option<Artifact>, ApplicationError> {
        let (_, _, command) = import_command(input, idempotency_key)?;
        self.artifacts
            .resolve_import(&command)
            .await?
            .map(replay_import)
            .transpose()
    }

    /// Loads one canonical artifact.
    pub async fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact, ApplicationError> {
        Ok(self.artifacts.get_artifact(id).await?)
    }

    /// Lists one bounded keyset page of artifacts.
    pub async fn list_artifacts(
        &self,
        project_id: &ProjectId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Artifact>, ApplicationError> {
        if !(1..=200).contains(&limit) {
            return Err(ApplicationError::InvalidInput(
                "page limit must be between 1 and 200".into(),
            ));
        }
        let after = cursor.map(ArtifactId::new).transpose()?;
        let mut items = self
            .artifacts
            .list_artifacts(project_id, after.as_ref(), limit + 1)
            .await?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = has_more
            .then(|| items.last().map(|artifact| artifact.id.as_str().to_owned()))
            .flatten();
        Ok(Page { items, next_cursor })
    }

    /// Opens one exact available version after persisting intent and dispatch state.
    pub async fn open_artifact(
        &self,
        input: OpenArtifact,
        idempotency_key: &str,
    ) -> Result<ArtifactOpenReceipt, ApplicationError> {
        let (artifact_id, content, command) = self.open_command(&input, idempotency_key).await?;
        if let Some(plan) = self.artifacts.resolve_open(&command).await? {
            return receipt_for_replay(&plan);
        }
        let artifact = self.artifacts.get_artifact(&artifact_id).await?;
        if artifact.state != ArtifactState::Available
            || artifact
                .content
                .as_ref()
                .is_none_or(|summary| summary.content_version != input.content_version)
        {
            return Err(ApplicationError::InvalidState(
                "exact artifact content is unavailable".into(),
            ));
        }
        match self
            .artifacts
            .prepare_open(content, &command, self.clock.now())
            .await?
        {
            ArtifactOpenReservation::ExactReplay(plan) => receipt_for_replay(&plan),
            ArtifactOpenReservation::NewlyPrepared(plan) => self.execute_new_open(plan).await,
        }
    }

    /// Resolves only a previously durable exact-version open command before
    /// volatile portal readiness is considered.
    pub async fn replay_open_if_known(
        &self,
        input: &OpenArtifact,
        idempotency_key: &str,
    ) -> Result<Option<ArtifactOpenReceipt>, ApplicationError> {
        let (_, _, command) = self.open_command(input, idempotency_key).await?;
        self.artifacts
            .resolve_open(&command)
            .await?
            .as_ref()
            .map(receipt_for_replay)
            .transpose()
    }

    async fn open_command(
        &self,
        input: &OpenArtifact,
        idempotency_key: &str,
    ) -> Result<(ArtifactId, ArtifactVersion, MutationCommand), ApplicationError> {
        let artifact_id = ArtifactId::new(input.artifact_id.clone())?;
        let content = self
            .artifacts
            .get_artifact_version(&artifact_id, input.content_version)
            .await?;
        let version_bytes = input.content_version.to_be_bytes();
        let command = mutation_command_bytes(
            ARTIFACT_OPEN_SCOPE,
            idempotency_key,
            &[
                artifact_id.as_str().as_bytes(),
                &version_bytes,
                &content.sha256,
            ],
        )?;
        Ok((artifact_id, content, command))
    }

    /// Atomically tombstones one exact current artifact and durably purges all
    /// locally retained immutable versions.
    pub async fn remove_artifact(
        &self,
        input: RemoveArtifact,
        idempotency_key: &str,
    ) -> Result<Artifact, ApplicationError> {
        let (artifact_id, command) = removal_command(&input, idempotency_key)?;
        let _execution = self.removal_execution.lock().await;
        if let Some(plan) = self.artifacts.resolve_removal(&command).await? {
            return self.resume_or_replay_removal(plan).await;
        }
        let artifact = self.artifacts.get_artifact(&artifact_id).await?;
        if artifact.state != ArtifactState::Available {
            return Err(ApplicationError::InvalidState(
                "artifact content is not currently available".into(),
            ));
        }
        if artifact.revision != input.expected_revision
            || artifact
                .content
                .as_ref()
                .is_none_or(|content| content.content_version != input.expected_content_version)
        {
            return Err(ApplicationError::Conflict);
        }
        self.content_retention()?;
        match self
            .artifacts
            .reserve_removal(
                &artifact_id,
                input.expected_revision,
                input.expected_content_version,
                &command,
                self.clock.now(),
            )
            .await?
        {
            ArtifactRemovalReservation::ExactReplay(plan)
            | ArtifactRemovalReservation::NewlyPending(plan) => {
                self.resume_or_replay_removal(plan).await
            }
        }
    }

    /// Resolves only a previously durable exact removal before volatile
    /// private-content readiness is considered.
    pub async fn replay_removal_if_known(
        &self,
        input: &RemoveArtifact,
        idempotency_key: &str,
    ) -> Result<Option<Artifact>, ApplicationError> {
        match self.resolve_removal(input, idempotency_key).await? {
            ArtifactRemovalResolution::Unknown => Ok(None),
            ArtifactRemovalResolution::Pending { .. } => Err(ApplicationError::Unavailable(
                "artifact removal is pending recovery".into(),
            )),
            ArtifactRemovalResolution::Committed { artifact } => Ok(Some(artifact)),
        }
    }

    /// Resolves one exact removal command without private-content or readiness I/O.
    pub async fn resolve_removal(
        &self,
        input: &RemoveArtifact,
        idempotency_key: &str,
    ) -> Result<ArtifactRemovalResolution, ApplicationError> {
        let (_, command) = removal_command(input, idempotency_key)?;
        let Some(plan) = self.artifacts.resolve_removal(&command).await? else {
            return Ok(ArtifactRemovalResolution::Unknown);
        };
        ArtifactRemovalPlan::restore(plan.clone())?;
        Ok(match plan.state {
            ArtifactRemovalState::Pending => ArtifactRemovalResolution::Pending {
                artifact: plan.artifact,
            },
            ArtifactRemovalState::Committed => ArtifactRemovalResolution::Committed {
                artifact: plan.artifact,
            },
        })
    }

    /// Recovers bounded pending removals without restoring content access or
    /// releasing byte quota before exact purge confirmation.
    pub async fn recover_incomplete_removals(
        &self,
        limit: usize,
    ) -> Result<ArtifactRemovalRecoverySummary, ApplicationError> {
        validate_recovery_limit(limit)?;
        self.content_retention()?;
        let _execution = self.removal_execution.lock().await;
        let plans = self.artifacts.list_incomplete_removals(limit + 1).await?;
        let mut summary = ArtifactRemovalRecoverySummary {
            truncated: plans.len() > limit,
            ..ArtifactRemovalRecoverySummary::default()
        };
        for plan in plans.into_iter().take(limit) {
            ArtifactRemovalPlan::restore(plan.clone())?;
            if plan.state != ArtifactRemovalState::Pending {
                return Err(invalid_removal_plan());
            }
            self.execute_pending_removal(plan).await?;
            summary.committed += 1;
        }
        Ok(summary)
    }

    async fn resume_or_replay_removal(
        &self,
        plan: ArtifactRemovalPlan,
    ) -> Result<Artifact, ApplicationError> {
        ArtifactRemovalPlan::restore(plan.clone())?;
        match plan.state {
            ArtifactRemovalState::Pending => self.execute_pending_removal(plan).await,
            ArtifactRemovalState::Committed => replay_removal(plan),
        }
    }

    async fn execute_pending_removal(
        &self,
        plan: ArtifactRemovalPlan,
    ) -> Result<Artifact, ApplicationError> {
        ArtifactRemovalPlan::restore(plan.clone())?;
        if plan.state != ArtifactRemovalState::Pending {
            return replay_removal(plan);
        }
        let retention = self.content_retention()?;
        let deadline = deadline(self.clock.now(), self.removal_io_timeout_ms)?;
        let timeout_at = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(self.removal_io_timeout_ms))
            .ok_or_else(|| ApplicationError::InvalidState("artifact deadline overflow".into()))?;
        loop {
            let pending = self
                .artifacts
                .list_pending_removal_versions(&plan.artifact.id, MAX_ARTIFACT_RECOVERY_BATCH)
                .await?;
            if pending.len() > MAX_ARTIFACT_RECOVERY_BATCH {
                return Err(invalid_retention_record());
            }
            if pending.is_empty() {
                break;
            }
            for record in pending {
                ArtifactRetentionRecord::restore(record.clone())?;
                if record.content.artifact_id != plan.artifact.id
                    || record.state != ArtifactRetentionState::PurgePending
                {
                    return Err(invalid_retention_record());
                }
                tokio::time::timeout_at(
                    timeout_at,
                    retention.purge_content(&record.content, deadline),
                )
                .await
                .map_err(|_| ApplicationError::DeadlineExceeded)?
                .map_err(application_error_for_retention_failure)?;
                let purged = self
                    .artifacts
                    .mark_content_purged(
                        &plan.artifact.id,
                        record.content.version,
                        record.revision,
                        self.clock.now().max(record.updated_at),
                    )
                    .await?;
                ArtifactRetentionRecord::restore(purged.clone())?;
                if purged.state != ArtifactRetentionState::Purged
                    || purged.content != record.content
                {
                    return Err(invalid_retention_record());
                }
            }
        }
        let committed = self
            .artifacts
            .commit_removal(
                &plan.artifact.id,
                plan.revision,
                self.clock.now().max(plan.updated_at),
            )
            .await?;
        ArtifactRemovalPlan::restore(committed.clone())?;
        replay_removal(committed)
    }

    fn content_retention(&self) -> Result<&Arc<dyn ArtifactContentRetention>, ApplicationError> {
        self.retention.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("artifact retention is not configured".into())
        })
    }

    /// Recovers imports without reusing any selected source path.
    pub async fn recover_incomplete_imports(
        &self,
        limit: usize,
    ) -> Result<ArtifactImportRecoverySummary, ApplicationError> {
        validate_recovery_limit(limit)?;
        let plans = self.artifacts.list_incomplete_imports(limit + 1).await?;
        let mut summary = ArtifactImportRecoverySummary {
            truncated: plans.len() > limit,
            ..ArtifactImportRecoverySummary::default()
        };
        for plan in plans.into_iter().take(limit) {
            ArtifactImportPlan::restore(plan.clone())?;
            match plan.state {
                ArtifactImportState::Prepared => {
                    self.content
                        .discard_reserved_content(&plan.artifact.id, 1)
                        .await
                        .map_err(application_error_for_import_failure)?;
                    self.artifacts
                        .fail_import(
                            &plan.artifact.id,
                            plan.revision,
                            ArtifactImportFailureCode::InterruptedBeforeContentReady,
                            self.clock.now().max(plan.updated_at),
                        )
                        .await?;
                    summary.failed += 1;
                }
                ArtifactImportState::ContentReady => {
                    if self.recover_content_ready_import(plan).await? {
                        summary.committed += 1;
                    } else {
                        summary.failed += 1;
                    }
                }
                ArtifactImportState::Committed | ArtifactImportState::Failed => {
                    return Err(invalid_import_plan());
                }
            }
        }
        Ok(summary)
    }

    /// Recovers open journals without ever replaying a platform open.
    pub async fn recover_incomplete_opens(
        &self,
        limit: usize,
    ) -> Result<ArtifactOpenRecoverySummary, ApplicationError> {
        validate_recovery_limit(limit)?;
        let plans = self.artifacts.list_incomplete_opens(limit + 1).await?;
        let mut summary = ArtifactOpenRecoverySummary {
            truncated: plans.len() > limit,
            ..ArtifactOpenRecoverySummary::default()
        };
        for plan in plans.into_iter().take(limit) {
            ArtifactOpenPlan::restore(plan.clone())?;
            let now = self.clock.now().max(plan.updated_at);
            match plan.state {
                ArtifactOpenState::Prepared => {
                    self.artifacts
                        .fail_open(
                            &plan.content.artifact_id,
                            plan.content.version,
                            plan.revision,
                            ArtifactOpenFailureCode::InterruptedBeforeDispatch,
                            now,
                        )
                        .await?;
                    summary.failed_before_dispatch += 1;
                }
                ArtifactOpenState::Dispatching => {
                    self.artifacts
                        .interrupt_open(
                            &plan.content.artifact_id,
                            plan.content.version,
                            plan.revision,
                            now,
                        )
                        .await?;
                    summary.interrupted_needs_review += 1;
                }
                ArtifactOpenState::Opened
                | ArtifactOpenState::Failed
                | ArtifactOpenState::InterruptedNeedsReview => {
                    return Err(invalid_open_plan());
                }
            }
        }
        Ok(summary)
    }

    async fn execute_new_import(
        &self,
        plan: ArtifactImportPlan,
        source: SelectedSourcePath,
        media_type: String,
    ) -> Result<Artifact, ApplicationError> {
        ArtifactImportPlan::restore(plan.clone())?;
        if plan.state == ArtifactImportState::Failed {
            return import_failure(&plan);
        }
        if plan.state != ArtifactImportState::Prepared {
            return Err(invalid_import_plan());
        }
        let deadline = deadline(self.clock.now(), self.import_io_timeout_ms)?;
        let timeout_at = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(self.import_io_timeout_ms))
            .ok_or_else(|| ApplicationError::InvalidState("artifact deadline overflow".into()))?;
        let prepared = match tokio::time::timeout_at(
            timeout_at,
            self.content.prepare_import_content(
                &source,
                &plan.artifact.id,
                1,
                &media_type,
                MAX_ARTIFACT_FILE_BYTES,
                deadline,
            ),
        )
        .await
        {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(failure)) => {
                return self
                    .record_import_failure_until(plan, failure, timeout_at, deadline)
                    .await;
            }
            // Dropping the bounded adapter future signals its blocking worker.
            // Do not wait on deterministic cleanup after the deadline: the
            // Prepared journal retains ownership until exact recovery.
            Err(_) => return Err(ApplicationError::DeadlineExceeded),
        };
        if prepared.byte_size > MAX_ARTIFACT_FILE_BYTES {
            return self
                .record_import_failure_until(
                    plan,
                    ArtifactImportFailureCode::FileTooLarge,
                    timeout_at,
                    deadline,
                )
                .await;
        }
        let created_at = self.clock.now().max(plan.updated_at);
        let content = ArtifactVersion::new(
            plan.artifact.id.clone(),
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            created_at,
        )?;
        let ready = self
            .artifacts
            .mark_content_ready(
                &plan.artifact.id,
                plan.revision,
                content.clone(),
                created_at,
            )
            .await?;
        match ready {
            ArtifactContentReadyResult::ContentReady(ready) => {
                self.publish_and_commit_until(ready, timeout_at, deadline)
                    .await
            }
            ArtifactContentReadyResult::QuotaExceeded { plan, failure } => {
                self.record_import_failure_until(plan, failure, timeout_at, deadline)
                    .await
            }
        }
    }

    #[cfg(test)]
    async fn publish_and_commit(
        &self,
        plan: ArtifactImportPlan,
    ) -> Result<Artifact, ApplicationError> {
        let deadline = deadline(self.clock.now(), self.import_io_timeout_ms)?;
        let timeout_at = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(self.import_io_timeout_ms))
            .ok_or_else(|| ApplicationError::InvalidState("artifact deadline overflow".into()))?;
        self.publish_and_commit_until(plan, timeout_at, deadline)
            .await
    }

    async fn publish_and_commit_until(
        &self,
        plan: ArtifactImportPlan,
        timeout_at: tokio::time::Instant,
        deadline: UnixMillis,
    ) -> Result<Artifact, ApplicationError> {
        let content = plan.content.clone().ok_or_else(invalid_import_plan)?;
        match tokio::time::timeout_at(timeout_at, self.content.publish_content(&content, deadline))
            .await
        {
            Ok(Ok(
                ArtifactContentPublication::Published
                | ArtifactContentPublication::AlreadyPublished,
            )) => {}
            Ok(Err(
                failure @ (ArtifactImportFailureCode::ContentStoreUnavailable
                | ArtifactImportFailureCode::DeadlineExceeded),
            )) => return Err(application_error_for_import_failure(failure)),
            Ok(Err(failure)) => {
                return self
                    .record_content_ready_failure(plan, failure, timeout_at, deadline)
                    .await;
            }
            // Publication cancellation is linearized by the content adapter.
            // Keep ContentReady ownership and reconcile the exact object on
            // recovery instead of waiting past the application deadline.
            Err(_) => return Err(ApplicationError::DeadlineExceeded),
        }
        let mut artifact = plan.artifact.clone();
        let now = self.clock.now().max(plan.updated_at);
        artifact.record_content(content.summary(), now)?;
        let committed = self
            .artifacts
            .commit_import(artifact, 0, plan.revision, content, now)
            .await?;
        if committed.state != ArtifactImportState::Committed {
            return Err(invalid_import_plan());
        }
        Ok(committed.artifact)
    }

    #[cfg(test)]
    async fn record_import_failure(
        &self,
        plan: ArtifactImportPlan,
        failure: ArtifactImportFailureCode,
    ) -> Result<Artifact, ApplicationError> {
        let deadline = deadline(self.clock.now(), self.import_io_timeout_ms)?;
        let timeout_at = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(self.import_io_timeout_ms))
            .ok_or_else(|| ApplicationError::InvalidState("artifact deadline overflow".into()))?;
        self.record_import_failure_until(plan, failure, timeout_at, deadline)
            .await
    }

    async fn record_import_failure_until(
        &self,
        plan: ArtifactImportPlan,
        failure: ArtifactImportFailureCode,
        timeout_at: tokio::time::Instant,
        deadline: UnixMillis,
    ) -> Result<Artifact, ApplicationError> {
        if plan.content.is_some() {
            return self
                .record_content_ready_failure(plan, failure, timeout_at, deadline)
                .await;
        }
        tokio::time::timeout_at(
            timeout_at,
            self.content.discard_reserved_content(&plan.artifact.id, 1),
        )
        .await
        .map_err(|_| ApplicationError::DeadlineExceeded)?
        .map_err(application_error_for_import_failure)?;
        let failed = self
            .artifacts
            .fail_import(
                &plan.artifact.id,
                plan.revision,
                failure,
                self.clock.now().max(plan.updated_at),
            )
            .await?;
        import_failure(&failed)
    }

    async fn record_content_ready_failure(
        &self,
        plan: ArtifactImportPlan,
        failure: ArtifactImportFailureCode,
        timeout_at: tokio::time::Instant,
        deadline: UnixMillis,
    ) -> Result<Artifact, ApplicationError> {
        self.cleanup_content_ready_for_terminal(&plan, timeout_at, deadline)
            .await?;
        let failed = self.persist_import_failure(&plan, failure).await?;
        import_failure(&failed)
    }

    async fn cleanup_content_ready_for_terminal(
        &self,
        plan: &ArtifactImportPlan,
        timeout_at: tokio::time::Instant,
        deadline: UnixMillis,
    ) -> Result<(), ApplicationError> {
        let content = plan.content.as_ref().ok_or_else(invalid_import_plan)?;
        tokio::time::timeout_at(timeout_at, self.content.discard_prepared_content(content))
            .await
            .map_err(|_| ApplicationError::DeadlineExceeded)?
            .map_err(application_error_for_import_failure)?;
        let status =
            tokio::time::timeout_at(timeout_at, self.content.content_status(content, deadline))
                .await
                .map_err(|_| ApplicationError::DeadlineExceeded)?;
        match status {
            Ok(ArtifactContentStatus::Missing) => Ok(()),
            Err(
                failure @ (ArtifactImportFailureCode::ContentStoreUnavailable
                | ArtifactImportFailureCode::DeadlineExceeded),
            ) => Err(application_error_for_import_failure(failure)),
            Ok(ArtifactContentStatus::Prepared | ArtifactContentStatus::Published) | Err(_) => Err(
                ApplicationError::Integrity("artifact content requires quarantine review".into()),
            ),
        }
    }

    async fn recover_content_ready_import(
        &self,
        plan: ArtifactImportPlan,
    ) -> Result<bool, ApplicationError> {
        let content = plan.content.clone().ok_or_else(invalid_import_plan)?;
        let deadline = deadline(self.clock.now(), self.import_io_timeout_ms)?;
        let timeout_at = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(self.import_io_timeout_ms))
            .ok_or_else(|| ApplicationError::InvalidState("artifact deadline overflow".into()))?;
        let status =
            tokio::time::timeout_at(timeout_at, self.content.content_status(&content, deadline))
                .await
                .map_err(|_| ApplicationError::DeadlineExceeded)?;
        match status {
            Ok(ArtifactContentStatus::Published | ArtifactContentStatus::Prepared) => {
                self.publish_and_commit_until(plan, timeout_at, deadline)
                    .await?;
                Ok(true)
            }
            Ok(ArtifactContentStatus::Missing) => {
                self.cleanup_content_ready_for_terminal(&plan, timeout_at, deadline)
                    .await?;
                self.persist_import_failure(&plan, ArtifactImportFailureCode::IntegrityFailure)
                    .await?;
                Ok(false)
            }
            Err(
                failure @ (ArtifactImportFailureCode::ContentStoreUnavailable
                | ArtifactImportFailureCode::DeadlineExceeded),
            ) => Err(application_error_for_import_failure(failure)),
            Err(_) => {
                // A corrupt deterministic staging file can be removed by its
                // validated private namespace identity. A corrupt published
                // object cannot yet be quarantined safely, so keep the journal
                // ContentReady and Files degraded instead of releasing quota
                // ownership around unaccounted bytes.
                self.cleanup_content_ready_for_terminal(&plan, timeout_at, deadline)
                    .await?;
                self.persist_import_failure(&plan, ArtifactImportFailureCode::IntegrityFailure)
                    .await?;
                Ok(false)
            }
        }
    }

    async fn persist_import_failure(
        &self,
        plan: &ArtifactImportPlan,
        failure: ArtifactImportFailureCode,
    ) -> Result<ArtifactImportPlan, ApplicationError> {
        let failed = self
            .artifacts
            .fail_import(
                &plan.artifact.id,
                plan.revision,
                failure,
                self.clock.now().max(plan.updated_at),
            )
            .await?;
        ArtifactImportPlan::restore(failed.clone())?;
        if failed.state != ArtifactImportState::Failed || failed.failure != Some(failure) {
            return Err(invalid_import_plan());
        }
        Ok(failed)
    }

    async fn execute_new_open(
        &self,
        plan: ArtifactOpenPlan,
    ) -> Result<ArtifactOpenReceipt, ApplicationError> {
        ArtifactOpenPlan::restore(plan.clone())?;
        if plan.state != ArtifactOpenState::Prepared {
            return Err(invalid_open_plan());
        }
        let dispatching = self
            .artifacts
            .mark_open_dispatching(
                &plan.content.artifact_id,
                plan.content.version,
                plan.revision,
                self.clock.now().max(plan.updated_at),
            )
            .await?;
        let deadline = deadline(self.clock.now(), self.open_timeout_ms)?;
        match tokio::time::timeout(
            Duration::from_millis(self.open_timeout_ms),
            self.opener.open_artifact(&dispatching.content, deadline),
        )
        .await
        {
            Ok(Ok(())) => {
                let complete = self
                    .artifacts
                    .complete_open(
                        &dispatching.content.artifact_id,
                        dispatching.content.version,
                        dispatching.revision,
                        self.clock.now().max(dispatching.updated_at),
                    )
                    .await?;
                receipt_for_terminal(&complete)
            }
            Ok(Err(ArtifactOpenError::Known(failure))) => {
                let failed = self
                    .artifacts
                    .fail_open(
                        &dispatching.content.artifact_id,
                        dispatching.content.version,
                        dispatching.revision,
                        failure,
                        self.clock.now().max(dispatching.updated_at),
                    )
                    .await?;
                receipt_for_terminal(&failed)
            }
            Ok(Err(ArtifactOpenError::OutcomeUnknown)) | Err(_) => {
                let interrupted = self
                    .artifacts
                    .interrupt_open(
                        &dispatching.content.artifact_id,
                        dispatching.content.version,
                        dispatching.revision,
                        self.clock.now().max(dispatching.updated_at),
                    )
                    .await?;
                receipt_for_terminal(&interrupted)
            }
        }
    }
}

fn import_command(
    input: &ImportArtifact,
    idempotency_key: &str,
) -> Result<(ProjectId, Option<ThreadId>, MutationCommand), ApplicationError> {
    let project_id = ProjectId::new(input.project_id.clone())?;
    let thread_id = input
        .thread_id
        .as_ref()
        .map(|value| ThreadId::new(value.clone()))
        .transpose()?;
    validate_imported_file_name(&input.display_name)?;
    ArtifactContentSummary::new(1, input.media_type.clone(), 0)?;
    let thread_fingerprint = thread_id
        .as_ref()
        .map_or_else(|| b"none".to_vec(), |id| id.as_str().as_bytes().to_vec());
    // The selected path is deliberately excluded: even a hash would be a
    // guessable durable derivative. Exact retries are bound to the logical
    // destination metadata and never consume a newly supplied path.
    let command = mutation_command_bytes(
        ARTIFACT_IMPORT_SCOPE,
        idempotency_key,
        &[
            project_id.as_str().as_bytes(),
            &thread_fingerprint,
            input.display_name.as_bytes(),
            input.media_type.as_bytes(),
        ],
    )?;
    Ok((project_id, thread_id, command))
}

fn removal_command(
    input: &RemoveArtifact,
    idempotency_key: &str,
) -> Result<(ArtifactId, MutationCommand), ApplicationError> {
    let artifact_id = ArtifactId::new(input.artifact_id.clone())?;
    if input.expected_revision == 0
        || !(1..=MAX_ARTIFACT_CONTENT_VERSION).contains(&input.expected_content_version)
    {
        return Err(ApplicationError::InvalidInput(
            "artifact removal expectation is invalid".into(),
        ));
    }
    let revision = input.expected_revision.to_be_bytes();
    let content_version = input.expected_content_version.to_be_bytes();
    let command = mutation_command_bytes(
        ARTIFACT_REMOVAL_SCOPE,
        idempotency_key,
        &[artifact_id.as_str().as_bytes(), &revision, &content_version],
    )?;
    Ok((artifact_id, command))
}

fn replay_import(plan: ArtifactImportPlan) -> Result<Artifact, ApplicationError> {
    ArtifactImportPlan::restore(plan.clone())?;
    match plan.state {
        ArtifactImportState::Committed => Ok(plan.artifact),
        ArtifactImportState::Failed => import_failure(&plan),
        ArtifactImportState::Prepared | ArtifactImportState::ContentReady => Err(
            ApplicationError::Unavailable("artifact import is pending recovery".into()),
        ),
    }
}

fn replay_removal(plan: ArtifactRemovalPlan) -> Result<Artifact, ApplicationError> {
    ArtifactRemovalPlan::restore(plan.clone())?;
    match plan.state {
        ArtifactRemovalState::Committed => Ok(plan.artifact),
        ArtifactRemovalState::Pending => Err(ApplicationError::Unavailable(
            "artifact removal is pending recovery".into(),
        )),
    }
}

fn import_failure(plan: &ArtifactImportPlan) -> Result<Artifact, ApplicationError> {
    let failure = plan.failure.ok_or_else(invalid_import_plan)?;
    import_failure_code(failure)
}

fn import_failure_code(failure: ArtifactImportFailureCode) -> Result<Artifact, ApplicationError> {
    Err(application_error_for_import_failure(failure))
}

fn application_error_for_import_failure(failure: ArtifactImportFailureCode) -> ApplicationError {
    match failure {
        ArtifactImportFailureCode::DeadlineExceeded => ApplicationError::DeadlineExceeded,
        ArtifactImportFailureCode::IntegrityFailure | ArtifactImportFailureCode::SourceChanged => {
            ApplicationError::Integrity(failure.to_string())
        }
        ArtifactImportFailureCode::SourceUnavailable
        | ArtifactImportFailureCode::ContentStoreUnavailable => {
            ApplicationError::Unavailable(failure.to_string())
        }
        ArtifactImportFailureCode::FileTooLarge
        | ArtifactImportFailureCode::ProjectByteQuotaExceeded
        | ArtifactImportFailureCode::GlobalByteQuotaExceeded
        | ArtifactImportFailureCode::ProjectCountQuotaExceeded => {
            ApplicationError::InvalidInput(failure.to_string())
        }
        ArtifactImportFailureCode::InterruptedBeforeContentReady => {
            ApplicationError::InvalidState(failure.to_string())
        }
    }
}

fn application_error_for_retention_failure(
    failure: ArtifactRetentionFailureCode,
) -> ApplicationError {
    match failure {
        ArtifactRetentionFailureCode::DeadlineExceeded => ApplicationError::DeadlineExceeded,
        ArtifactRetentionFailureCode::IntegrityFailure => {
            ApplicationError::Integrity(failure.to_string())
        }
        ArtifactRetentionFailureCode::ContentStoreUnavailable => {
            ApplicationError::Unavailable(failure.to_string())
        }
    }
}

fn receipt_for_replay(plan: &ArtifactOpenPlan) -> Result<ArtifactOpenReceipt, ApplicationError> {
    ArtifactOpenPlan::restore(plan.clone())?;
    match plan.state {
        ArtifactOpenState::Opened
        | ArtifactOpenState::Failed
        | ArtifactOpenState::InterruptedNeedsReview => receipt_for_terminal(plan),
        ArtifactOpenState::Prepared | ArtifactOpenState::Dispatching => Err(
            ApplicationError::Unavailable("artifact open is pending recovery".into()),
        ),
    }
}

fn receipt_for_terminal(plan: &ArtifactOpenPlan) -> Result<ArtifactOpenReceipt, ApplicationError> {
    let status = match plan.state {
        ArtifactOpenState::Opened => ArtifactOpenReceiptStatus::Opened,
        ArtifactOpenState::Failed => ArtifactOpenReceiptStatus::Failed,
        ArtifactOpenState::InterruptedNeedsReview => {
            ArtifactOpenReceiptStatus::InterruptedNeedsReview
        }
        ArtifactOpenState::Prepared | ArtifactOpenState::Dispatching => {
            return Err(invalid_open_plan());
        }
    };
    Ok(ArtifactOpenReceipt {
        artifact_id: plan.content.artifact_id.clone(),
        content_version: plan.content.version,
        status,
        failure: plan.failure,
    })
}

fn deadline(now: UnixMillis, timeout_ms: u64) -> Result<UnixMillis, ApplicationError> {
    now.checked_add(timeout_ms)
        .ok_or_else(|| ApplicationError::InvalidState("artifact deadline overflow".into()))
}

fn validate_recovery_limit(limit: usize) -> Result<(), ApplicationError> {
    if limit == 0 || limit > MAX_ARTIFACT_RECOVERY_BATCH {
        return Err(ApplicationError::InvalidInput(format!(
            "artifact recovery limit must be between 1 and {MAX_ARTIFACT_RECOVERY_BATCH}"
        )));
    }
    Ok(())
}

fn invalid_import_plan() -> ApplicationError {
    ApplicationError::Integrity("invalid artifact import plan".into())
}

fn invalid_open_plan() -> ApplicationError {
    ApplicationError::Integrity("invalid artifact open plan".into())
}

fn invalid_removal_plan() -> ApplicationError {
    ApplicationError::Integrity("invalid artifact removal plan".into())
}

fn invalid_retention_record() -> ApplicationError {
    ApplicationError::Integrity("invalid artifact retention record".into())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        future::pending,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use grok_domain::{
        Automation, AutomationHistoryEntry, AutomationId, Message, MessageId, Project, Thread,
    };

    use super::*;
    use crate::{WorkspaceSearchHit, WorkspaceSearchKind};

    #[derive(Debug)]
    struct FixedClock(UnixMillis);

    impl Clock for FixedClock {
        fn now(&self) -> UnixMillis {
            self.0
        }
    }

    #[derive(Debug)]
    struct FixedIds;

    impl IdGenerator for FixedIds {
        fn generate(&self, prefix: &str) -> String {
            format!("{prefix}-test")
        }
    }

    #[derive(Debug)]
    struct TestWorkspace {
        project: Project,
    }

    #[async_trait]
    impl WorkspaceStore for TestWorkspace {
        async fn resolve_mutation(
            &self,
            _scope: &str,
            _command: &MutationCommand,
        ) -> Result<Option<String>, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn create_project(
            &self,
            _project: Project,
            _command: &MutationCommand,
        ) -> Result<Project, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn get_project(&self, id: &ProjectId) -> Result<Project, StoreError> {
            (id == &self.project.id)
                .then(|| self.project.clone())
                .ok_or(StoreError::NotFound)
        }

        async fn save_project(
            &self,
            _project: Project,
            _expected_revision: u64,
            _command: &MutationCommand,
        ) -> Result<(), StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn list_projects(
            &self,
            _after: Option<&ProjectId>,
            _limit: usize,
        ) -> Result<Vec<Project>, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn create_thread(
            &self,
            _thread: Thread,
            _command: &MutationCommand,
        ) -> Result<Thread, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn get_thread(&self, _id: &ThreadId) -> Result<Thread, StoreError> {
            Err(StoreError::NotFound)
        }

        async fn save_thread(
            &self,
            _thread: Thread,
            _expected_revision: u64,
            _command: &MutationCommand,
        ) -> Result<(), StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn list_threads(
            &self,
            _project_id: &ProjectId,
            _after: Option<&ThreadId>,
            _limit: usize,
        ) -> Result<Vec<Thread>, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn create_message(
            &self,
            _message: Message,
            _command: &MutationCommand,
        ) -> Result<Message, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn get_message(&self, _id: &MessageId) -> Result<Message, StoreError> {
            Err(StoreError::NotFound)
        }

        async fn save_message(
            &self,
            _message: Message,
            _expected_revision: u64,
            _command: &MutationCommand,
        ) -> Result<(), StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn list_messages(
            &self,
            _thread_id: &ThreadId,
            _after: Option<&MessageId>,
            _limit: usize,
        ) -> Result<Vec<Message>, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn create_automation(
            &self,
            _automation: Automation,
            _command: &MutationCommand,
        ) -> Result<Automation, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn get_automation(&self, _id: &AutomationId) -> Result<Automation, StoreError> {
            Err(StoreError::NotFound)
        }

        async fn save_automation(
            &self,
            _automation: Automation,
            _expected_revision: u64,
            _command: &MutationCommand,
        ) -> Result<(), StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn list_automations(
            &self,
            _project_id: &ProjectId,
            _after: Option<&AutomationId>,
            _limit: usize,
        ) -> Result<Vec<Automation>, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn record_automation_history(
            &self,
            _entry: AutomationHistoryEntry,
        ) -> Result<AutomationHistoryEntry, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn automation_history(
            &self,
            _automation_id: &AutomationId,
            _after_sequence: u64,
            _limit: usize,
        ) -> Result<Vec<AutomationHistoryEntry>, StoreError> {
            Err(StoreError::Internal("unexpected test call".into()))
        }

        async fn search(
            &self,
            _project_id: Option<&ProjectId>,
            _query: &str,
            _offset: usize,
            _limit: usize,
        ) -> Result<Vec<WorkspaceSearchHit>, StoreError> {
            let _ = WorkspaceSearchKind::Artifact;
            Err(StoreError::Internal("unexpected test call".into()))
        }
    }

    #[derive(Debug, Default)]
    struct TestArtifactState {
        artifact: Option<Artifact>,
        version: Option<ArtifactVersion>,
        retention: Vec<ArtifactRetentionRecord>,
        import: Option<(MutationCommand, ArtifactImportPlan)>,
        open: Option<(MutationCommand, ArtifactOpenPlan)>,
        removal: Option<(MutationCommand, ArtifactRemovalPlan)>,
        fail_import_persistence: bool,
        fail_removal_mark_persistence: bool,
        fail_removal_commit_persistence: bool,
        pending_removal_list_limits: Vec<usize>,
    }

    #[derive(Debug, Default)]
    struct TestArtifactStore(Mutex<TestArtifactState>);

    impl TestArtifactStore {
        fn with_available(artifact: Artifact, version: ArtifactVersion) -> Self {
            Self::with_available_versions(artifact, vec![version])
        }

        fn with_available_versions(artifact: Artifact, versions: Vec<ArtifactVersion>) -> Self {
            let version = versions.last().cloned().expect("current artifact version");
            let retention = versions
                .into_iter()
                .map(|version| {
                    ArtifactRetentionRecord::retained(version).expect("retained artifact version")
                })
                .collect();
            Self(Mutex::new(TestArtifactState {
                artifact: Some(artifact),
                version: Some(version),
                retention,
                ..TestArtifactState::default()
            }))
        }

        fn import_plan(&self) -> ArtifactImportPlan {
            self.0
                .lock()
                .expect("state")
                .import
                .as_ref()
                .expect("import")
                .1
                .clone()
        }

        fn removal_plan(&self) -> ArtifactRemovalPlan {
            self.0
                .lock()
                .expect("state")
                .removal
                .as_ref()
                .expect("removal")
                .1
                .clone()
        }

        fn retention_record(&self) -> ArtifactRetentionRecord {
            self.0
                .lock()
                .expect("state")
                .retention
                .first()
                .expect("retention")
                .clone()
        }

        fn retention_records(&self) -> Vec<ArtifactRetentionRecord> {
            self.0.lock().expect("state").retention.clone()
        }

        fn fail_next_removal_mark(&self) {
            self.0.lock().expect("state").fail_removal_mark_persistence = true;
        }

        fn fail_next_removal_commit(&self) {
            self.0
                .lock()
                .expect("state")
                .fail_removal_commit_persistence = true;
        }

        fn pending_removal_list_limits(&self) -> Vec<usize> {
            self.0
                .lock()
                .expect("state")
                .pending_removal_list_limits
                .clone()
        }

        fn fail_next_import_persistence(&self) {
            self.0.lock().expect("state").fail_import_persistence = true;
        }

        fn tombstone_artifact(&self) {
            let mut state = self.0.lock().expect("state");
            let artifact = state.artifact.as_mut().expect("artifact");
            artifact.state = ArtifactState::Deleted;
            artifact.content = None;
            artifact.revision = artifact.revision.saturating_add(1);
            artifact.updated_at = artifact.updated_at.saturating_add(1);
            Artifact::restore(artifact.clone()).expect("valid tombstone");
        }
    }

    #[async_trait]
    impl ArtifactStore for TestArtifactStore {
        async fn resolve_import(
            &self,
            command: &MutationCommand,
        ) -> Result<Option<ArtifactImportPlan>, StoreError> {
            let state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            let Some((existing, plan)) = &state.import else {
                return Ok(None);
            };
            if existing.scope != command.scope || existing.key != command.key {
                return Ok(None);
            }
            if existing.fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            Ok(Some(plan.clone()))
        }

        async fn reserve_import(
            &self,
            artifact: Artifact,
            command: &MutationCommand,
        ) -> Result<ArtifactImportReservation, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            if let Some((existing_command, plan)) = &state.import {
                if existing_command.fingerprint != command.fingerprint {
                    return Err(StoreError::Conflict);
                }
                return Ok(ArtifactImportReservation::ExactReplay(plan.clone()));
            }
            let plan = ArtifactImportPlan::prepared(artifact.clone())
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            state.artifact = Some(artifact);
            state.import = Some((command.clone(), plan.clone()));
            Ok(ArtifactImportReservation::NewlyPrepared(plan))
        }

        async fn mark_content_ready(
            &self,
            artifact_id: &ArtifactId,
            expected_revision: u64,
            content: ArtifactVersion,
            now: UnixMillis,
        ) -> Result<ArtifactContentReadyResult, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            let plan = &mut state.import.as_mut().ok_or(StoreError::NotFound)?.1;
            if &plan.artifact.id != artifact_id || plan.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            plan.record_content_ready(content, now)
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            Ok(ArtifactContentReadyResult::ContentReady(plan.clone()))
        }

        async fn commit_import(
            &self,
            artifact: Artifact,
            expected_artifact_revision: u64,
            expected_import_revision: u64,
            content: ArtifactVersion,
            now: UnixMillis,
        ) -> Result<ArtifactImportPlan, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            if state
                .artifact
                .as_ref()
                .is_none_or(|current| current.revision != expected_artifact_revision)
            {
                return Err(StoreError::Conflict);
            }
            let plan = &mut state.import.as_mut().ok_or(StoreError::NotFound)?.1;
            if plan.revision != expected_import_revision || plan.content.as_ref() != Some(&content)
            {
                return Err(StoreError::Conflict);
            }
            plan.commit(artifact.clone(), now)
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            let result = plan.clone();
            state.artifact = Some(artifact);
            state.retention = vec![
                ArtifactRetentionRecord::retained(content.clone())
                    .map_err(|error| StoreError::Internal(error.to_string()))?,
            ];
            state.version = Some(content);
            Ok(result)
        }

        async fn fail_import(
            &self,
            artifact_id: &ArtifactId,
            expected_revision: u64,
            failure: ArtifactImportFailureCode,
            now: UnixMillis,
        ) -> Result<ArtifactImportPlan, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            if state.fail_import_persistence {
                state.fail_import_persistence = false;
                return Err(StoreError::Unavailable(
                    "injected artifact failure persistence outage".into(),
                ));
            }
            let plan = &mut state.import.as_mut().ok_or(StoreError::NotFound)?.1;
            if &plan.artifact.id != artifact_id || plan.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            plan.fail(failure, now)
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            Ok(plan.clone())
        }

        async fn list_incomplete_imports(
            &self,
            limit: usize,
        ) -> Result<Vec<ArtifactImportPlan>, StoreError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?
                .import
                .as_ref()
                .map(|(_, plan)| plan)
                .filter(|plan| {
                    matches!(
                        plan.state,
                        ArtifactImportState::Prepared | ArtifactImportState::ContentReady
                    )
                })
                .cloned()
                .into_iter()
                .take(limit)
                .collect())
        }

        async fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact, StoreError> {
            self.0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?
                .artifact
                .as_ref()
                .filter(|artifact| &artifact.id == id)
                .cloned()
                .ok_or(StoreError::NotFound)
        }

        async fn list_artifacts(
            &self,
            project_id: &ProjectId,
            _after: Option<&ArtifactId>,
            limit: usize,
        ) -> Result<Vec<Artifact>, StoreError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?
                .artifact
                .as_ref()
                .filter(|artifact| &artifact.project_id == project_id)
                .cloned()
                .into_iter()
                .take(limit)
                .collect())
        }

        async fn get_artifact_version(
            &self,
            artifact_id: &ArtifactId,
            version: u32,
        ) -> Result<ArtifactVersion, StoreError> {
            self.0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?
                .version
                .as_ref()
                .filter(|item| &item.artifact_id == artifact_id && item.version == version)
                .cloned()
                .ok_or(StoreError::NotFound)
        }

        async fn quota_usage(
            &self,
            _project_id: &ProjectId,
        ) -> Result<ArtifactQuotaUsage, StoreError> {
            let project_artifact_count = if self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?
                .import
                .is_some()
            {
                MAX_PROJECT_ARTIFACT_COUNT
            } else {
                0
            };
            Ok(ArtifactQuotaUsage {
                project_artifact_count,
                ..ArtifactQuotaUsage::default()
            })
        }

        async fn resolve_open(
            &self,
            command: &MutationCommand,
        ) -> Result<Option<ArtifactOpenPlan>, StoreError> {
            let state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            let Some((existing, plan)) = &state.open else {
                return Ok(None);
            };
            if existing.scope != command.scope || existing.key != command.key {
                return Ok(None);
            }
            if existing.fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            Ok(Some(plan.clone()))
        }

        async fn prepare_open(
            &self,
            content: ArtifactVersion,
            command: &MutationCommand,
            now: UnixMillis,
        ) -> Result<ArtifactOpenReservation, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            if let Some((existing_command, plan)) = &state.open {
                if existing_command.fingerprint != command.fingerprint {
                    return Err(StoreError::Conflict);
                }
                return Ok(ArtifactOpenReservation::ExactReplay(plan.clone()));
            }
            let plan = ArtifactOpenPlan::prepared(content, now)
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            state.open = Some((command.clone(), plan.clone()));
            Ok(ArtifactOpenReservation::NewlyPrepared(plan))
        }

        async fn mark_open_dispatching(
            &self,
            artifact_id: &ArtifactId,
            content_version: u32,
            expected_revision: u64,
            now: UnixMillis,
        ) -> Result<ArtifactOpenPlan, StoreError> {
            self.mutate_open(artifact_id, content_version, expected_revision, |plan| {
                plan.begin_dispatch(now)
            })
        }

        async fn complete_open(
            &self,
            artifact_id: &ArtifactId,
            content_version: u32,
            expected_revision: u64,
            now: UnixMillis,
        ) -> Result<ArtifactOpenPlan, StoreError> {
            self.mutate_open(artifact_id, content_version, expected_revision, |plan| {
                plan.complete(now)
            })
        }

        async fn fail_open(
            &self,
            artifact_id: &ArtifactId,
            content_version: u32,
            expected_revision: u64,
            failure: ArtifactOpenFailureCode,
            now: UnixMillis,
        ) -> Result<ArtifactOpenPlan, StoreError> {
            self.mutate_open(artifact_id, content_version, expected_revision, |plan| {
                plan.fail(failure, now)
            })
        }

        async fn interrupt_open(
            &self,
            artifact_id: &ArtifactId,
            content_version: u32,
            expected_revision: u64,
            now: UnixMillis,
        ) -> Result<ArtifactOpenPlan, StoreError> {
            self.mutate_open(artifact_id, content_version, expected_revision, |plan| {
                plan.interrupt(now)
            })
        }

        async fn list_incomplete_opens(
            &self,
            limit: usize,
        ) -> Result<Vec<ArtifactOpenPlan>, StoreError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?
                .open
                .as_ref()
                .map(|(_, plan)| plan)
                .filter(|plan| {
                    matches!(
                        plan.state,
                        ArtifactOpenState::Prepared | ArtifactOpenState::Dispatching
                    )
                })
                .cloned()
                .into_iter()
                .take(limit)
                .collect())
        }

        async fn resolve_removal(
            &self,
            command: &MutationCommand,
        ) -> Result<Option<ArtifactRemovalPlan>, StoreError> {
            let state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            let Some((existing, plan)) = &state.removal else {
                return Ok(None);
            };
            if existing.scope != command.scope || existing.key != command.key {
                return Ok(None);
            }
            if existing.fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            Ok(Some(plan.clone()))
        }

        async fn reserve_removal(
            &self,
            artifact_id: &ArtifactId,
            expected_revision: u64,
            expected_content_version: u32,
            command: &MutationCommand,
            now: UnixMillis,
        ) -> Result<ArtifactRemovalReservation, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            if let Some((existing, plan)) = &state.removal {
                if existing.scope == command.scope
                    && existing.key == command.key
                    && existing.fingerprint == command.fingerprint
                {
                    return Ok(ArtifactRemovalReservation::ExactReplay(plan.clone()));
                }
                return Err(StoreError::Conflict);
            }
            if state.open.as_ref().is_some_and(|(_, plan)| {
                matches!(
                    plan.state,
                    ArtifactOpenState::Prepared | ArtifactOpenState::Dispatching
                )
            }) {
                return Err(StoreError::Conflict);
            }
            let mut artifact = state.artifact.clone().ok_or(StoreError::NotFound)?;
            if &artifact.id != artifact_id
                || artifact.revision != expected_revision
                || artifact
                    .content
                    .as_ref()
                    .is_none_or(|content| content.content_version != expected_content_version)
            {
                return Err(StoreError::Conflict);
            }
            let mut retention = state.retention.clone();
            if retention.is_empty()
                || retention.iter().any(|record| {
                    record.content.artifact_id != *artifact_id
                        || record.state != ArtifactRetentionState::Retained
                })
                || !retention
                    .iter()
                    .any(|record| record.content.version == expected_content_version)
            {
                return Err(StoreError::Conflict);
            }
            artifact.remove(now).map_err(|_| StoreError::Conflict)?;
            for record in &mut retention {
                record.begin_purge(now).map_err(|_| StoreError::Conflict)?;
            }
            let plan = ArtifactRemovalPlan::pending(artifact.clone())
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            state.artifact = Some(artifact);
            state.retention = retention;
            state.removal = Some((command.clone(), plan.clone()));
            Ok(ArtifactRemovalReservation::NewlyPending(plan))
        }

        async fn list_pending_removal_versions(
            &self,
            artifact_id: &ArtifactId,
            limit: usize,
        ) -> Result<Vec<ArtifactRetentionRecord>, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            state.pending_removal_list_limits.push(limit);
            let mut records = state
                .retention
                .iter()
                .filter(|record| {
                    record.content.artifact_id == *artifact_id
                        && record.state == ArtifactRetentionState::PurgePending
                })
                .cloned()
                .collect::<Vec<_>>();
            records.sort_by_key(|record| record.content.version);
            records.truncate(limit);
            Ok(records)
        }

        async fn mark_content_purged(
            &self,
            artifact_id: &ArtifactId,
            content_version: u32,
            expected_revision: u64,
            now: UnixMillis,
        ) -> Result<ArtifactRetentionRecord, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            if state.fail_removal_mark_persistence {
                state.fail_removal_mark_persistence = false;
                return Err(StoreError::Unavailable(
                    "injected artifact removal mark outage".into(),
                ));
            }
            let retention = state
                .retention
                .iter_mut()
                .find(|record| record.content.version == content_version)
                .ok_or(StoreError::NotFound)?;
            if retention.content.artifact_id != *artifact_id
                || retention.content.version != content_version
                || retention.revision != expected_revision
            {
                return Err(StoreError::Conflict);
            }
            retention
                .record_purged(now)
                .map_err(|_| StoreError::Conflict)?;
            Ok(retention.clone())
        }

        async fn commit_removal(
            &self,
            artifact_id: &ArtifactId,
            expected_revision: u64,
            now: UnixMillis,
        ) -> Result<ArtifactRemovalPlan, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            if state.fail_removal_commit_persistence {
                state.fail_removal_commit_persistence = false;
                return Err(StoreError::Unavailable(
                    "injected artifact removal commit outage".into(),
                ));
            }
            if state.retention.is_empty()
                || state.retention.iter().any(|record| {
                    record.content.artifact_id != *artifact_id
                        || record.state != ArtifactRetentionState::Purged
                })
            {
                return Err(StoreError::Conflict);
            }
            let plan = &mut state.removal.as_mut().ok_or(StoreError::NotFound)?.1;
            if plan.artifact.id != *artifact_id || plan.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            plan.commit(now).map_err(|_| StoreError::Conflict)?;
            Ok(plan.clone())
        }

        async fn list_incomplete_removals(
            &self,
            limit: usize,
        ) -> Result<Vec<ArtifactRemovalPlan>, StoreError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?
                .removal
                .as_ref()
                .map(|(_, plan)| plan)
                .filter(|plan| plan.state == ArtifactRemovalState::Pending)
                .cloned()
                .into_iter()
                .take(limit)
                .collect())
        }
    }

    impl TestArtifactStore {
        fn mutate_open(
            &self,
            artifact_id: &ArtifactId,
            content_version: u32,
            expected_revision: u64,
            mutation: impl FnOnce(&mut ArtifactOpenPlan) -> Result<(), ApplicationError>,
        ) -> Result<ArtifactOpenPlan, StoreError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| StoreError::Internal("lock".into()))?;
            let plan = &mut state.open.as_mut().ok_or(StoreError::NotFound)?.1;
            if &plan.content.artifact_id != artifact_id
                || plan.content.version != content_version
                || plan.revision != expected_revision
            {
                return Err(StoreError::Conflict);
            }
            mutation(plan).map_err(|error| StoreError::Internal(error.to_string()))?;
            Ok(plan.clone())
        }
    }

    #[derive(Debug, Default)]
    struct HangingContent {
        prepare_calls: AtomicUsize,
        reserved_discards: AtomicUsize,
    }

    #[async_trait]
    impl ArtifactContentStore for HangingContent {
        async fn prepare_import_content(
            &self,
            _source: &SelectedSourcePath,
            _artifact_id: &ArtifactId,
            _content_version: u32,
            _media_type: &str,
            _max_bytes: u64,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode> {
            self.prepare_calls.fetch_add(1, Ordering::SeqCst);
            pending().await
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
            Ok(ArtifactContentStatus::Missing)
        }

        async fn discard_prepared_content(
            &self,
            _content: &ArtifactVersion,
        ) -> Result<(), ArtifactImportFailureCode> {
            Ok(())
        }

        async fn discard_reserved_content(
            &self,
            _artifact_id: &ArtifactId,
            _content_version: u32,
        ) -> Result<(), ArtifactImportFailureCode> {
            self.reserved_discards.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TransientContent {
        failure: ArtifactImportFailureCode,
    }

    #[async_trait]
    impl ArtifactContentStore for TransientContent {
        async fn prepare_import_content(
            &self,
            _source: &SelectedSourcePath,
            _artifact_id: &ArtifactId,
            _content_version: u32,
            _media_type: &str,
            _max_bytes: u64,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode> {
            Err(self.failure)
        }

        async fn publish_content(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<ArtifactContentPublication, ArtifactImportFailureCode> {
            Err(self.failure)
        }

        async fn content_status(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<ArtifactContentStatus, ArtifactImportFailureCode> {
            Err(self.failure)
        }

        async fn discard_prepared_content(
            &self,
            _content: &ArtifactVersion,
        ) -> Result<(), ArtifactImportFailureCode> {
            Err(self.failure)
        }

        async fn discard_reserved_content(
            &self,
            _artifact_id: &ArtifactId,
            _content_version: u32,
        ) -> Result<(), ArtifactImportFailureCode> {
            Err(self.failure)
        }
    }

    #[derive(Debug, Default)]
    struct CorruptPublishedContent {
        discards: AtomicUsize,
        hang_status: bool,
    }

    #[async_trait]
    impl ArtifactContentStore for CorruptPublishedContent {
        async fn prepare_import_content(
            &self,
            _source: &SelectedSourcePath,
            _artifact_id: &ArtifactId,
            _content_version: u32,
            _media_type: &str,
            _max_bytes: u64,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode> {
            Err(ArtifactImportFailureCode::IntegrityFailure)
        }

        async fn publish_content(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<ArtifactContentPublication, ArtifactImportFailureCode> {
            Err(ArtifactImportFailureCode::IntegrityFailure)
        }

        async fn content_status(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<ArtifactContentStatus, ArtifactImportFailureCode> {
            if self.hang_status {
                return pending().await;
            }
            Err(ArtifactImportFailureCode::IntegrityFailure)
        }

        async fn discard_prepared_content(
            &self,
            _content: &ArtifactVersion,
        ) -> Result<(), ArtifactImportFailureCode> {
            self.discards.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn discard_reserved_content(
            &self,
            _artifact_id: &ArtifactId,
            _content_version: u32,
        ) -> Result<(), ArtifactImportFailureCode> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TestContentRetention {
        calls: AtomicUsize,
        failure: Option<ArtifactRetentionFailureCode>,
        outcome: ArtifactContentPurge,
    }

    impl Default for TestContentRetention {
        fn default() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                failure: None,
                outcome: ArtifactContentPurge::Purged,
            }
        }
    }

    #[async_trait]
    impl ArtifactContentRetention for TestContentRetention {
        async fn purge_content(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<ArtifactContentPurge, ArtifactRetentionFailureCode> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.failure.map_or(Ok(self.outcome), Err)
        }
    }

    #[derive(Debug, Default)]
    struct TimeoutOnceContentRetention {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ArtifactContentRetention for TimeoutOnceContentRetention {
        async fn purge_content(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<ArtifactContentPurge, ArtifactRetentionFailureCode> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                pending().await
            } else {
                Ok(ArtifactContentPurge::Purged)
            }
        }
    }

    #[derive(Debug, Default)]
    struct HangingOpener {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ArtifactOpener for HangingOpener {
        async fn open_artifact(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<(), ArtifactOpenError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            pending().await
        }
    }

    #[derive(Debug, Default)]
    struct UnknownOpener;

    #[async_trait]
    impl ArtifactOpener for UnknownOpener {
        async fn open_artifact(
            &self,
            _content: &ArtifactVersion,
            _deadline_unix_ms: UnixMillis,
        ) -> Result<(), ArtifactOpenError> {
            Err(ArtifactOpenError::OutcomeUnknown)
        }
    }

    fn project() -> Project {
        Project::new(
            ProjectId::new("project-test").expect("project ID"),
            "Artifacts".into(),
            String::new(),
            100,
        )
        .expect("project")
    }

    fn available_artifact() -> (Artifact, ArtifactVersion) {
        let mut artifact = Artifact::new_unavailable(
            ArtifactId::new("artifact-test").expect("artifact ID"),
            project().id,
            None,
            "report.txt".into(),
            100,
        )
        .expect("artifact");
        let version =
            ArtifactVersion::new(artifact.id.clone(), 1, [9; 32], "text/plain".into(), 5, 100)
                .expect("version");
        artifact
            .record_content(version.summary(), 100)
            .expect("available");
        (artifact, version)
    }

    fn available_artifact_versions(count: u32) -> (Artifact, Vec<ArtifactVersion>) {
        assert!((1..=MAX_ARTIFACT_CONTENT_VERSION).contains(&count));
        let mut artifact = Artifact::new_unavailable(
            ArtifactId::new("artifact-test").expect("artifact ID"),
            project().id,
            None,
            "report.txt".into(),
            100,
        )
        .expect("artifact");
        let mut versions = Vec::with_capacity(usize::try_from(count).expect("version count"));
        for version in 1..=count {
            let content = ArtifactVersion::new(
                artifact.id.clone(),
                version,
                [u8::try_from(version % 251).expect("digest byte"); 32],
                "text/plain".into(),
                u64::from(version),
                100,
            )
            .expect("version");
            artifact
                .record_content(content.summary(), 100)
                .expect("available version");
            versions.push(content);
        }
        (artifact, versions)
    }

    #[test]
    fn selected_source_path_never_debugs_or_errors_with_path_text() {
        let selected =
            SelectedSourcePath::new(PathBuf::from("/private/secret.txt")).expect("absolute source");
        assert_eq!(format!("{selected:?}"), "SelectedSourcePath([REDACTED])");
        let error = SelectedSourcePath::new(PathBuf::from("relative/secret.txt"))
            .expect_err("relative rejected");
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn plans_enforce_exact_state_shapes() {
        let (artifact, version) = available_artifact();
        let artifact_for_removal = artifact.clone();
        let unavailable = Artifact::new_unavailable(
            artifact.id.clone(),
            artifact.project_id.clone(),
            None,
            artifact.name.clone(),
            100,
        )
        .expect("unavailable");
        let mut import = ArtifactImportPlan::prepared(unavailable.clone()).expect("prepared");
        import
            .record_content_ready(version.clone(), 101)
            .expect("ready");
        let ready = import.clone();
        let mut forged = artifact.clone();
        forged.project_id = ProjectId::new("other-project").expect("project ID");
        assert!(ready.clone().commit(forged, 102).is_err());
        let mut forged = artifact.clone();
        forged.thread_id = Some(ThreadId::new("other-thread").expect("thread ID"));
        assert!(ready.clone().commit(forged, 102).is_err());
        let mut forged = artifact.clone();
        forged.name = "other.txt".into();
        assert!(ready.clone().commit(forged, 102).is_err());
        let mut forged = artifact.clone();
        forged.created_at = forged.created_at.saturating_sub(1);
        assert!(ready.clone().commit(forged, 102).is_err());
        import.commit(artifact, 102).expect("committed");
        ArtifactImportPlan::restore(import).expect("restored import");

        let mut open = ArtifactOpenPlan::prepared(version, 100).expect("prepared open");
        open.begin_dispatch(101).expect("dispatch");
        open.interrupt(102).expect("interrupted");
        ArtifactOpenPlan::restore(open).expect("restored open");

        let mut retention = ArtifactRetentionRecord::retained(
            ArtifactVersion::new(
                artifact_for_removal.id.clone(),
                1,
                [9; 32],
                "text/plain".into(),
                5,
                100,
            )
            .expect("retained version"),
        )
        .expect("retained");
        retention.begin_purge(101).expect("purge pending");
        assert!(retention.clone().record_purged(100).is_err());
        retention.record_purged(102).expect("purged");
        ArtifactRetentionRecord::restore(retention).expect("restored retention");

        let mut tombstone = artifact_for_removal;
        tombstone.remove(101).expect("tombstone");
        let mut removal = ArtifactRemovalPlan::pending(tombstone).expect("pending removal");
        assert!(removal.clone().commit(100).is_err());
        removal.commit(102).expect("committed removal");
        ArtifactRemovalPlan::restore(removal).expect("restored removal");
    }

    #[tokio::test]
    async fn removal_tombstones_purges_and_exact_replay_never_repeats_io() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        let retention = Arc::new(TestContentRetention::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_content_retention(retention.clone());
        let request = || RemoveArtifact {
            artifact_id: "artifact-test".into(),
            expected_revision: 1,
            expected_content_version: 1,
        };

        let removed = service
            .remove_artifact(request(), "remove-command")
            .await
            .expect("removed");
        assert_eq!(removed.state, ArtifactState::Deleted);
        assert!(removed.content.is_none());
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Committed
        );
        assert_eq!(
            artifacts.retention_record().state,
            ArtifactRetentionState::Purged
        );
        assert_eq!(retention.calls.load(Ordering::SeqCst), 1);

        let replay_without_platform = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(101)),
            Arc::new(FixedIds),
        );
        assert_eq!(
            replay_without_platform
                .remove_artifact(request(), "remove-command")
                .await
                .expect("terminal replay"),
            removed
        );
        assert_eq!(retention.calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            service
                .remove_artifact(
                    RemoveArtifact {
                        expected_revision: 2,
                        ..request()
                    },
                    "remove-command",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
    }

    #[tokio::test]
    async fn unavailable_retention_never_reserves_a_removal() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        );
        assert!(matches!(
            service
                .remove_artifact(
                    RemoveArtifact {
                        artifact_id: "artifact-test".into(),
                        expected_revision: 1,
                        expected_content_version: 1,
                    },
                    "remove-unavailable",
                )
                .await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(
            artifacts
                .get_artifact(&ArtifactId::new("artifact-test").expect("id"))
                .await
                .expect("artifact")
                .state,
            ArtifactState::Available
        );
    }

    #[tokio::test]
    async fn removal_failure_retains_pending_ownership_for_bounded_recovery() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        let failing = Arc::new(TestContentRetention {
            calls: AtomicUsize::new(0),
            failure: Some(ArtifactRetentionFailureCode::ContentStoreUnavailable),
            outcome: ArtifactContentPurge::Purged,
        });
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_content_retention(failing.clone());
        let request = RemoveArtifact {
            artifact_id: "artifact-test".into(),
            expected_revision: 1,
            expected_content_version: 1,
        };
        assert!(matches!(
            service.remove_artifact(request, "remove-recover").await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(failing.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Pending
        );
        assert_eq!(
            artifacts.retention_record().state,
            ArtifactRetentionState::PurgePending
        );

        let recovered_retention = Arc::new(TestContentRetention {
            outcome: ArtifactContentPurge::AlreadyAbsent,
            ..TestContentRetention::default()
        });
        let recovery = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(101)),
            Arc::new(FixedIds),
        )
        .with_content_retention(recovered_retention.clone());
        assert_eq!(
            recovery
                .recover_incomplete_removals(1)
                .await
                .expect("recovery"),
            ArtifactRemovalRecoverySummary {
                committed: 1,
                truncated: false,
            }
        );
        assert_eq!(recovered_retention.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Committed
        );
        assert_eq!(
            recovery
                .recover_incomplete_removals(1)
                .await
                .expect("empty"),
            ArtifactRemovalRecoverySummary::default()
        );
    }

    #[tokio::test]
    async fn removal_timeout_is_resumed_by_an_exact_same_key_retry() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        let retention = Arc::new(TimeoutOnceContentRetention::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_content_retention(retention.clone())
        .with_inner_timeouts(1, 1);
        let request = RemoveArtifact {
            artifact_id: "artifact-test".into(),
            expected_revision: 1,
            expected_content_version: 1,
        };
        assert!(matches!(
            service
                .remove_artifact(request.clone(), "remove-timeout")
                .await,
            Err(ApplicationError::DeadlineExceeded)
        ));
        assert_eq!(retention.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Pending
        );
        assert_eq!(
            artifacts.retention_record().state,
            ArtifactRetentionState::PurgePending
        );
        let read_only_resolver = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        );
        assert!(matches!(
            read_only_resolver
                .resolve_removal(&request, "remove-timeout")
                .await
                .expect("pending resolution"),
            ArtifactRemovalResolution::Pending { artifact }
                if artifact.state == ArtifactState::Deleted
        ));
        assert_eq!(
            read_only_resolver
                .resolve_removal(&request, "different-key")
                .await
                .expect("unknown resolution"),
            ArtifactRemovalResolution::Unknown
        );
        assert!(matches!(
            read_only_resolver
                .resolve_removal(
                    &RemoveArtifact {
                        expected_revision: 2,
                        ..request.clone()
                    },
                    "remove-timeout",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));

        let removed = service
            .remove_artifact(request.clone(), "remove-timeout")
            .await
            .expect("live exact retry");
        assert_eq!(removed.state, ArtifactState::Deleted);
        assert_eq!(retention.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Committed
        );
        assert!(matches!(
            read_only_resolver
                .resolve_removal(&request, "remove-timeout")
                .await
                .expect("committed resolution"),
            ArtifactRemovalResolution::Committed { artifact } if artifact == removed
        ));
    }

    #[tokio::test]
    async fn removal_mark_failure_is_resumed_by_an_exact_same_key_retry() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        artifacts.fail_next_removal_mark();
        let retention = Arc::new(TestContentRetention::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_content_retention(retention.clone());
        let request = RemoveArtifact {
            artifact_id: "artifact-test".into(),
            expected_revision: 1,
            expected_content_version: 1,
        };

        assert!(matches!(
            service
                .remove_artifact(request.clone(), "remove-mark-retry")
                .await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(retention.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            artifacts.retention_record().state,
            ArtifactRetentionState::PurgePending
        );

        service
            .remove_artifact(request, "remove-mark-retry")
            .await
            .expect("mark retry committed");
        assert_eq!(retention.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            artifacts.retention_record().state,
            ArtifactRetentionState::Purged
        );
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Committed
        );
    }

    #[tokio::test]
    async fn removal_commit_failure_retries_without_repeating_purge() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        artifacts.fail_next_removal_commit();
        let retention = Arc::new(TestContentRetention::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_content_retention(retention.clone());
        let request = RemoveArtifact {
            artifact_id: "artifact-test".into(),
            expected_revision: 1,
            expected_content_version: 1,
        };

        assert!(matches!(
            service
                .remove_artifact(request.clone(), "remove-commit-retry")
                .await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(retention.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            artifacts.retention_record().state,
            ArtifactRetentionState::Purged
        );
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Pending
        );

        service
            .remove_artifact(request, "remove-commit-retry")
            .await
            .expect("commit retry committed");
        assert_eq!(retention.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            artifacts.removal_plan().state,
            ArtifactRemovalState::Committed
        );
    }

    #[tokio::test]
    async fn removal_purges_arbitrary_version_history_in_bounded_store_chunks() {
        let version_count =
            u32::try_from(MAX_ARTIFACT_RECOVERY_BATCH * 2 + 5).expect("bounded test version count");
        let (artifact, versions) = available_artifact_versions(version_count);
        let artifacts = Arc::new(TestArtifactStore::with_available_versions(
            artifact, versions,
        ));
        let retention = Arc::new(TestContentRetention::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_content_retention(retention.clone());

        let removed = service
            .remove_artifact(
                RemoveArtifact {
                    artifact_id: "artifact-test".into(),
                    expected_revision: u64::from(version_count),
                    expected_content_version: version_count,
                },
                "remove-version-history",
            )
            .await
            .expect("all version chunks removed");
        assert_eq!(removed.revision, u64::from(version_count) + 1);
        assert_eq!(
            retention.calls.load(Ordering::SeqCst),
            usize::try_from(version_count).expect("version count")
        );
        assert!(
            artifacts
                .retention_records()
                .iter()
                .all(|record| record.state == ArtifactRetentionState::Purged)
        );
        let list_limits = artifacts.pending_removal_list_limits();
        assert_eq!(list_limits.len(), 4);
        assert!(
            list_limits
                .iter()
                .all(|limit| *limit == MAX_ARTIFACT_RECOVERY_BATCH)
        );
    }

    #[tokio::test]
    async fn import_timeout_stays_owned_and_exact_replay_never_reuses_source() {
        let artifacts = Arc::new(TestArtifactStore::default());
        let content = Arc::new(HangingContent::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            content.clone(),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_inner_timeouts(1, 1);
        let request = |path: &str| ImportArtifact {
            project_id: "project-test".into(),
            thread_id: None,
            display_name: "report.txt".into(),
            media_type: "text/plain".into(),
            source: SelectedSourcePath::new(PathBuf::from(path)).expect("source"),
        };

        assert_eq!(
            service
                .import_artifact(request("/tmp/report.txt"), "import-command")
                .await
                .expect_err("timeout")
                .to_string(),
            ApplicationError::DeadlineExceeded.to_string()
        );
        assert_eq!(artifacts.import_plan().state, ArtifactImportState::Prepared);
        assert_eq!(artifacts.import_plan().failure, None);
        let mut archived = project();
        archived.archive(101).expect("archive project");
        let replay_service = ArtifactService::new(
            artifacts,
            content.clone(),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: archived }),
            Arc::new(FixedClock(101)),
            Arc::new(FixedIds),
        )
        .with_inner_timeouts(1, 1);
        assert!(matches!(
            replay_service
                .import_artifact(request("/different/selection.txt"), "import-command")
                .await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(content.prepare_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn timeout_after_open_dispatch_requires_review_and_never_reopens() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        let opener = Arc::new(HangingOpener::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            opener.clone(),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_inner_timeouts(1, 1);
        let request = || OpenArtifact {
            artifact_id: "artifact-test".into(),
            content_version: 1,
        };

        let first = service
            .open_artifact(request(), "open-command")
            .await
            .expect("review receipt");
        assert_eq!(
            first.status,
            ArtifactOpenReceiptStatus::InterruptedNeedsReview
        );
        artifacts.tombstone_artifact();
        let replay = service
            .open_artifact(request(), "open-command")
            .await
            .expect("same review receipt");
        assert_eq!(replay, first);
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_open_response_is_review_required_not_a_stable_failure() {
        let (artifact, version) = available_artifact();
        let artifacts = Arc::new(TestArtifactStore::with_available(artifact, version));
        let service = ArtifactService::new(
            artifacts,
            Arc::new(HangingContent::default()),
            Arc::new(UnknownOpener),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        );
        let receipt = service
            .open_artifact(
                OpenArtifact {
                    artifact_id: "artifact-test".into(),
                    content_version: 1,
                },
                "unknown-open",
            )
            .await
            .expect("review receipt");
        assert_eq!(
            receipt.status,
            ArtifactOpenReceiptStatus::InterruptedNeedsReview
        );
        assert_eq!(receipt.failure, None);
    }

    #[tokio::test]
    async fn recovery_propagates_failure_journal_outage_and_leaves_content_ready() {
        let artifacts = Arc::new(TestArtifactStore::default());
        let reserved = Artifact::new_unavailable(
            ArtifactId::new("artifact-failure-outage").expect("artifact ID"),
            project().id,
            None,
            "outage.txt".into(),
            100,
        )
        .expect("reservation");
        let command = MutationCommand {
            scope: ARTIFACT_IMPORT_SCOPE.into(),
            key: "failure-outage-command".into(),
            fingerprint: [5; 32],
        };
        let prepared = match artifacts
            .reserve_import(reserved, &command)
            .await
            .expect("reserve")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let version = ArtifactVersion::new(
            prepared.artifact.id.clone(),
            1,
            [6; 32],
            "text/plain".into(),
            2,
            100,
        )
        .expect("version");
        artifacts
            .mark_content_ready(&prepared.artifact.id, 0, version, 100)
            .await
            .expect("content ready");
        artifacts.fail_next_import_persistence();
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(HangingContent::default()),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        );

        assert!(matches!(
            service.recover_incomplete_imports(1).await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(
            artifacts.import_plan().state,
            ArtifactImportState::ContentReady
        );
    }

    #[tokio::test]
    async fn transient_content_ready_failures_stay_pending_for_safe_recovery() {
        for failure in [
            ArtifactImportFailureCode::ContentStoreUnavailable,
            ArtifactImportFailureCode::DeadlineExceeded,
        ] {
            let artifacts = Arc::new(TestArtifactStore::default());
            let reserved = Artifact::new_unavailable(
                ArtifactId::new(format!("transient-{failure}")).expect("artifact ID"),
                project().id,
                None,
                "transient.txt".into(),
                100,
            )
            .expect("reservation");
            let command = MutationCommand {
                scope: ARTIFACT_IMPORT_SCOPE.into(),
                key: format!("transient-{failure}"),
                fingerprint: [8; 32],
            };
            let prepared = match artifacts
                .reserve_import(reserved, &command)
                .await
                .expect("reserve")
            {
                ArtifactImportReservation::NewlyPrepared(plan) => plan,
                ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
            };
            let version = ArtifactVersion::new(
                prepared.artifact.id.clone(),
                1,
                [8; 32],
                "text/plain".into(),
                2,
                100,
            )
            .expect("version");
            let ready = artifacts
                .mark_content_ready(&prepared.artifact.id, 0, version, 100)
                .await
                .expect("content ready");
            let ArtifactContentReadyResult::ContentReady(ready) = ready else {
                panic!("unexpected quota failure");
            };
            let service = ArtifactService::new(
                artifacts.clone(),
                Arc::new(TransientContent { failure }),
                Arc::new(HangingOpener::default()),
                Arc::new(TestWorkspace { project: project() }),
                Arc::new(FixedClock(100)),
                Arc::new(FixedIds),
            );

            assert!(service.publish_and_commit(ready).await.is_err());
            assert_eq!(
                artifacts.import_plan().state,
                ArtifactImportState::ContentReady
            );
            assert!(service.recover_incomplete_imports(1).await.is_err());
            assert_eq!(
                artifacts.import_plan().state,
                ArtifactImportState::ContentReady
            );
        }
    }

    #[tokio::test]
    async fn corrupt_published_bytes_keep_content_ready_ownership_live_and_on_recovery() {
        let artifacts = Arc::new(TestArtifactStore::default());
        let reserved = Artifact::new_unavailable(
            ArtifactId::new("corrupt-published").expect("artifact ID"),
            project().id,
            None,
            "corrupt.txt".into(),
            100,
        )
        .expect("reservation");
        let command = MutationCommand {
            scope: ARTIFACT_IMPORT_SCOPE.into(),
            key: "corrupt-published".into(),
            fingerprint: [19; 32],
        };
        let prepared = match artifacts
            .reserve_import(reserved, &command)
            .await
            .expect("reserve")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let version = ArtifactVersion::new(
            prepared.artifact.id.clone(),
            1,
            [20; 32],
            "text/plain".into(),
            2,
            100,
        )
        .expect("version");
        let ready = artifacts
            .mark_content_ready(&prepared.artifact.id, 0, version, 100)
            .await
            .expect("content ready");
        let ArtifactContentReadyResult::ContentReady(ready) = ready else {
            panic!("unexpected quota failure");
        };
        let content = Arc::new(CorruptPublishedContent::default());
        let service = ArtifactService::new(
            artifacts.clone(),
            content.clone(),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        );

        assert!(matches!(
            service.publish_and_commit(ready).await,
            Err(ApplicationError::Integrity(_))
        ));
        assert_eq!(
            artifacts.import_plan().state,
            ArtifactImportState::ContentReady
        );
        assert!(matches!(
            service.recover_incomplete_imports(1).await,
            Err(ApplicationError::Integrity(_))
        ));
        assert_eq!(
            artifacts.import_plan().state,
            ArtifactImportState::ContentReady
        );
        assert_eq!(content.discards.load(Ordering::SeqCst), 2);

        let hanging = Arc::new(CorruptPublishedContent {
            hang_status: true,
            ..CorruptPublishedContent::default()
        });
        let bounded_recovery = ArtifactService::new(
            artifacts.clone(),
            hanging,
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        )
        .with_inner_timeouts(1, 1);
        assert!(matches!(
            bounded_recovery.recover_incomplete_imports(1).await,
            Err(ApplicationError::DeadlineExceeded)
        ));
        assert_eq!(
            artifacts.import_plan().state,
            ArtifactImportState::ContentReady
        );
    }

    #[tokio::test]
    async fn cleanup_outage_never_terminalizes_or_releases_owned_staging() {
        let artifacts = Arc::new(TestArtifactStore::default());
        let reserved = Artifact::new_unavailable(
            ArtifactId::new("cleanup-outage").expect("artifact ID"),
            project().id,
            None,
            "cleanup.txt".into(),
            100,
        )
        .expect("reservation");
        let command = MutationCommand {
            scope: ARTIFACT_IMPORT_SCOPE.into(),
            key: "cleanup-outage".into(),
            fingerprint: [11; 32],
        };
        let prepared = match artifacts
            .reserve_import(reserved, &command)
            .await
            .expect("reserve")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let service = ArtifactService::new(
            artifacts.clone(),
            Arc::new(TransientContent {
                failure: ArtifactImportFailureCode::ContentStoreUnavailable,
            }),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        );
        assert!(matches!(
            service.recover_incomplete_imports(1).await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(artifacts.import_plan().state, ArtifactImportState::Prepared);

        let version = ArtifactVersion::new(
            prepared.artifact.id.clone(),
            1,
            [12; 32],
            "text/plain".into(),
            4,
            101,
        )
        .expect("version");
        let ready = artifacts
            .mark_content_ready(&prepared.artifact.id, 0, version, 101)
            .await
            .expect("content ready");
        let ArtifactContentReadyResult::ContentReady(ready) = ready else {
            panic!("unexpected quota failure");
        };
        assert!(matches!(
            service
                .record_import_failure(ready, ArtifactImportFailureCode::IntegrityFailure)
                .await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(
            artifacts.import_plan().state,
            ArtifactImportState::ContentReady
        );
    }

    #[tokio::test]
    async fn prepared_crash_recovery_discards_deterministic_unjournaled_staging() {
        let artifacts = Arc::new(TestArtifactStore::default());
        let content = Arc::new(HangingContent::default());
        let reserved = Artifact::new_unavailable(
            ArtifactId::new("artifact-crash").expect("artifact ID"),
            project().id,
            None,
            "crash.txt".into(),
            100,
        )
        .expect("reservation");
        let command = MutationCommand {
            scope: ARTIFACT_IMPORT_SCOPE.into(),
            key: "crash-command".into(),
            fingerprint: [4; 32],
        };
        assert!(matches!(
            artifacts
                .reserve_import(reserved, &command)
                .await
                .expect("reserved"),
            ArtifactImportReservation::NewlyPrepared(_)
        ));
        let service = ArtifactService::new(
            artifacts.clone(),
            content.clone(),
            Arc::new(HangingOpener::default()),
            Arc::new(TestWorkspace { project: project() }),
            Arc::new(FixedClock(100)),
            Arc::new(FixedIds),
        );

        let summary = service
            .recover_incomplete_imports(1)
            .await
            .expect("recovery");
        assert_eq!(summary.failed, 1);
        assert_eq!(content.reserved_discards.load(Ordering::SeqCst), 1);
        assert_eq!(
            artifacts.import_plan().failure,
            Some(ArtifactImportFailureCode::InterruptedBeforeContentReady)
        );
    }

    #[test]
    fn quota_constants_match_the_contract() {
        let expected = HashMap::from([
            ("file", MAX_ARTIFACT_FILE_BYTES),
            ("project", MAX_PROJECT_ARTIFACT_BYTES),
            ("global", MAX_GLOBAL_ARTIFACT_BYTES),
            ("count", MAX_PROJECT_ARTIFACT_COUNT),
        ]);
        assert_eq!(expected["file"], 64 * 1024 * 1024);
        assert_eq!(expected["project"], 1024 * 1024 * 1024);
        assert_eq!(expected["global"], 4 * 1024 * 1024 * 1024);
        assert_eq!(expected["count"], 10_000);
    }
}
