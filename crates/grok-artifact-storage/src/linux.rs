use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::{Read, Seek as _, SeekFrom, Write as _},
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex, Weak,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use grok_application::{
    ArtifactContentPublication, ArtifactContentPurge, ArtifactContentRetention,
    ArtifactContentStatus, ArtifactContentStore, ArtifactImportFailureCode, ArtifactOpenError,
    ArtifactOpenFailureCode, ArtifactOpener, ArtifactRetentionFailureCode, MAX_ARTIFACT_FILE_BYTES,
    PreparedArtifactContent, SelectedSourcePath,
};
use grok_domain::{ArtifactId, ArtifactVersion, UnixMillis};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags, Stat};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

const ROOT_DIRECTORY: &str = "artifacts-v1";
const OBJECTS_DIRECTORY: &str = "objects";
const STAGING_DIRECTORY: &str = "staging";
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const PREPARE_COPYING: u8 = 0;
const PREPARE_CANCELLED: u8 = 1;
const PREPARE_FINALIZING: u8 = 2;
const PREPARE_COMPLETE: u8 = 3;

/// Fixed artifact-content failure classes that never include a host path.
#[derive(Debug, Error)]
pub enum ArtifactContentError {
    /// The configured private root is absent or unsafe.
    #[error("artifact content root is unavailable")]
    RootUnavailable,
    /// The selected object could not be opened as a regular file.
    #[error("selected artifact source is unavailable")]
    SourceUnavailable,
    /// Source identity or metadata changed during the bounded copy.
    #[error("selected artifact source changed")]
    SourceChanged,
    /// The selected file exceeds the supported import bound.
    #[error("selected artifact source exceeds the 64 MiB limit")]
    SourceTooLarge,
    /// The requested immutable content object does not exist.
    #[error("artifact content object was not found")]
    NotFound,
    /// Object identity, permissions, size, or digest did not validate.
    #[error("artifact content integrity validation failed")]
    Integrity,
    /// The operation's application-owned deadline elapsed.
    #[error("artifact content operation deadline elapsed")]
    DeadlineExceeded,
    /// Publication visibility could not be determined safely.
    #[error("artifact content publication outcome is uncertain")]
    PublicationUncertain,
    /// The desktop portal did not safely accept the held descriptor.
    #[error("artifact local-open portal is unavailable")]
    PortalUnavailable,
    /// A pathless filesystem operation failed.
    #[error("artifact content filesystem operation failed")]
    Io(#[source] std::io::Error),
}

impl From<std::io::Error> for ArtifactContentError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Default)]
struct StagingGateRegistry {
    gates: StdMutex<HashMap<OsString, Weak<AsyncMutex<()>>>>,
}

#[derive(Default)]
struct StagingCleanupSync {
    #[cfg(test)]
    failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    attempts: std::sync::atomic::AtomicUsize,
}

impl StagingCleanupSync {
    fn sync(&self, staging: &OwnedFd) -> Result<(), ArtifactContentError> {
        #[cfg(not(test))]
        let _ = self;
        #[cfg(test)]
        {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self
                .failures
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
            {
                return Err(ArtifactContentError::Io(std::io::Error::other(
                    "injected staging sync failure",
                )));
            }
        }
        rustix::fs::fsync(staging).map_err(io_error)?;
        Ok(())
    }
}

#[derive(Default)]
struct PublicationSync {
    #[cfg(test)]
    failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    after_sync: PreparePause,
}

#[derive(Default)]
struct PurgeSync {
    #[cfg(test)]
    failures: std::sync::atomic::AtomicUsize,
}

impl PurgeSync {
    fn sync(&self, shard: &OwnedFd, objects: &OwnedFd) -> Result<(), ArtifactContentError> {
        #[cfg(not(test))]
        let _ = self;
        #[cfg(test)]
        if self
            .failures
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(ArtifactContentError::PublicationUncertain);
        }
        rustix::fs::fsync(shard).map_err(|_| ArtifactContentError::PublicationUncertain)?;
        rustix::fs::fsync(objects).map_err(|_| ArtifactContentError::PublicationUncertain)?;
        Ok(())
    }

    fn sync_objects(&self, objects: &OwnedFd) -> Result<(), ArtifactContentError> {
        #[cfg(not(test))]
        let _ = self;
        #[cfg(test)]
        if self
            .failures
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(ArtifactContentError::PublicationUncertain);
        }
        rustix::fs::fsync(objects).map_err(|_| ArtifactContentError::PublicationUncertain)
    }
}

impl PublicationSync {
    fn sync(&self, shard: &OwnedFd, staging: &OwnedFd) -> Result<(), ArtifactContentError> {
        #[cfg(not(test))]
        let _ = self;
        #[cfg(test)]
        if self
            .failures
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(ArtifactContentError::PublicationUncertain);
        }
        rustix::fs::fsync(shard).map_err(|_| ArtifactContentError::PublicationUncertain)?;
        rustix::fs::fsync(staging).map_err(|_| ArtifactContentError::PublicationUncertain)?;
        #[cfg(test)]
        self.after_sync.wait_before_finalization();
        Ok(())
    }
}

impl StagingGateRegistry {
    fn gate(&self, artifact_id: &ArtifactId, version: u32) -> Arc<AsyncMutex<()>> {
        let key = prepared_name(artifact_id, version);
        let mut gates = self
            .gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|_, gate| gate.strong_count() != 0);
        if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(AsyncMutex::new(()));
        gates.insert(key, Arc::downgrade(&gate));
        gate
    }
}

struct PrepareRequest {
    source_path: PathBuf,
    artifact_id: ArtifactId,
    version: u32,
    media_type: String,
    max_bytes: u64,
    deadline_unix_ms: UnixMillis,
}

struct PrepareCancellationGuard {
    phase: Arc<AtomicU8>,
    armed: bool,
}

impl PrepareCancellationGuard {
    fn new(phase: Arc<AtomicU8>) -> Self {
        Self { phase, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PrepareCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.phase.compare_exchange(
                PREPARE_COPYING,
                PREPARE_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct PreparePause {
    state: StdMutex<PreparePauseState>,
    release: std::sync::Condvar,
    finalizations: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
#[derive(Default)]
struct PreparePauseState {
    armed: bool,
    reached: bool,
    released: bool,
}

#[cfg(test)]
impl PreparePause {
    fn arm(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = PreparePauseState {
            armed: true,
            reached: false,
            released: false,
        };
    }

    fn wait_before_finalization(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.armed {
            return;
        }
        state.reached = true;
        while !state.released {
            state = self
                .release
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.armed = false;
    }

    fn reached(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reached
    }

    fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.released = true;
        self.release.notify_all();
    }
}

#[cfg(test)]
#[derive(Default)]
struct CheckpointPause {
    remaining: std::sync::atomic::AtomicUsize,
    pause: PreparePause,
}

#[cfg(test)]
impl CheckpointPause {
    fn arm_after(&self, checkpoints: usize) {
        assert!(checkpoints != 0);
        self.remaining
            .store(checkpoints, std::sync::atomic::Ordering::SeqCst);
        self.pause.arm();
    }

    fn checkpoint(&self) {
        if self
            .remaining
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok_and(|previous| previous == 1)
        {
            self.pause.wait_before_finalization();
        }
    }

    fn reached(&self) -> bool {
        self.pause.reached()
    }

    fn release(&self) {
        self.pause.release();
    }
}

/// Linux-only private immutable-object adapter.
///
/// All object and staging operations are relative to retained private
/// directory descriptors. The supplied base directory is expected to be the
/// already-qualified, owner-private daemon data directory.
#[derive(Clone)]
pub struct LinuxArtifactContent {
    objects: Arc<OwnedFd>,
    staging: Arc<OwnedFd>,
    staging_gates: Arc<StagingGateRegistry>,
    staging_cleanup_sync: Arc<StagingCleanupSync>,
    publication_sync: Arc<PublicationSync>,
    purge_sync: Arc<PurgeSync>,
    portal_connection: Option<ashpd::zbus::Connection>,
    #[cfg(test)]
    prepare_pause: Arc<PreparePause>,
    #[cfg(test)]
    publish_pause: Arc<PreparePause>,
    #[cfg(test)]
    status_pause: Arc<CheckpointPause>,
    #[cfg(test)]
    purge_pause: Arc<PreparePause>,
}

impl LinuxArtifactContent {
    /// Opens or creates the fixed private artifact hierarchy below `base`.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactContentError`] unless every retained directory is a
    /// non-symlink, owner-owned `0700` directory.
    pub fn open(base: &Path) -> Result<Self, ArtifactContentError> {
        let base = rustix::fs::open(base, directory_flags(), Mode::empty())
            .map_err(|_| ArtifactContentError::RootUnavailable)?;
        validate_private_directory(&rustix::fs::fstat(&base).map_err(io_error)?)
            .map_err(|_| ArtifactContentError::RootUnavailable)?;
        let root = ensure_private_directory(&base, OsStr::new(ROOT_DIRECTORY))?;
        let objects = ensure_private_directory(&root, OsStr::new(OBJECTS_DIRECTORY))?;
        let staging = ensure_private_directory(&root, OsStr::new(STAGING_DIRECTORY))?;
        rustix::fs::fsync(&root).map_err(io_error)?;
        purge_abandoned_staging(&staging)?;
        Ok(Self {
            objects: Arc::new(objects),
            staging: Arc::new(staging),
            staging_gates: Arc::new(StagingGateRegistry::default()),
            staging_cleanup_sync: Arc::new(StagingCleanupSync::default()),
            publication_sync: Arc::new(PublicationSync::default()),
            purge_sync: Arc::new(PurgeSync::default()),
            portal_connection: None,
            #[cfg(test)]
            prepare_pause: Arc::new(PreparePause::default()),
            #[cfg(test)]
            publish_pause: Arc::new(PreparePause::default()),
            #[cfg(test)]
            status_pause: Arc::new(CheckpointPause::default()),
            #[cfg(test)]
            purge_pause: Arc::new(PreparePause::default()),
        })
    }

    /// Qualifies and retains the session portal connection required for
    /// exact-version local opening without launching an application.
    ///
    /// # Errors
    ///
    /// Returns a fixed portal-unavailable class unless the `OpenURI` interface
    /// is reachable and supports the `OpenFile` method introduced in version 2.
    pub async fn qualify_open_portal(&mut self) -> Result<(), ArtifactContentError> {
        let connection =
            tokio::time::timeout(Duration::from_secs(2), ashpd::zbus::Connection::session())
                .await
                .map_err(|_| ArtifactContentError::PortalUnavailable)?
                .map_err(|_| ArtifactContentError::PortalUnavailable)?;
        let proxy = tokio::time::timeout(
            Duration::from_secs(2),
            ashpd::desktop::open_uri::OpenURIProxy::with_connection(connection.clone()),
        )
        .await
        .map_err(|_| ArtifactContentError::PortalUnavailable)?
        .map_err(|_| ArtifactContentError::PortalUnavailable)?;
        if !portal_version_supports_open_file(proxy.version()) {
            return Err(ArtifactContentError::PortalUnavailable);
        }
        self.portal_connection = Some(connection);
        Ok(())
    }

    async fn prepare(
        &self,
        source_path: &Path,
        artifact_id: &ArtifactId,
        version: u32,
        media_type: &str,
        max_bytes: u64,
        deadline_unix_ms: UnixMillis,
    ) -> Result<PreparedArtifactContent, ArtifactContentError> {
        check_deadline(deadline_unix_ms)?;
        if version == 0 || max_bytes > MAX_ARTIFACT_FILE_BYTES {
            return Err(ArtifactContentError::Integrity);
        }
        let gate = self.staging_gates.gate(artifact_id, version);
        let gate_guard = Arc::clone(&gate).lock_owned().await;
        let phase = Arc::new(AtomicU8::new(PREPARE_COPYING));
        let mut cancellation = PrepareCancellationGuard::new(Arc::clone(&phase));
        let request = PrepareRequest {
            source_path: source_path.to_path_buf(),
            artifact_id: artifact_id.clone(),
            version,
            media_type: media_type.to_owned(),
            max_bytes,
            deadline_unix_ms,
        };
        let staging = Arc::clone(&self.staging);
        let cleanup_sync = Arc::clone(&self.staging_cleanup_sync);
        #[cfg(test)]
        let pause = Arc::clone(&self.prepare_pause);
        let result = tokio::task::spawn_blocking(move || {
            // The gate outlives every local staging guard, including unwind,
            // so deterministic discard cannot race a detached blocking task.
            let result = prepare_blocking(
                request,
                &staging,
                &cleanup_sync,
                &phase,
                #[cfg(test)]
                &pause,
            );
            drop(gate_guard);
            result
        })
        .await
        .map_err(|_| ArtifactContentError::Io(std::io::Error::other("prepare worker failed")));
        cancellation.disarm();
        result?
    }

    async fn publish(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPublication, ArtifactContentError> {
        check_deadline(deadline_unix_ms)?;
        let gate = self
            .staging_gates
            .gate(&content.artifact_id, content.version);
        let gate_guard = Arc::clone(&gate).lock_owned().await;
        let phase = Arc::new(AtomicU8::new(PREPARE_COPYING));
        let mut cancellation = PrepareCancellationGuard::new(Arc::clone(&phase));
        let content = content.clone();
        let objects = Arc::clone(&self.objects);
        let staging = Arc::clone(&self.staging);
        let cleanup_sync = Arc::clone(&self.staging_cleanup_sync);
        let publication_sync = Arc::clone(&self.publication_sync);
        #[cfg(test)]
        let pause = Arc::clone(&self.publish_pause);
        let result = tokio::task::spawn_blocking(move || {
            let result = publish_blocking(
                &content,
                deadline_unix_ms,
                &objects,
                &staging,
                &cleanup_sync,
                &publication_sync,
                &phase,
                #[cfg(test)]
                &pause,
            );
            drop(gate_guard);
            result
        })
        .await
        .map_err(|_| ArtifactContentError::Io(std::io::Error::other("publish worker failed")));
        cancellation.disarm();
        let publication = result??;
        // A sync that crossed the application deadline is deliberately
        // reported as deadline-exceeded. Exact replay revalidates the durable
        // object while the ContentReady journal remains the source of truth.
        check_deadline(deadline_unix_ms)?;
        Ok(publication)
    }

    async fn status(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentStatus, ArtifactContentError> {
        check_deadline(deadline_unix_ms)?;
        let phase = Arc::new(AtomicU8::new(PREPARE_COPYING));
        let mut cancellation = PrepareCancellationGuard::new(Arc::clone(&phase));
        let content = content.clone();
        let objects = Arc::clone(&self.objects);
        let staging = Arc::clone(&self.staging);
        #[cfg(test)]
        let pause = Arc::clone(&self.status_pause);
        let result = tokio::task::spawn_blocking(move || {
            status_blocking(
                &content,
                deadline_unix_ms,
                &objects,
                &staging,
                &phase,
                #[cfg(test)]
                &pause,
            )
        })
        .await
        .map_err(|_| ArtifactContentError::Io(std::io::Error::other("status worker failed")));
        cancellation.disarm();
        result?
    }

    async fn purge(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPurge, ArtifactContentError> {
        check_deadline(deadline_unix_ms)?;
        let gate = self
            .staging_gates
            .gate(&content.artifact_id, content.version);
        let gate_guard = Arc::clone(&gate).lock_owned().await;
        let phase = Arc::new(AtomicU8::new(PREPARE_COPYING));
        let mut cancellation = PrepareCancellationGuard::new(Arc::clone(&phase));
        let content = content.clone();
        let objects = Arc::clone(&self.objects);
        let purge_sync = Arc::clone(&self.purge_sync);
        #[cfg(test)]
        let pause = Arc::clone(&self.purge_pause);
        let result = tokio::task::spawn_blocking(move || {
            let result = purge_blocking(
                &content,
                deadline_unix_ms,
                &objects,
                &purge_sync,
                &phase,
                #[cfg(test)]
                &pause,
            );
            drop(gate_guard);
            result
        })
        .await
        .map_err(|_| ArtifactContentError::Io(std::io::Error::other("purge worker failed")));
        cancellation.disarm();
        let purge = result??;
        // If durability completed after the caller-owned deadline, retain the
        // removal journal. Exact recovery re-proves absence and directory sync.
        check_deadline(deadline_unix_ms)?;
        Ok(purge)
    }

    async fn discard(&self, content: &ArtifactVersion) -> Result<(), ArtifactContentError> {
        self.discard_version(&content.artifact_id, content.version)
            .await
    }

    async fn discard_reserved(
        &self,
        artifact_id: &ArtifactId,
        version: u32,
    ) -> Result<(), ArtifactContentError> {
        self.discard_version(artifact_id, version).await
    }

    async fn discard_version(
        &self,
        artifact_id: &ArtifactId,
        version: u32,
    ) -> Result<(), ArtifactContentError> {
        let gate = self.staging_gates.gate(artifact_id, version);
        let gate_guard = Arc::clone(&gate).lock_owned().await;
        let artifact_id = artifact_id.clone();
        let staging = Arc::clone(&self.staging);
        let cleanup_sync = Arc::clone(&self.staging_cleanup_sync);
        tokio::task::spawn_blocking(move || {
            let result = discard_staging_names(&staging, &cleanup_sync, &artifact_id, version);
            drop(gate_guard);
            result
        })
        .await
        .map_err(|_| ArtifactContentError::Io(std::io::Error::other("cleanup worker failed")))?
    }

    async fn portal_open(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<(), ArtifactOpenError> {
        check_deadline(deadline_unix_ms)
            .map_err(|error| ArtifactOpenError::Known(open_failure(&error)))?;
        let content = content.clone();
        let objects = Arc::clone(&self.objects);
        let file = tokio::task::spawn_blocking(move || validated_object(&content, &objects))
            .await
            .map_err(|_| ArtifactOpenError::Known(ArtifactOpenFailureCode::PlatformUnavailable))?
            .map_err(|error| ArtifactOpenError::Known(open_failure(&error)))?;
        check_deadline(deadline_unix_ms)
            .map_err(|error| ArtifactOpenError::Known(open_failure(&error)))?;
        let connection = self
            .portal_connection
            .clone()
            .ok_or(ArtifactOpenError::Known(
                ArtifactOpenFailureCode::PlatformUnavailable,
            ))?;
        // From this call onward the portal may have accepted the descriptor
        // even when transport/response delivery fails. Every error is therefore
        // outcome-unknown and must never be persisted as a retryable failure.
        let request = ashpd::desktop::open_uri::OpenFileRequest::default()
            .writeable(false)
            .connection(Some(connection))
            .send_file(&file)
            .await
            .map_err(|_| ArtifactOpenError::OutcomeUnknown)?;
        request
            .response()
            .map_err(|_| ArtifactOpenError::OutcomeUnknown)
    }
}

// Every syscall touching the selected source or mutable staging namespace runs
// on Tokio's blocking pool. Linux offers no portable way to preempt a regular
// file syscall already in the kernel, so cancellation is checked between each
// bounded copy chunk and linearized immediately before the final rename. The
// caller-held staging gate remains owned until all RAII cleanup has completed.
#[allow(clippy::too_many_lines)]
fn prepare_blocking(
    request: PrepareRequest,
    staging: &Arc<OwnedFd>,
    cleanup_sync: &Arc<StagingCleanupSync>,
    phase: &Arc<AtomicU8>,
    #[cfg(test)] pause: &Arc<PreparePause>,
) -> Result<PreparedArtifactContent, ArtifactContentError> {
    check_prepare_copying(phase, request.deadline_unix_ms)?;
    let source_fd = open_source(&request.source_path)?;
    check_prepare_copying(phase, request.deadline_unix_ms)?;
    let before = rustix::fs::fstat(&source_fd).map_err(io_error)?;
    let expected_size = validate_source(&before, request.max_bytes)?;

    let temporary_name = copy_name(&request.artifact_id, request.version);
    // An earlier process interruption can leave only this deterministic,
    // private temporary. Exact serialization makes its removal race-free.
    remove_owned_staging(staging, cleanup_sync, &temporary_name)?;
    check_prepare_copying(phase, request.deadline_unix_ms)?;
    let target_fd = rustix::fs::openat(
        staging,
        &temporary_name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map_err(io_error)?;
    let target_identity = identity(&rustix::fs::fstat(&target_fd).map_err(io_error)?);
    let mut guard = StagingGuard::new(
        Arc::clone(staging),
        Arc::clone(cleanup_sync),
        temporary_name.clone(),
        target_identity,
    );
    let mut source = File::from(source_fd);
    let mut target = File::from(target_fd);
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();

    loop {
        check_prepare_copying(phase, request.deadline_unix_ms)?;
        let count = source.read(&mut buffer).map_err(ArtifactContentError::Io)?;
        check_prepare_copying(phase, request.deadline_unix_ms)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count).map_err(|_| ArtifactContentError::SourceTooLarge)?)
            .ok_or(ArtifactContentError::SourceTooLarge)?;
        if total > request.max_bytes {
            return Err(ArtifactContentError::SourceTooLarge);
        }
        if total > expected_size {
            return Err(ArtifactContentError::SourceChanged);
        }
        target
            .write_all(&buffer[..count])
            .map_err(ArtifactContentError::Io)?;
        digest.update(&buffer[..count]);
    }
    if total != expected_size {
        return Err(ArtifactContentError::SourceChanged);
    }
    let after = rustix::fs::fstat(&source).map_err(io_error)?;
    if !same_source_snapshot(&before, &after) {
        return Err(ArtifactContentError::SourceChanged);
    }
    target.flush().map_err(ArtifactContentError::Io)?;
    check_prepare_copying(phase, request.deadline_unix_ms)?;
    target.sync_all().map_err(ArtifactContentError::Io)?;
    check_prepare_copying(phase, request.deadline_unix_ms)?;
    let target_stat = rustix::fs::fstat(&target).map_err(io_error)?;
    validate_private_file(&target_stat)?;
    if identity(&target_stat) != target_identity
        || u64::try_from(target_stat.st_size).ok() != Some(total)
    {
        return Err(ArtifactContentError::Integrity);
    }

    let sha256: [u8; 32] = digest.finalize().into();
    let content = ArtifactVersion::new(
        request.artifact_id.clone(),
        request.version,
        sha256,
        request.media_type.clone(),
        total,
        0,
    )
    .map_err(|_| ArtifactContentError::Integrity)?;
    let prepared_name = prepared_name(&request.artifact_id, request.version);
    if entry_identity(staging, &temporary_name, target_identity) != EntryIdentity::Owned {
        return Err(ArtifactContentError::Integrity);
    }
    #[cfg(test)]
    pause.wait_before_finalization();
    check_deadline(request.deadline_unix_ms)?;
    phase
        .compare_exchange(
            PREPARE_COPYING,
            PREPARE_FINALIZING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| ArtifactContentError::DeadlineExceeded)?;
    #[cfg(test)]
    pause
        .finalizations
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let mut prepared_guard = None;
    match rustix::fs::renameat_with(
        staging,
        &temporary_name,
        staging,
        &prepared_name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            guard.disarm();
            prepared_guard = Some(StagingGuard::new(
                Arc::clone(staging),
                Arc::clone(cleanup_sync),
                prepared_name.clone(),
                target_identity,
            ));
            if entry_identity(staging, &prepared_name, target_identity) != EntryIdentity::Owned {
                return Err(ArtifactContentError::PublicationUncertain);
            }
        }
        Err(rustix::io::Errno::EXIST) => {
            // A same-process retry may observe the exact staged bytes. It is
            // accepted only after full digest validation.
            let existing = validated_named_content(&content, staging, &prepared_name)?;
            drop(existing);
            guard.cleanup()?;
        }
        Err(error) => return Err(ArtifactContentError::Io(io_error(error))),
    }
    rustix::fs::fsync(staging).map_err(io_error)?;
    if let Some(guard) = prepared_guard.as_mut() {
        guard.disarm();
    }
    phase.store(PREPARE_COMPLETE, Ordering::Release);
    Ok(PreparedArtifactContent {
        sha256,
        media_type: request.media_type,
        byte_size: total,
    })
}

fn check_prepare_copying(
    phase: &AtomicU8,
    deadline_unix_ms: UnixMillis,
) -> Result<(), ArtifactContentError> {
    if phase.load(Ordering::Acquire) != PREPARE_COPYING {
        return Err(ArtifactContentError::DeadlineExceeded);
    }
    check_deadline(deadline_unix_ms)
}

// Publication stays under the same exact staging gate as discard. If
// cancellation wins before `PREPARE_FINALIZING`, no namespace mutation starts.
// Once finalization wins, the worker runs rename and durability sync to a
// definite result; a timed-out caller can then resolve that result by exact
// object replay while cleanup waits for this gate.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn publish_blocking(
    content: &ArtifactVersion,
    deadline_unix_ms: UnixMillis,
    objects: &OwnedFd,
    staging: &OwnedFd,
    cleanup_sync: &StagingCleanupSync,
    publication_sync: &PublicationSync,
    phase: &AtomicU8,
    #[cfg(test)] pause: &PreparePause,
) -> Result<ArtifactContentPublication, ArtifactContentError> {
    check_prepare_copying(phase, deadline_unix_ms)?;
    let prepared = prepared_name(&content.artifact_id, content.version);
    let verified = validated_named_content_checked(content, staging, &prepared, || {
        check_prepare_copying(phase, deadline_unix_ms)
    });
    let staged_file = match verified {
        Ok(file) => file,
        Err(error @ (ArtifactContentError::NotFound | ArtifactContentError::Integrity)) => {
            // A prior exact publisher can already own the immutable object.
            // Revalidation includes every directory sync required before SQL
            // may observe this as a completed replay.
            return match validated_durable_object_checked(content, objects, staging, || {
                check_prepare_copying(phase, deadline_unix_ms)
            }) {
                Ok(file) => {
                    drop(file);
                    remove_owned_staging(staging, cleanup_sync, &prepared)?;
                    phase.store(PREPARE_COMPLETE, Ordering::Release);
                    check_deadline(deadline_unix_ms)?;
                    Ok(ArtifactContentPublication::AlreadyPublished)
                }
                Err(ArtifactContentError::NotFound) => Err(error),
                Err(published_error) => Err(published_error),
            };
        }
        Err(error) => return Err(error),
    };
    check_prepare_copying(phase, deadline_unix_ms)?;
    let staged_stat = rustix::fs::fstat(&staged_file).map_err(io_error)?;
    let staged_identity = identity(&staged_stat);
    match entry_identity(staging, &prepared, staged_identity) {
        EntryIdentity::Owned => {}
        EntryIdentity::Missing => {
            let published = validated_durable_object_checked(content, objects, staging, || {
                check_prepare_copying(phase, deadline_unix_ms)
            })?;
            drop(published);
            phase.store(PREPARE_COMPLETE, Ordering::Release);
            check_deadline(deadline_unix_ms)?;
            return Ok(ArtifactContentPublication::AlreadyPublished);
        }
        EntryIdentity::Other | EntryIdentity::Unreadable => {
            return Err(ArtifactContentError::Integrity);
        }
    }

    #[cfg(test)]
    pause.wait_before_finalization();
    check_deadline(deadline_unix_ms)?;
    phase
        .compare_exchange(
            PREPARE_COPYING,
            PREPARE_FINALIZING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| ArtifactContentError::DeadlineExceeded)?;
    #[cfg(test)]
    pause
        .finalizations
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let (shard_name, object_name) = object_names(&content.artifact_id, content.version);
    let shard = ensure_private_directory(objects, &shard_name)?;
    check_deadline(deadline_unix_ms)?;
    let publication = match rustix::fs::renameat_with(
        staging,
        &prepared,
        &shard,
        &object_name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            if entry_identity(&shard, &object_name, staged_identity) != EntryIdentity::Owned {
                return Err(ArtifactContentError::PublicationUncertain);
            }
            ArtifactContentPublication::Published
        }
        Err(rustix::io::Errno::EXIST) => {
            let existing = validated_named_content_checked(content, &shard, &object_name, || {
                check_deadline(deadline_unix_ms)
            })?;
            drop(existing);
            if entry_identity(staging, &prepared, staged_identity) == EntryIdentity::Owned {
                rustix::fs::unlinkat(staging, &prepared, AtFlags::empty()).map_err(io_error)?;
            }
            ArtifactContentPublication::AlreadyPublished
        }
        Err(error) => {
            match (
                entry_identity(staging, &prepared, staged_identity),
                entry_identity(&shard, &object_name, staged_identity),
            ) {
                (EntryIdentity::Missing, EntryIdentity::Owned) => {
                    ArtifactContentPublication::AlreadyPublished
                }
                (EntryIdentity::Owned, EntryIdentity::Missing | EntryIdentity::Other) => {
                    return Err(ArtifactContentError::Io(io_error(error)));
                }
                _ => return Err(ArtifactContentError::PublicationUncertain),
            }
        }
    };
    // Visibility is not enough for the SQL availability commit. A failure or
    // elapsed deadline after this sync remains exactly replayable from the
    // ContentReady journal and immutable destination.
    publication_sync.sync(&shard, staging)?;
    check_deadline(deadline_unix_ms)?;
    phase.store(PREPARE_COMPLETE, Ordering::Release);
    Ok(publication)
}

fn status_blocking(
    content: &ArtifactVersion,
    deadline_unix_ms: UnixMillis,
    objects: &OwnedFd,
    staging: &OwnedFd,
    phase: &AtomicU8,
    #[cfg(test)] pause: &CheckpointPause,
) -> Result<ArtifactContentStatus, ArtifactContentError> {
    let mut checkpoint = || {
        #[cfg(test)]
        pause.checkpoint();
        check_prepare_copying(phase, deadline_unix_ms)
    };
    match validated_object_checked(content, objects, &mut checkpoint) {
        Ok(file) => {
            drop(file);
            checkpoint()?;
            return Ok(ArtifactContentStatus::Published);
        }
        Err(ArtifactContentError::NotFound) => {}
        Err(error) => return Err(error),
    }
    checkpoint()?;
    let name = prepared_name(&content.artifact_id, content.version);
    match validated_named_content_checked(content, staging, &name, &mut checkpoint) {
        Ok(file) => {
            drop(file);
            checkpoint()?;
            Ok(ArtifactContentStatus::Prepared)
        }
        Err(ArtifactContentError::NotFound) => {
            checkpoint()?;
            Ok(ArtifactContentStatus::Missing)
        }
        Err(error) => Err(error),
    }
}

// Purge is retry-safe but still linearized. Removal validates the deterministic
// entry's private ownership and open-file identity, rather than its contents:
// corrupt daemon-owned bytes must remain removable. Once unlink wins, the
// worker finishes the shard/object directory sync so recovery can distinguish
// a durable namespace absence from an interrupted mutation. An already-open
// descriptor may continue to reference the unlinked inode; this operation does
// not promise descriptor revocation or immediate physical block reclamation.
fn purge_blocking(
    content: &ArtifactVersion,
    deadline_unix_ms: UnixMillis,
    objects: &OwnedFd,
    purge_sync: &PurgeSync,
    phase: &AtomicU8,
    #[cfg(test)] pause: &PreparePause,
) -> Result<ArtifactContentPurge, ArtifactContentError> {
    let checkpoint = || check_prepare_copying(phase, deadline_unix_ms);
    checkpoint()?;
    let (shard_name, object_name) = object_names(&content.artifact_id, content.version);
    let shard = match open_private_directory(objects, &shard_name) {
        Ok(shard) => shard,
        Err(ArtifactContentError::NotFound) => {
            checkpoint()?;
            purge_sync.sync_objects(objects)?;
            checkpoint()?;
            phase.store(PREPARE_COMPLETE, Ordering::Release);
            return Ok(ArtifactContentPurge::AlreadyAbsent);
        }
        Err(error) => return Err(error),
    };
    checkpoint()?;
    let file = match validated_owned_entry_for_purge(&shard, &object_name) {
        Ok(file) => file,
        Err(ArtifactContentError::NotFound) => {
            checkpoint()?;
            purge_sync.sync(&shard, objects)?;
            checkpoint()?;
            phase.store(PREPARE_COMPLETE, Ordering::Release);
            return Ok(ArtifactContentPurge::AlreadyAbsent);
        }
        Err(error) => return Err(error),
    };
    let expected_identity = identity(&rustix::fs::fstat(&file).map_err(io_error)?);
    if entry_identity(&shard, &object_name, expected_identity) != EntryIdentity::Owned {
        return Err(ArtifactContentError::Integrity);
    }
    #[cfg(test)]
    pause.wait_before_finalization();
    check_deadline(deadline_unix_ms)?;
    phase
        .compare_exchange(
            PREPARE_COPYING,
            PREPARE_FINALIZING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| ArtifactContentError::DeadlineExceeded)?;
    let final_stat = rustix::fs::fstat(&file).map_err(io_error)?;
    validate_private_file(&final_stat)?;
    if identity(&final_stat) != expected_identity
        || entry_identity(&shard, &object_name, expected_identity) != EntryIdentity::Owned
    {
        return Err(ArtifactContentError::Integrity);
    }
    #[cfg(test)]
    pause
        .finalizations
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    rustix::fs::unlinkat(&shard, &object_name, AtFlags::empty()).map_err(|error| {
        if error == rustix::io::Errno::NOENT {
            ArtifactContentError::PublicationUncertain
        } else {
            ArtifactContentError::Io(io_error(error))
        }
    })?;
    let after = rustix::fs::fstat(&file).map_err(io_error)?;
    if identity(&after) != expected_identity || after.st_nlink != 0 {
        return Err(ArtifactContentError::PublicationUncertain);
    }
    drop(file);
    purge_sync.sync(&shard, objects)?;
    check_deadline(deadline_unix_ms)?;
    phase.store(PREPARE_COMPLETE, Ordering::Release);
    Ok(ArtifactContentPurge::Purged)
}

fn validated_owned_entry_for_purge(
    directory: &OwnedFd,
    name: &OsStr,
) -> Result<OwnedFd, ArtifactContentError> {
    let file = rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::NOENT {
            ArtifactContentError::NotFound
        } else {
            ArtifactContentError::Io(io_error(error))
        }
    })?;
    let stat = rustix::fs::fstat(&file).map_err(io_error)?;
    validate_private_file(&stat)?;
    if entry_identity(directory, name, identity(&stat)) != EntryIdentity::Owned {
        return Err(ArtifactContentError::Integrity);
    }
    Ok(file)
}

const fn portal_version_supports_open_file(version: u32) -> bool {
    version >= 2
}

#[async_trait]
impl ArtifactContentStore for LinuxArtifactContent {
    async fn prepare_import_content(
        &self,
        source: &SelectedSourcePath,
        artifact_id: &ArtifactId,
        content_version: u32,
        media_type: &str,
        max_bytes: u64,
        deadline_unix_ms: UnixMillis,
    ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode> {
        self.prepare(
            source.as_path(),
            artifact_id,
            content_version,
            media_type,
            max_bytes,
            deadline_unix_ms,
        )
        .await
        .map_err(|error| import_failure(&error))
    }

    async fn publish_content(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPublication, ArtifactImportFailureCode> {
        self.publish(content, deadline_unix_ms)
            .await
            .map_err(|error| import_failure(&error))
    }

    async fn content_status(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentStatus, ArtifactImportFailureCode> {
        self.status(content, deadline_unix_ms)
            .await
            .map_err(|error| import_failure(&error))
    }

    async fn discard_prepared_content(
        &self,
        content: &ArtifactVersion,
    ) -> Result<(), ArtifactImportFailureCode> {
        self.discard(content)
            .await
            .map_err(|error| import_failure(&error))
    }

    async fn discard_reserved_content(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
    ) -> Result<(), ArtifactImportFailureCode> {
        self.discard_reserved(artifact_id, content_version)
            .await
            .map_err(|error| import_failure(&error))
    }
}

#[async_trait]
impl ArtifactOpener for LinuxArtifactContent {
    async fn open_artifact(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<(), ArtifactOpenError> {
        self.portal_open(content, deadline_unix_ms).await
    }
}

#[async_trait]
impl ArtifactContentRetention for LinuxArtifactContent {
    async fn purge_content(
        &self,
        content: &ArtifactVersion,
        deadline_unix_ms: UnixMillis,
    ) -> Result<ArtifactContentPurge, ArtifactRetentionFailureCode> {
        self.purge(content, deadline_unix_ms)
            .await
            .map_err(|error| retention_failure(&error))
    }
}

struct StagingGuard {
    directory: Arc<OwnedFd>,
    cleanup_sync: Arc<StagingCleanupSync>,
    name: OsString,
    identity: FileIdentity,
    armed: bool,
}

impl StagingGuard {
    fn new(
        directory: Arc<OwnedFd>,
        cleanup_sync: Arc<StagingCleanupSync>,
        name: OsString,
        identity: FileIdentity,
    ) -> Self {
        Self {
            directory,
            cleanup_sync,
            name,
            identity,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn cleanup(&mut self) -> Result<(), ArtifactContentError> {
        if !self.armed {
            return Ok(());
        }
        match entry_identity(&self.directory, &self.name, self.identity) {
            EntryIdentity::Owned => {
                rustix::fs::unlinkat(&*self.directory, &self.name, AtFlags::empty())
                    .map_err(io_error)?;
                self.cleanup_sync.sync(&self.directory)?;
                self.armed = false;
                Ok(())
            }
            EntryIdentity::Missing => {
                self.cleanup_sync.sync(&self.directory)?;
                self.armed = false;
                Ok(())
            }
            EntryIdentity::Other | EntryIdentity::Unreadable => {
                Err(ArtifactContentError::Integrity)
            }
        }
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn open_source(path: &Path) -> Result<OwnedFd, ArtifactContentError> {
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| ArtifactContentError::SourceUnavailable)
}

fn validated_object(
    content: &ArtifactVersion,
    objects: &OwnedFd,
) -> Result<File, ArtifactContentError> {
    let (shard_name, object_name) = object_names(&content.artifact_id, content.version);
    let shard = open_private_directory(objects, &shard_name)?;
    validated_named_content(content, &shard, &object_name)
}

fn validated_object_checked<F>(
    content: &ArtifactVersion,
    objects: &OwnedFd,
    mut checkpoint: F,
) -> Result<File, ArtifactContentError>
where
    F: FnMut() -> Result<(), ArtifactContentError>,
{
    checkpoint()?;
    let (shard_name, object_name) = object_names(&content.artifact_id, content.version);
    let shard = open_private_directory(objects, &shard_name)?;
    checkpoint()?;
    validated_named_content_checked(content, &shard, &object_name, &mut checkpoint)
}

fn validated_durable_object_checked<F>(
    content: &ArtifactVersion,
    objects: &OwnedFd,
    staging: &OwnedFd,
    mut checkpoint: F,
) -> Result<File, ArtifactContentError>
where
    F: FnMut() -> Result<(), ArtifactContentError>,
{
    checkpoint()?;
    let (shard_name, object_name) = object_names(&content.artifact_id, content.version);
    let shard = open_private_directory(objects, &shard_name)?;
    let file = validated_named_content_checked(content, &shard, &object_name, &mut checkpoint)?;
    checkpoint()?;
    rustix::fs::fsync(&shard).map_err(|_| ArtifactContentError::PublicationUncertain)?;
    checkpoint()?;
    rustix::fs::fsync(objects).map_err(|_| ArtifactContentError::PublicationUncertain)?;
    checkpoint()?;
    rustix::fs::fsync(staging).map_err(|_| ArtifactContentError::PublicationUncertain)?;
    checkpoint()?;
    Ok(file)
}

fn validated_named_content(
    content: &ArtifactVersion,
    directory: &OwnedFd,
    name: &OsStr,
) -> Result<File, ArtifactContentError> {
    validated_named_content_checked(content, directory, name, || Ok(()))
}

fn validated_named_content_checked<F>(
    content: &ArtifactVersion,
    directory: &OwnedFd,
    name: &OsStr,
    mut checkpoint: F,
) -> Result<File, ArtifactContentError>
where
    F: FnMut() -> Result<(), ArtifactContentError>,
{
    checkpoint()?;
    let fd = rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::NOENT {
            ArtifactContentError::NotFound
        } else {
            ArtifactContentError::Io(io_error(error))
        }
    })?;
    let before = rustix::fs::fstat(&fd).map_err(io_error)?;
    validate_private_file(&before)?;
    if u64::try_from(before.st_size).ok() != Some(content.byte_size)
        || content.byte_size > MAX_ARTIFACT_FILE_BYTES
    {
        return Err(ArtifactContentError::Integrity);
    }
    let expected_identity = identity(&before);
    let entry =
        rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
            if error == rustix::io::Errno::NOENT {
                ArtifactContentError::NotFound
            } else {
                ArtifactContentError::Io(io_error(error))
            }
        })?;
    if identity(&entry) != expected_identity {
        return Err(ArtifactContentError::Integrity);
    }

    let mut file = File::from(fd);
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    loop {
        checkpoint()?;
        let count = file.read(&mut buffer).map_err(ArtifactContentError::Io)?;
        checkpoint()?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count).map_err(|_| ArtifactContentError::Integrity)?)
            .ok_or(ArtifactContentError::Integrity)?;
        if total > content.byte_size {
            return Err(ArtifactContentError::Integrity);
        }
        digest.update(&buffer[..count]);
    }
    let after = rustix::fs::fstat(&file).map_err(io_error)?;
    checkpoint()?;
    let actual: [u8; 32] = digest.finalize().into();
    if total != content.byte_size
        || actual != content.sha256
        || identity(&after) != expected_identity
        || !same_file_snapshot(&before, &after)
    {
        return Err(ArtifactContentError::Integrity);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(ArtifactContentError::Io)?;
    checkpoint()?;
    Ok(file)
}

fn remove_owned_staging(
    staging: &OwnedFd,
    cleanup_sync: &StagingCleanupSync,
    name: &OsStr,
) -> Result<(), ArtifactContentError> {
    let fd = match rustix::fs::openat(
        staging,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return cleanup_sync.sync(staging),
        Err(error) => return Err(ArtifactContentError::Io(io_error(error))),
    };
    let stat = rustix::fs::fstat(&fd).map_err(io_error)?;
    validate_private_file(&stat)?;
    if entry_identity(staging, name, identity(&stat)) != EntryIdentity::Owned {
        return Err(ArtifactContentError::Integrity);
    }
    rustix::fs::unlinkat(staging, name, AtFlags::empty()).map_err(io_error)?;
    cleanup_sync.sync(staging)
}

fn discard_staging_names(
    staging: &OwnedFd,
    cleanup_sync: &StagingCleanupSync,
    artifact_id: &ArtifactId,
    version: u32,
) -> Result<(), ArtifactContentError> {
    let copy_result = remove_owned_staging(staging, cleanup_sync, &copy_name(artifact_id, version));
    let prepared_result =
        remove_owned_staging(staging, cleanup_sync, &prepared_name(artifact_id, version));
    copy_result?;
    prepared_result
}

fn ensure_private_directory(
    parent: &OwnedFd,
    name: &OsStr,
) -> Result<OwnedFd, ArtifactContentError> {
    match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(ArtifactContentError::Io(io_error(error))),
    }
    let directory = open_private_directory(parent, name)?;
    rustix::fs::fchmod(&directory, Mode::from_raw_mode(0o700)).map_err(io_error)?;
    validate_private_directory(&rustix::fs::fstat(&directory).map_err(io_error)?)?;
    rustix::fs::fsync(&directory).map_err(io_error)?;
    // Persist both a newly-created namespace entry and any permission repair
    // before callers use the directory as a publication boundary.
    rustix::fs::fsync(parent).map_err(io_error)?;
    Ok(directory)
}

fn open_private_directory(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, ArtifactContentError> {
    let directory =
        rustix::fs::openat(parent, name, directory_flags(), Mode::empty()).map_err(|error| {
            if error == rustix::io::Errno::NOENT {
                ArtifactContentError::NotFound
            } else {
                ArtifactContentError::Io(io_error(error))
            }
        })?;
    validate_private_directory(&rustix::fs::fstat(&directory).map_err(io_error)?)?;
    Ok(directory)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
}

fn validate_private_directory(stat: &Stat) -> Result<(), ArtifactContentError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7777 != 0o700
    {
        return Err(ArtifactContentError::Integrity);
    }
    Ok(())
}

fn validate_private_file(stat: &Stat) -> Result<(), ArtifactContentError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
    {
        return Err(ArtifactContentError::Integrity);
    }
    Ok(())
}

fn validate_source(stat: &Stat, max_bytes: u64) -> Result<u64, ArtifactContentError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(ArtifactContentError::SourceUnavailable);
    }
    let size = u64::try_from(stat.st_size).map_err(|_| ArtifactContentError::SourceUnavailable)?;
    if size > max_bytes {
        return Err(ArtifactContentError::SourceTooLarge);
    }
    Ok(size)
}

fn same_source_snapshot(before: &Stat, after: &Stat) -> bool {
    identity(before) == identity(after)
        && before.st_size == after.st_size
        && same_file_snapshot(before, after)
}

fn same_file_snapshot(before: &Stat, after: &Stat) -> bool {
    before.st_mtime == after.st_mtime
        && before.st_mtime_nsec == after.st_mtime_nsec
        && before.st_ctime == after.st_ctime
        && before.st_ctime_nsec == after.st_ctime_nsec
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

const fn identity(stat: &Stat) -> FileIdentity {
    FileIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryIdentity {
    Missing,
    Owned,
    Other,
    Unreadable,
}

fn entry_identity(directory: &OwnedFd, name: &OsStr, expected: FileIdentity) -> EntryIdentity {
    match rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if identity(&stat) == expected => EntryIdentity::Owned,
        Ok(_) => EntryIdentity::Other,
        Err(rustix::io::Errno::NOENT) => EntryIdentity::Missing,
        Err(_) => EntryIdentity::Unreadable,
    }
}

fn prepared_name(artifact_id: &ArtifactId, version: u32) -> OsString {
    let artifact_hash = hex_digest(artifact_id.as_str().as_bytes());
    OsString::from(format!("{artifact_hash}-v{version:07}.prepared"))
}

fn copy_name(artifact_id: &ArtifactId, version: u32) -> OsString {
    let artifact_hash = hex_digest(artifact_id.as_str().as_bytes());
    OsString::from(format!(".copy-{artifact_hash}-v{version:07}.tmp"))
}

fn object_names(artifact_id: &ArtifactId, version: u32) -> (OsString, OsString) {
    let artifact_hash = hex_digest(artifact_id.as_str().as_bytes());
    (
        OsString::from(&artifact_hash[..2]),
        OsString::from(format!("{artifact_hash}-v{version:07}.object")),
    )
}

fn hex_digest(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn purge_abandoned_staging(staging: &OwnedFd) -> Result<(), ArtifactContentError> {
    // The directory is adapter-owned and contains no committed object. A
    // restart never reuses selected paths, so every entry is abandoned until
    // a ContentReady journal proves exact bytes; those are reconciled only
    // after service construction. We therefore remove copy temporaries here
    // and retain deterministic `.prepared` entries for journal recovery.
    let path = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(staging));
    let entries = std::fs::read_dir(path).map_err(ArtifactContentError::Io)?;
    let mut count = 0_usize;
    for entry in entries {
        count = count
            .checked_add(1)
            .ok_or(ArtifactContentError::Integrity)?;
        if count > 20_000 {
            return Err(ArtifactContentError::Integrity);
        }
        let entry = entry.map_err(ArtifactContentError::Io)?;
        let name = entry.file_name();
        let name_bytes = std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str());
        if name_bytes.starts_with(b".copy-") && name_bytes.ends_with(b".tmp") {
            let stat =
                rustix::fs::statat(staging, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(io_error)?;
            validate_private_file(&stat)?;
            rustix::fs::unlinkat(staging, &name, AtFlags::empty()).map_err(io_error)?;
        }
    }
    rustix::fs::fsync(staging)
        .map_err(io_error)
        .map_err(ArtifactContentError::Io)
}

fn check_deadline(deadline_unix_ms: UnixMillis) -> Result<(), ArtifactContentError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ArtifactContentError::DeadlineExceeded)?;
    let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    if now_ms >= deadline_unix_ms {
        return Err(ArtifactContentError::DeadlineExceeded);
    }
    Ok(())
}

fn import_failure(error: &ArtifactContentError) -> ArtifactImportFailureCode {
    match error {
        ArtifactContentError::SourceUnavailable => ArtifactImportFailureCode::SourceUnavailable,
        ArtifactContentError::SourceChanged => ArtifactImportFailureCode::SourceChanged,
        ArtifactContentError::SourceTooLarge => ArtifactImportFailureCode::FileTooLarge,
        ArtifactContentError::DeadlineExceeded => ArtifactImportFailureCode::DeadlineExceeded,
        ArtifactContentError::Integrity => ArtifactImportFailureCode::IntegrityFailure,
        ArtifactContentError::PublicationUncertain => {
            ArtifactImportFailureCode::ContentStoreUnavailable
        }
        ArtifactContentError::RootUnavailable
        | ArtifactContentError::NotFound
        | ArtifactContentError::PortalUnavailable
        | ArtifactContentError::Io(_) => ArtifactImportFailureCode::ContentStoreUnavailable,
    }
}

fn open_failure(error: &ArtifactContentError) -> ArtifactOpenFailureCode {
    match error {
        ArtifactContentError::NotFound => ArtifactOpenFailureCode::ContentUnavailable,
        ArtifactContentError::DeadlineExceeded => ArtifactOpenFailureCode::DeadlineExceeded,
        ArtifactContentError::Integrity
        | ArtifactContentError::PublicationUncertain
        | ArtifactContentError::SourceChanged => ArtifactOpenFailureCode::IntegrityFailure,
        ArtifactContentError::RootUnavailable
        | ArtifactContentError::SourceUnavailable
        | ArtifactContentError::SourceTooLarge
        | ArtifactContentError::PortalUnavailable
        | ArtifactContentError::Io(_) => ArtifactOpenFailureCode::PlatformUnavailable,
    }
}

fn retention_failure(error: &ArtifactContentError) -> ArtifactRetentionFailureCode {
    match error {
        ArtifactContentError::DeadlineExceeded => ArtifactRetentionFailureCode::DeadlineExceeded,
        ArtifactContentError::Integrity | ArtifactContentError::SourceChanged => {
            ArtifactRetentionFailureCode::IntegrityFailure
        }
        ArtifactContentError::RootUnavailable
        | ArtifactContentError::SourceUnavailable
        | ArtifactContentError::SourceTooLarge
        | ArtifactContentError::NotFound
        | ArtifactContentError::PublicationUncertain
        | ArtifactContentError::PortalUnavailable
        | ArtifactContentError::Io(_) => ArtifactRetentionFailureCode::ContentStoreUnavailable,
    }
}

fn io_error(error: rustix::io::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _, path::PathBuf};

    use grok_application::{ArtifactContentStore as _, ArtifactOpener as _};
    use tempfile::TempDir;

    use super::*;

    fn private_temp() -> TempDir {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("private tempdir");
        directory
    }

    fn deadline() -> u64 {
        deadline_after(30_000)
    }

    fn deadline_after(milliseconds: u64) -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_millis(),
        )
        .expect("millis")
            + milliseconds
    }

    fn source(path: PathBuf) -> SelectedSourcePath {
        SelectedSourcePath::new(path).expect("selected source")
    }

    fn write_corrupt_prepared(base: &Path, artifact_id: &ArtifactId, version: u32) {
        let path = base
            .join(ROOT_DIRECTORY)
            .join(STAGING_DIRECTORY)
            .join(prepared_name(artifact_id, version));
        fs::write(&path, b"corrupt staged bytes").expect("corrupt prepared bytes");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("private prepared permissions");
    }

    #[tokio::test]
    async fn stages_publishes_and_verifies_pathless_immutable_content() {
        let directory = private_temp();
        let selected = directory.path().join("selected.txt");
        fs::write(&selected, b"immutable artifact\n").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact/with/pathlike-id").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected.clone()),
                &artifact_id,
                1,
                "text/plain",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Prepared)
        );
        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Ok(ArtifactContentPublication::Published)
        );
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Published)
        );
        assert_eq!(
            fs::read(selected).expect("source retained"),
            b"immutable artifact\n"
        );
    }

    #[tokio::test]
    async fn publication_replay_accepts_only_exact_journaled_bytes() {
        let directory = private_temp();
        let selected = directory.path().join("selected.bin");
        fs::write(&selected, b"first").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-1").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Ok(ArtifactContentPublication::AlreadyPublished)
        );
        let corrupt = ArtifactVersion::new(
            version.artifact_id.clone(),
            1,
            [0; 32],
            version.media_type.clone(),
            version.byte_size,
            1,
        )
        .expect("corrupt metadata");
        assert_eq!(
            storage.content_status(&corrupt, deadline()).await,
            Err(ArtifactImportFailureCode::IntegrityFailure)
        );
    }

    #[tokio::test]
    async fn exact_published_replay_removes_corrupt_deterministic_staging() {
        let directory = private_temp();
        let selected = directory.path().join("selected-corrupt-replay.bin");
        fs::write(&selected, b"durable object bytes").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-corrupt-replay").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id.clone(),
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("initial publish");
        write_corrupt_prepared(directory.path(), &artifact_id, 1);

        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Ok(ArtifactContentPublication::AlreadyPublished)
        );
        assert!(matches!(
            rustix::fs::statat(
                &*storage.staging,
                prepared_name(&artifact_id, 1),
                AtFlags::SYMLINK_NOFOLLOW,
            ),
            Err(rustix::io::Errno::NOENT)
        ));
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Published)
        );
    }

    #[tokio::test]
    async fn exact_published_replay_reports_corrupt_staging_cleanup_failure() {
        let directory = private_temp();
        let selected = directory.path().join("selected-corrupt-cleanup.bin");
        fs::write(&selected, b"durable cleanup bytes").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-corrupt-cleanup").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id.clone(),
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("initial publish");
        write_corrupt_prepared(directory.path(), &artifact_id, 1);
        storage
            .staging_cleanup_sync
            .failures
            .store(1, std::sync::atomic::Ordering::SeqCst);

        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Err(ArtifactImportFailureCode::ContentStoreUnavailable)
        );
        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Ok(ArtifactContentPublication::AlreadyPublished),
            "ContentReady replay succeeds only after the missing-entry sync retry"
        );
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Published)
        );
    }

    #[tokio::test]
    async fn directory_sync_uncertainty_stays_recoverable_until_exact_replay() {
        let directory = private_temp();
        let selected = directory.path().join("selected-sync.bin");
        fs::write(&selected, b"durable bytes").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-sync").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publication_sync
            .failures
            .store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Err(ArtifactImportFailureCode::ContentStoreUnavailable)
        );
        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Ok(ArtifactContentPublication::AlreadyPublished)
        );
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Published)
        );
    }

    #[tokio::test]
    async fn staging_cleanup_failure_is_reported_and_preserves_recoverable_bytes() {
        let directory = private_temp();
        let selected = directory.path().join("selected-cleanup.bin");
        fs::write(&selected, b"cleanup bytes").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-cleanup").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        rustix::fs::fchmod(&*storage.staging, Mode::from_raw_mode(0o500))
            .expect("make staging read-only");
        assert_eq!(
            storage.discard_prepared_content(&version).await,
            Err(ArtifactImportFailureCode::ContentStoreUnavailable)
        );
        rustix::fs::fchmod(&*storage.staging, Mode::from_raw_mode(0o700))
            .expect("restore staging permissions");
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Prepared)
        );
        storage
            .discard_prepared_content(&version)
            .await
            .expect("cleanup retry");
    }

    #[tokio::test]
    async fn missing_cleanup_retry_still_syncs_after_unlink_sync_failure() {
        let directory = private_temp();
        let selected = directory.path().join("selected-cleanup-retry.bin");
        fs::write(&selected, b"cleanup retry bytes").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-cleanup-retry").expect("id");
        storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        storage
            .staging_cleanup_sync
            .attempts
            .store(0, std::sync::atomic::Ordering::SeqCst);
        storage
            .staging_cleanup_sync
            .failures
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let name = prepared_name(&artifact_id, 1);
        assert!(matches!(
            remove_owned_staging(&storage.staging, &storage.staging_cleanup_sync, &name),
            Err(ArtifactContentError::Io(_))
        ));
        assert_eq!(
            entry_identity(
                &storage.staging,
                &name,
                FileIdentity {
                    device: u64::MAX,
                    inode: u64::MAX,
                },
            ),
            EntryIdentity::Missing
        );
        remove_owned_staging(&storage.staging, &storage.staging_cleanup_sync, &name)
            .expect("missing cleanup retry syncs directory");
        assert_eq!(
            storage
                .staging_cleanup_sync
                .attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "missing retry must not skip the durability sync"
        );
    }

    #[tokio::test]
    async fn prepared_bytes_survive_reopen_for_recovery_and_reserved_cleanup_is_exact() {
        let directory = private_temp();
        let selected = directory.path().join("selected.bin");
        fs::write(&selected, b"recoverable").expect("source");
        let artifact_id = ArtifactId::new("artifact-recovery").expect("id");
        let first = LinuxArtifactContent::open(directory.path()).expect("storage");
        let prepared = first
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        drop(first);
        let reopened = LinuxArtifactContent::open(directory.path()).expect("reopen");
        let version = ArtifactVersion::new(
            artifact_id.clone(),
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        assert_eq!(
            reopened.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Prepared)
        );
        reopened
            .discard_reserved_content(&artifact_id, 1)
            .await
            .expect("discard reserved content");
        assert_eq!(
            reopened.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Missing)
        );
    }

    #[tokio::test]
    async fn concurrent_publication_has_one_visible_winner_and_exact_replay() {
        let directory = private_temp();
        let selected = directory.path().join("selected.bin");
        fs::write(&selected, vec![0x5a; 1024 * 1024]).expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-race").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        let (left, right) = tokio::join!(
            storage.publish_content(&version, deadline()),
            storage.publish_content(&version, deadline())
        );
        let outcomes = [left.expect("left"), right.expect("right")];
        assert!(outcomes.contains(&ArtifactContentPublication::Published));
        assert!(outcomes.contains(&ArtifactContentPublication::AlreadyPublished));
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Published)
        );
    }

    #[tokio::test]
    async fn cancelled_blocking_publish_serializes_before_namespace_mutation() {
        let directory = private_temp();
        let selected = directory.path().join("selected-publish-cancel.bin");
        fs::write(&selected, b"cancelled publication").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-publish-cancel").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage.publish_pause.arm();

        let publish_storage = storage.clone();
        let publish_version = version.clone();
        let publish = tokio::spawn(async move {
            publish_storage
                .publish_content(&publish_version, deadline())
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !storage.publish_pause.reached() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("publish reached finalization boundary");
        publish.abort();
        assert!(publish.await.expect_err("publish cancelled").is_cancelled());

        let cleanup_storage = storage.clone();
        let cleanup_version = version.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_storage
                .discard_prepared_content(&cleanup_version)
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !cleanup.is_finished(),
            "cleanup must wait for publish ownership"
        );
        storage.publish_pause.release();
        tokio::time::timeout(Duration::from_secs(2), cleanup)
            .await
            .expect("cleanup completed")
            .expect("cleanup task")
            .expect("staging cleanup");
        assert_eq!(
            storage
                .publish_pause
                .finalizations
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "cancellation won before shard creation and object rename"
        );
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Missing)
        );
    }

    #[tokio::test]
    async fn deadline_after_publication_sync_is_exactly_replayable() {
        let directory = private_temp();
        let selected = directory.path().join("selected-post-sync.bin");
        fs::write(&selected, b"post sync deadline").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-post-sync").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage.publication_sync.after_sync.arm();
        let publish_storage = storage.clone();
        let publish_version = version.clone();
        let publish = tokio::spawn(async move {
            publish_storage
                .publish_content(&publish_version, deadline_after(500))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !storage.publication_sync.after_sync.reached() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("publication reached post-sync boundary");
        tokio::time::sleep(Duration::from_millis(600)).await;
        storage.publication_sync.after_sync.release();
        assert_eq!(
            publish.await.expect("publish task"),
            Err(ArtifactImportFailureCode::DeadlineExceeded)
        );
        assert_eq!(
            storage.publish_content(&version, deadline()).await,
            Ok(ArtifactContentPublication::AlreadyPublished)
        );
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Published)
        );
    }

    #[tokio::test]
    async fn content_status_stops_after_a_returned_chunk_crosses_deadline() {
        let directory = private_temp();
        let selected = directory.path().join("selected-status-deadline.bin");
        fs::write(&selected, vec![0x33; COPY_BUFFER_BYTES * 2]).expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-status-deadline").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        // Checkpoints 1-3 cover object/shard setup and checkpoint 4 is
        // immediately before the first digest read. Pausing checkpoint 5
        // therefore proves that one normally returning chunk has completed.
        storage.status_pause.arm_after(5);
        let status_storage = storage.clone();
        let status_version = version.clone();
        let status = tokio::spawn(async move {
            status_storage
                .content_status(&status_version, deadline_after(500))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !storage.status_pause.reached() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("status paused after first digest chunk");
        tokio::time::sleep(Duration::from_millis(600)).await;
        storage.status_pause.release();
        assert_eq!(
            status.await.expect("status task"),
            Err(ArtifactImportFailureCode::DeadlineExceeded)
        );
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Published)
        );
    }

    #[tokio::test]
    async fn exact_purge_is_directory_durable_and_replayable() {
        let directory = private_temp();
        let selected = directory.path().join("selected-purge.bin");
        fs::write(&selected, b"purge exact immutable bytes").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-purge").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::Purged)
        );
        assert_eq!(
            storage.content_status(&version, deadline()).await,
            Ok(ArtifactContentStatus::Missing)
        );
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::AlreadyAbsent)
        );
    }

    #[tokio::test]
    async fn corrupt_daemon_owned_object_is_safely_purged_by_identity() {
        let directory = private_temp();
        let selected = directory.path().join("selected-corrupt-purge.bin");
        fs::write(&selected, b"expected immutable bytes").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-corrupt-purge").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id.clone(),
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        let (shard, object) = object_names(&artifact_id, 1);
        let object_path = directory
            .path()
            .join(ROOT_DIRECTORY)
            .join(OBJECTS_DIRECTORY)
            .join(shard)
            .join(object);
        fs::write(
            &object_path,
            vec![0x44; usize::try_from(version.byte_size).expect("bounded test object size")],
        )
        .expect("corrupt object");
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::Purged)
        );
        assert!(!object_path.exists(), "owned corrupt object is unlinked");
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::AlreadyAbsent)
        );
    }

    #[tokio::test]
    async fn purge_rejects_an_entry_that_is_no_longer_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = private_temp();
        let selected = directory.path().join("selected-non-private-purge.bin");
        fs::write(&selected, b"private before tampering").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-non-private-purge").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id.clone(),
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        let (shard, object) = object_names(&artifact_id, 1);
        let object_path = directory
            .path()
            .join(ROOT_DIRECTORY)
            .join(OBJECTS_DIRECTORY)
            .join(shard)
            .join(object);
        fs::set_permissions(&object_path, fs::Permissions::from_mode(0o640))
            .expect("weaken object mode");
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Err(ArtifactRetentionFailureCode::IntegrityFailure)
        );
        assert!(
            object_path.exists(),
            "non-private entry is retained fail-closed"
        );
    }

    #[tokio::test]
    async fn purge_is_namespace_removal_and_does_not_revoke_an_open_descriptor() {
        use std::io::Read as _;

        let directory = private_temp();
        let selected = directory.path().join("selected-held-purge.bin");
        let expected = b"descriptor can outlive namespace removal";
        fs::write(&selected, expected).expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-held-purge").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id.clone(),
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        let (shard, object) = object_names(&artifact_id, 1);
        let object_path = directory
            .path()
            .join(ROOT_DIRECTORY)
            .join(OBJECTS_DIRECTORY)
            .join(shard)
            .join(object);
        let mut held = fs::File::open(&object_path).expect("held descriptor");

        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::Purged)
        );
        assert!(!object_path.exists(), "private namespace entry is absent");
        let mut bytes = Vec::new();
        held.read_to_end(&mut bytes).expect("read held descriptor");
        assert_eq!(bytes, expected);
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::AlreadyAbsent)
        );
    }

    #[tokio::test]
    async fn purge_sync_failure_keeps_recovery_exactly_replayable() {
        let directory = private_temp();
        let selected = directory.path().join("selected-purge-sync.bin");
        fs::write(&selected, b"purge sync recovery").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-purge-sync").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        storage
            .purge_sync
            .failures
            .store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Err(ArtifactRetentionFailureCode::ContentStoreUnavailable)
        );
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::AlreadyAbsent)
        );
    }

    #[tokio::test]
    async fn cancelled_purge_cannot_cross_the_unlink_linearization_point() {
        let directory = private_temp();
        let selected = directory.path().join("selected-purge-cancel.bin");
        fs::write(&selected, b"cancel purge before unlink").expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-purge-cancel").expect("id");
        let prepared = storage
            .prepare_import_content(
                &source(selected),
                &artifact_id,
                1,
                "application/octet-stream",
                MAX_ARTIFACT_FILE_BYTES,
                deadline(),
            )
            .await
            .expect("prepare");
        let version = ArtifactVersion::new(
            artifact_id,
            1,
            prepared.sha256,
            prepared.media_type,
            prepared.byte_size,
            1,
        )
        .expect("version");
        storage
            .publish_content(&version, deadline())
            .await
            .expect("publish");
        storage.purge_pause.arm();
        let purge_storage = storage.clone();
        let purge_version = version.clone();
        let purge = tokio::spawn(async move {
            purge_storage
                .purge_content(&purge_version, deadline())
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !storage.purge_pause.reached() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("purge reached finalization boundary");
        purge.abort();
        assert!(purge.await.expect_err("purge cancelled").is_cancelled());
        storage.purge_pause.release();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if storage
                    .content_status(&version, deadline())
                    .await
                    .is_ok_and(|status| status == ArtifactContentStatus::Published)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled worker released exact gate");
        assert_eq!(
            storage
                .purge_pause
                .finalizations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            storage.purge_content(&version, deadline()).await,
            Ok(ArtifactContentPurge::Purged)
        );
    }

    #[tokio::test]
    async fn symlink_and_expired_source_fail_without_retaining_staging() {
        let directory = private_temp();
        let selected = directory.path().join("selected.bin");
        fs::write(&selected, b"content").expect("source");
        let symlink = directory.path().join("selected-link.bin");
        std::os::unix::fs::symlink(&selected, &symlink).expect("symlink");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let id = ArtifactId::new("artifact-2").expect("id");
        assert_eq!(
            storage
                .prepare_import_content(
                    &source(symlink),
                    &id,
                    1,
                    "application/octet-stream",
                    MAX_ARTIFACT_FILE_BYTES,
                    deadline(),
                )
                .await,
            Err(ArtifactImportFailureCode::SourceUnavailable)
        );
        assert_eq!(
            storage
                .prepare_import_content(
                    &source(selected),
                    &id,
                    1,
                    "application/octet-stream",
                    MAX_ARTIFACT_FILE_BYTES,
                    1,
                )
                .await,
            Err(ArtifactImportFailureCode::DeadlineExceeded)
        );
    }

    #[tokio::test]
    async fn cancelled_blocking_prepare_serializes_exact_staging_cleanup() {
        let directory = private_temp();
        let selected = directory.path().join("selected-cancelled.bin");
        fs::write(&selected, vec![0x5a; COPY_BUFFER_BYTES * 2]).expect("source");
        let storage = LinuxArtifactContent::open(directory.path()).expect("storage");
        let artifact_id = ArtifactId::new("artifact-cancelled").expect("id");
        storage.prepare_pause.arm();

        let prepare_storage = storage.clone();
        let prepare_id = artifact_id.clone();
        let prepare = tokio::spawn(async move {
            prepare_storage
                .prepare_import_content(
                    &source(selected),
                    &prepare_id,
                    1,
                    "application/octet-stream",
                    MAX_ARTIFACT_FILE_BYTES,
                    deadline(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !storage.prepare_pause.reached() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking prepare reached finalization boundary");

        prepare.abort();
        assert!(prepare.await.expect_err("prepare cancelled").is_cancelled());
        let cleanup_storage = storage.clone();
        let cleanup_id = artifact_id.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_storage
                .discard_reserved_content(&cleanup_id, 1)
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !cleanup.is_finished(),
            "cleanup must wait for prepare ownership"
        );

        storage.prepare_pause.release();
        tokio::time::timeout(Duration::from_secs(2), cleanup)
            .await
            .expect("cleanup completed")
            .expect("cleanup task")
            .expect("staging cleanup");
        assert_eq!(
            storage
                .prepare_pause
                .finalizations
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "cancellation won before the staging rename"
        );
        let staging = directory
            .path()
            .join(ROOT_DIRECTORY)
            .join(STAGING_DIRECTORY);
        assert_eq!(
            fs::read_dir(staging).expect("staging directory").count(),
            0,
            "successful application cleanup leaves no copy or prepared bytes"
        );
    }

    #[test]
    fn unsafe_root_permissions_are_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("permissions");
        assert!(matches!(
            LinuxArtifactContent::open(directory.path()),
            Err(ArtifactContentError::RootUnavailable)
        ));
    }

    #[test]
    fn source_path_never_appears_in_fixed_errors_or_debug() {
        let selected =
            SelectedSourcePath::new(PathBuf::from("/private/customer-secret.txt")).expect("source");
        assert_eq!(format!("{selected:?}"), "SelectedSourcePath([REDACTED])");
        assert!(
            !ArtifactContentError::SourceUnavailable
                .to_string()
                .contains("customer-secret")
        );
    }

    #[test]
    fn portal_qualification_requires_open_file_interface_version() {
        assert!(!portal_version_supports_open_file(0));
        assert!(!portal_version_supports_open_file(1));
        assert!(portal_version_supports_open_file(2));
        assert!(portal_version_supports_open_file(u32::MAX));
    }

    // Unit tests never invoke the portal because opening is an external side
    // effect; daemon startup performs the bounded non-launching qualification.
    #[allow(dead_code)]
    async fn opener_typechecks(storage: &LinuxArtifactContent, version: &ArtifactVersion) {
        let _ = storage.open_artifact(version, deadline()).await;
    }
}
