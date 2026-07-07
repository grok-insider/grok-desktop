use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use fs2::FileExt;
use grok_application::{
    ExecutionMutationOutcome, ExecutionStore, KeyProviderError, MutationCommand, NewRunEvent,
    SecureKeyProvider, StoreError,
};
use grok_domain::{
    Approval, ApprovalId, ApprovalRisk, ApprovalScope, ApprovalStatus, EffectId, ProjectId,
    RequestedAction, Run, RunEvent, RunId, RunState, SideEffect, ThreadId,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{mapping, schema};

const RUN_COLUMNS: &str = "id, project_id, thread_id, state, revision, created_at, updated_at";
const APPROVAL_COLUMNS: &str = "id, run_id, action, target, data_summary, risk, scope, \
                                resource_id, status, revision, created_at, expires_at, decided_at";
const EFFECT_COLUMNS: &str =
    "id, run_id, kind, target, idempotency, state, revision, created_at, updated_at";
const EVENT_COLUMNS: &str = "sequence, run_id, occurred_at, kind, from_state, to_state, related_id";
const MAX_EXECUTION_OUTCOME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct StoredExecutionOutcome {
    version: u8,
    result: StoredExecutionResult,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredExecutionResult {
    Run {
        id: String,
        project_id: String,
        thread_id: String,
        state: String,
        revision: u64,
        created_at: u64,
        updated_at: u64,
    },
    Approval {
        id: String,
        run_id: String,
        action: String,
        target: String,
        data_summary: String,
        risk: String,
        scope: String,
        resource_id: Option<String>,
        status: String,
        revision: u64,
        created_at: u64,
        expires_at: u64,
        decided_at: Option<u64>,
    },
}

/// Adapter initialization, migration, backup, or integrity failure.
#[derive(Debug, Error)]
pub enum SqlCipherStoreError {
    /// `SQLCipher`-backed `SQLite` operation failed.
    #[error("encrypted database operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Database key could not be obtained.
    #[error(transparent)]
    Key(#[from] KeyProviderError),
    /// The linked `SQLite` library does not provide `SQLCipher`.
    #[error("linked SQLite library does not provide SQLCipher")]
    CipherUnavailable,
    /// Database was created by a newer application version.
    #[error("database schema {found} is newer than supported schema {supported}")]
    NewerSchema {
        /// Version found on disk.
        found: u32,
        /// Newest version this binary understands.
        supported: u32,
    },
    /// Integer cannot be represented in `SQLite`'s signed storage class.
    #[error("numeric value exceeds SQLite range")]
    NumericOverflow,
    /// The connection lock was poisoned by a failed blocking operation.
    #[error("database connection lock poisoned")]
    Poisoned,
    /// Blocking worker could not complete.
    #[error("database worker failed: {0}")]
    Worker(String),
    /// Another process or application instance owns this database path.
    #[error("encrypted database is already open by another application instance")]
    DatabaseInUse,
    /// Backup path already exists or cannot be managed safely.
    #[error("backup target is unsafe: {0}")]
    BackupTarget(String),
    /// The publication syscall returned an error and namespace reconciliation
    /// could not prove whether the backup committed.
    #[error("backup publication outcome is uncertain; inspect the target before retrying")]
    BackupPublicationUncertain,
    /// Filesystem operation around the database failed.
    #[error("database filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Process-lifetime exclusive advisory lock for one canonical database path.
pub struct DatabaseLock {
    database_path: PathBuf,
    _file: File,
}

impl std::fmt::Debug for DatabaseLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseLock")
            .field("database_path", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl DatabaseLock {
    /// Acquires the exclusive lock before key provisioning or database access.
    ///
    /// # Errors
    ///
    /// Returns [`SqlCipherStoreError::DatabaseInUse`] when another instance owns
    /// the lock, or an I/O error when the lock file cannot be secured.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, SqlCipherStoreError> {
        let database_path = canonical_database_path(path.as_ref())?;
        let file_name = database_path
            .file_name()
            .ok_or_else(|| std::io::Error::other("database path has no file name"))?;
        let mut lock_name = file_name.to_os_string();
        lock_name.push(".lock");
        let lock_path = database_path.with_file_name(lock_name);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        let file = options.open(lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        if let Err(error) = file.try_lock_exclusive() {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(SqlCipherStoreError::DatabaseInUse);
            }
            return Err(error.into());
        }
        Ok(Self {
            database_path,
            _file: file,
        })
    }
}

/// Result of `SQLCipher` and `SQLite` integrity checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    /// Page-level `SQLCipher` errors; empty means no errors.
    pub cipher_errors: Vec<String>,
    /// `SQLite` structural results, expected to contain exactly `ok`.
    pub sqlite_results: Vec<String>,
}

impl IntegrityReport {
    /// Returns whether both encryption and structural checks passed.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.cipher_errors.is_empty()
            && self.sqlite_results.len() == 1
            && self.sqlite_results[0] == "ok"
    }
}

/// SQLCipher-backed implementation of the execution aggregate store.
#[derive(Clone)]
pub struct SqlCipherStore {
    connection: Arc<Mutex<Connection>>,
    key_provider: Arc<dyn SecureKeyProvider>,
    _lock: Arc<DatabaseLock>,
}

impl std::fmt::Debug for SqlCipherStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqlCipherStore")
            .field("connection", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl SqlCipherStore {
    /// Opens, verifies, configures, and migrates an encrypted database.
    ///
    /// # Errors
    ///
    /// Returns [`SqlCipherStoreError`] when the key is unavailable or incorrect,
    /// `SQLCipher` is missing, migration fails, or a blocking worker fails.
    pub async fn open(
        path: impl AsRef<Path>,
        key_provider: Arc<dyn SecureKeyProvider>,
    ) -> Result<Self, SqlCipherStoreError> {
        let lock = DatabaseLock::acquire(path.as_ref())?;
        Self::open_locked(path, key_provider, lock).await
    }

    /// Opens a database using a lock acquired before external key provisioning.
    ///
    /// # Errors
    ///
    /// Returns [`SqlCipherStoreError`] when the lock targets another path, the
    /// key is unavailable, or encrypted database initialization fails.
    pub async fn open_locked(
        path: impl AsRef<Path>,
        key_provider: Arc<dyn SecureKeyProvider>,
        lock: DatabaseLock,
    ) -> Result<Self, SqlCipherStoreError> {
        let path = canonical_database_path(path.as_ref())?;
        if path != lock.database_path {
            return Err(std::io::Error::other("database lock targets another path").into());
        }
        let key = key_provider.database_key()?;
        let connection = tokio::task::spawn_blocking(move || schema::open_encrypted(&path, &key))
            .await
            .map_err(|error| SqlCipherStoreError::Worker(error.to_string()))??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            key_provider,
            _lock: Arc::new(lock),
        })
    }

    /// Runs `SQLCipher` and `SQLite` integrity checks on the live snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SqlCipherStoreError`] when the check cannot be executed.
    pub async fn verify_integrity(&self) -> Result<IntegrityReport, SqlCipherStoreError> {
        self.with_connection(|connection| {
            let cipher_errors = collect_strings(connection, "PRAGMA cipher_integrity_check")?;
            let sqlite_results = collect_strings(connection, "PRAGMA integrity_check")?;
            Ok(IntegrityReport {
                cipher_errors,
                sqlite_results,
            })
        })
        .await
    }

    /// Checkpoints the write-ahead log into the encrypted main database.
    ///
    /// # Errors
    ///
    /// Returns [`SqlCipherStoreError`] when checkpointing fails.
    pub async fn checkpoint(&self) -> Result<(), SqlCipherStoreError> {
        self.with_connection(|connection| {
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await
    }

    /// Creates an online encrypted backup and atomically publishes it at `target`.
    ///
    /// # Errors
    ///
    /// Returns [`SqlCipherStoreError`] for unsafe targets, key failures, I/O, or
    /// `SQLite` backup failures. Existing targets are never overwritten.
    pub async fn backup_to(
        &self,
        target: impl AsRef<Path>,
    ) -> Result<PathBuf, SqlCipherStoreError> {
        self.backup_to_inner(target.as_ref().to_path_buf(), None, None)
            .await
    }

    async fn backup_to_inner(
        &self,
        target: PathBuf,
        before_publish: Option<BackupPublishHook>,
        rename_hook: Option<BackupRenameHook>,
    ) -> Result<PathBuf, SqlCipherStoreError> {
        if !backup_publication_supported() {
            // Windows needs an audited handle-relative no-replace rename
            // primitive before this API can safely publish. Path-based
            // move/rename calls are deliberately not a compatibility fallback.
            return Err(SqlCipherStoreError::BackupTarget(
                "atomic no-replace backup publication is unavailable on this platform".into(),
            ));
        }
        let target = canonical_backup_target(&target)?;
        let key = self.key_provider.database_key()?;
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            create_and_publish_backup(&connection, &key, target, before_publish, rename_hook)
        })
        .await
        .map_err(|error| SqlCipherStoreError::Worker(error.to_string()))?
    }

    #[cfg(all(test, target_os = "linux"))]
    async fn backup_to_with_publish_hook<F>(
        &self,
        target: impl AsRef<Path>,
        before_publish: F,
    ) -> Result<PathBuf, SqlCipherStoreError>
    where
        F: FnOnce(&Path) -> Result<(), SqlCipherStoreError> + Send + 'static,
    {
        self.backup_to_inner(
            target.as_ref().to_path_buf(),
            Some(Box::new(before_publish)),
            None,
        )
        .await
    }

    #[cfg(all(test, target_os = "linux"))]
    async fn backup_to_with_rename_fault(
        &self,
        target: impl AsRef<Path>,
        fault: BackupRenameFault,
    ) -> Result<PathBuf, SqlCipherStoreError> {
        use rustix::fs::RenameFlags;

        let rename_hook: BackupRenameHook = Box::new(move |staging, file_name| match fault {
            BackupRenameFault::CommitThenReportError => {
                rustix::fs::renameat_with(
                    &staging.directory_fd,
                    BACKUP_SNAPSHOT_NAME,
                    &staging.parent_fd,
                    file_name,
                    RenameFlags::NOREPLACE,
                )?;
                Err(rustix::io::Errno::IO)
            }
            BackupRenameFault::CommitThenReportExist => {
                rustix::fs::renameat_with(
                    &staging.directory_fd,
                    BACKUP_SNAPSHOT_NAME,
                    &staging.parent_fd,
                    file_name,
                    RenameFlags::NOREPLACE,
                )?;
                Err(rustix::io::Errno::EXIST)
            }
            BackupRenameFault::LoseSourceThenReportError => {
                rustix::fs::unlinkat(
                    &staging.directory_fd,
                    BACKUP_SNAPSHOT_NAME,
                    rustix::fs::AtFlags::empty(),
                )?;
                Err(rustix::io::Errno::IO)
            }
        });
        self.backup_to_inner(target.as_ref().to_path_buf(), None, Some(rename_hook))
            .await
    }

    async fn with_connection<T, F>(&self, operation: F) -> Result<T, SqlCipherStoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, SqlCipherStoreError> + Send + 'static,
    {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| SqlCipherStoreError::Poisoned)?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| SqlCipherStoreError::Worker(error.to_string()))?
    }

    pub(crate) async fn with_store<T, F>(&self, operation: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| StoreError::Internal("database connection lock poisoned".into()))?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| StoreError::Internal(format!("database worker failed: {error}")))?
    }
}

type BackupPublishHook = Box<dyn FnOnce(&Path) -> Result<(), SqlCipherStoreError> + Send + 'static>;

#[cfg(target_os = "linux")]
type BackupStagingHook = Box<dyn FnOnce() -> Result<(), SqlCipherStoreError> + Send + 'static>;

#[cfg(target_os = "linux")]
type BackupRenameHook = Box<
    dyn FnOnce(&BackupStaging, &std::ffi::OsStr) -> Result<(), rustix::io::Errno> + Send + 'static,
>;

#[cfg(not(target_os = "linux"))]
type BackupRenameHook = ();

#[cfg(all(test, target_os = "linux"))]
#[derive(Clone, Copy)]
enum BackupRenameFault {
    CommitThenReportError,
    CommitThenReportExist,
    LoseSourceThenReportError,
}

#[cfg(target_os = "linux")]
const BACKUP_SNAPSHOT_NAME: &str = "snapshot.db";

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackupIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
struct BackupStaging {
    parent_path: PathBuf,
    parent_fd: std::os::fd::OwnedFd,
    parent_identity: BackupIdentity,
    directory_name: std::ffi::OsString,
    directory_fd: std::os::fd::OwnedFd,
    directory_identity: BackupIdentity,
    database_fd: std::os::fd::OwnedFd,
    database_identity: BackupIdentity,
    cleaned: bool,
}

#[cfg(target_os = "linux")]
impl BackupStaging {
    fn create(parent: &Path) -> Result<Self, SqlCipherStoreError> {
        Self::create_inner(parent, None)
    }

    #[cfg(test)]
    fn create_with_post_file_failure(parent: &Path) -> Result<Self, SqlCipherStoreError> {
        Self::create_inner(
            parent,
            Some(Box::new(|| {
                Err(SqlCipherStoreError::BackupTarget(
                    "injected post-file creation failure".into(),
                ))
            })),
        )
    }

    fn create_inner(
        parent: &Path,
        mut after_file_created: Option<BackupStagingHook>,
    ) -> Result<Self, SqlCipherStoreError> {
        use rustix::fs::{AtFlags, Mode, OFlags};

        let directory_flags = private_directory_open_flags();
        let (parent_fd, parent_identity) = open_private_backup_parent(parent)?;

        for _ in 0..16 {
            let directory_name = backup_staging_name();
            match rustix::fs::mkdirat(&parent_fd, &directory_name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(rustix_io_error(error).into()),
            }

            let result = (|| {
                let directory_fd =
                    rustix::fs::openat(&parent_fd, &directory_name, directory_flags, Mode::empty())
                        .map_err(rustix_io_error)?;
                rustix::fs::fchmod(&directory_fd, Mode::from_raw_mode(0o700))
                    .map_err(rustix_io_error)?;
                let directory_stat = rustix::fs::fstat(&directory_fd).map_err(rustix_io_error)?;
                validate_private_directory_stat(&directory_stat, "backup staging directory")?;
                let directory_identity = backup_identity(&directory_stat);
                let directory_entry =
                    rustix::fs::statat(&parent_fd, &directory_name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(rustix_io_error)?;
                if backup_identity(&directory_entry) != directory_identity {
                    return Err(SqlCipherStoreError::BackupTarget(
                        "backup staging directory identity changed".into(),
                    ));
                }

                let retained_parent_fd = rustix::io::dup(&parent_fd).map_err(rustix_io_error)?;
                let database_fd = rustix::fs::openat(
                    &directory_fd,
                    BACKUP_SNAPSHOT_NAME,
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::CLOEXEC
                        | OFlags::NOFOLLOW,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(rustix_io_error)?;
                let database_stat = match rustix::fs::fstat(&database_fd) {
                    Ok(stat) => stat,
                    Err(error) => {
                        let _ = rustix::fs::unlinkat(
                            &directory_fd,
                            BACKUP_SNAPSHOT_NAME,
                            AtFlags::empty(),
                        );
                        return Err(rustix_io_error(error).into());
                    }
                };
                let database_identity = backup_identity(&database_stat);
                let staging = Self {
                    parent_path: parent.to_path_buf(),
                    parent_fd: retained_parent_fd,
                    parent_identity,
                    directory_name: directory_name.clone(),
                    directory_fd,
                    directory_identity,
                    database_fd,
                    database_identity,
                    cleaned: false,
                };

                if let Some(after_file_created) = after_file_created.take() {
                    after_file_created()?;
                }

                rustix::fs::fchmod(&staging.database_fd, Mode::from_raw_mode(0o600))
                    .map_err(rustix_io_error)?;
                let database_stat =
                    rustix::fs::fstat(&staging.database_fd).map_err(rustix_io_error)?;
                validate_private_database_stat(&database_stat)?;
                if backup_identity(&database_stat) != staging.database_identity {
                    return Err(SqlCipherStoreError::BackupTarget(
                        "backup snapshot descriptor identity changed".into(),
                    ));
                }
                let database_entry = rustix::fs::statat(
                    &staging.directory_fd,
                    BACKUP_SNAPSHOT_NAME,
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(rustix_io_error)?;
                if backup_identity(&database_entry) != staging.database_identity {
                    return Err(SqlCipherStoreError::BackupTarget(
                        "backup snapshot identity changed".into(),
                    ));
                }

                Ok(staging)
            })();
            match result {
                Ok(staging) => return Ok(staging),
                Err(error) => {
                    let _ = rustix::fs::unlinkat(&parent_fd, &directory_name, AtFlags::REMOVEDIR);
                    return Err(error);
                }
            }
        }
        Err(SqlCipherStoreError::BackupTarget(
            "could not reserve private backup staging".into(),
        ))
    }

    fn database_path(&self) -> PathBuf {
        use std::os::fd::AsRawFd as _;

        PathBuf::from(format!(
            "/proc/self/fd/{}/{}",
            self.directory_fd.as_raw_fd(),
            BACKUP_SNAPSHOT_NAME
        ))
    }

    fn verify_bindings(&self) -> Result<(), SqlCipherStoreError> {
        use rustix::fs::{AtFlags, Mode, OFlags};

        let parent_stat = rustix::fs::fstat(&self.parent_fd).map_err(rustix_io_error)?;
        validate_private_directory_stat(&parent_stat, "target parent")?;
        if backup_identity(&parent_stat) != self.parent_identity {
            return Err(SqlCipherStoreError::BackupTarget(
                "target parent descriptor identity changed".into(),
            ));
        }

        let reopened_parent = rustix::fs::open(
            &self.parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(rustix_io_error)?;
        let reopened_parent_stat = rustix::fs::fstat(&reopened_parent).map_err(rustix_io_error)?;
        validate_private_directory_stat(&reopened_parent_stat, "target parent")?;
        if backup_identity(&reopened_parent_stat) != self.parent_identity {
            return Err(SqlCipherStoreError::BackupTarget(
                "target parent path identity changed".into(),
            ));
        }

        let directory_stat = rustix::fs::fstat(&self.directory_fd).map_err(rustix_io_error)?;
        validate_private_directory_stat(&directory_stat, "backup staging directory")?;
        if backup_identity(&directory_stat) != self.directory_identity {
            return Err(SqlCipherStoreError::BackupTarget(
                "backup staging descriptor identity changed".into(),
            ));
        }
        let directory_entry = rustix::fs::statat(
            &self.parent_fd,
            &self.directory_name,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(rustix_io_error)?;
        if backup_identity(&directory_entry) != self.directory_identity {
            return Err(SqlCipherStoreError::BackupTarget(
                "backup staging path identity changed".into(),
            ));
        }

        let database_stat = rustix::fs::fstat(&self.database_fd).map_err(rustix_io_error)?;
        validate_private_database_stat(&database_stat)?;
        if backup_identity(&database_stat) != self.database_identity {
            return Err(SqlCipherStoreError::BackupTarget(
                "backup snapshot descriptor identity changed".into(),
            ));
        }
        let database_entry = rustix::fs::statat(
            &self.directory_fd,
            BACKUP_SNAPSHOT_NAME,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(rustix_io_error)?;
        validate_private_database_stat(&database_entry)?;
        if backup_identity(&database_entry) != self.database_identity {
            return Err(SqlCipherStoreError::BackupTarget(
                "backup snapshot path identity changed".into(),
            ));
        }
        ensure_staging_sidecars_absent(&self.directory_fd)
    }

    fn cleanup(&mut self) {
        use rustix::fs::AtFlags;

        if self.cleaned {
            return;
        }

        if let Ok(entry) = rustix::fs::statat(
            &self.directory_fd,
            BACKUP_SNAPSHOT_NAME,
            AtFlags::SYMLINK_NOFOLLOW,
        ) && backup_identity(&entry) == self.database_identity
        {
            let _ =
                rustix::fs::unlinkat(&self.directory_fd, BACKUP_SNAPSHOT_NAME, AtFlags::empty());
        }

        // Sidecar names are removed relative to the retained private staging
        // directory. `unlinkat` removes only the directory entry and never
        // follows a symlink to external content.
        for suffix in ["-wal", "-shm", "-journal"] {
            let _ = rustix::fs::unlinkat(
                &self.directory_fd,
                sidecar_name(std::ffi::OsStr::new(BACKUP_SNAPSHOT_NAME), suffix),
                AtFlags::empty(),
            );
        }

        if let Ok(entry) = rustix::fs::statat(
            &self.parent_fd,
            &self.directory_name,
            AtFlags::SYMLINK_NOFOLLOW,
        ) && backup_identity(&entry) == self.directory_identity
        {
            let _ = rustix::fs::unlinkat(&self.parent_fd, &self.directory_name, AtFlags::REMOVEDIR);
        }
        self.cleaned = true;
    }
}

#[cfg(target_os = "linux")]
impl Drop for BackupStaging {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn canonical_backup_target(target: &Path) -> Result<PathBuf, SqlCipherStoreError> {
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()?.join(target)
    };
    let file_name = absolute.file_name().ok_or_else(|| {
        SqlCipherStoreError::BackupTarget("target must name one backup file".into())
    })?;
    let parent = absolute
        .parent()
        .ok_or_else(|| SqlCipherStoreError::BackupTarget("target has no parent".into()))?
        .canonicalize()?;
    validate_private_backup_parent(&parent)?;
    let target = parent.join(file_name);
    #[cfg(target_os = "linux")]
    ensure_target_and_sidecars_absent_path(&target)?;
    Ok(target)
}

#[cfg(target_os = "linux")]
fn create_and_publish_backup(
    connection: &Arc<Mutex<Connection>>,
    key: &grok_application::DatabaseKey,
    target: PathBuf,
    before_publish: Option<BackupPublishHook>,
    rename_hook: Option<BackupRenameHook>,
) -> Result<PathBuf, SqlCipherStoreError> {
    let parent = target
        .parent()
        .ok_or_else(|| SqlCipherStoreError::BackupTarget("target has no parent".into()))?;
    let mut staging = BackupStaging::create(parent)?;
    let database_path = staging.database_path();
    let mut destination = open_backup_destination(&database_path, key)?;
    {
        let source = connection
            .lock()
            .map_err(|_| SqlCipherStoreError::Poisoned)?;
        let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
        backup.run_to_completion(128, Duration::from_millis(10), None)?;
    }
    finalize_backup_destination(destination)?;
    ensure_staging_sidecars_absent(&staging.directory_fd)?;
    verify_staged_snapshot_read_only(&database_path, key)?;
    staging.verify_bindings()?;
    rustix::fs::fsync(&staging.database_fd).map_err(rustix_io_error)?;
    rustix::fs::fsync(&staging.directory_fd).map_err(rustix_io_error)?;
    rustix::fs::fsync(&staging.parent_fd).map_err(rustix_io_error)?;

    publish_backup_no_replace(&staging, &target, before_publish, rename_hook)?;
    // The handle-relative no-replace rename above is the commit point. The
    // moved inode was already closed, integrity-checked, synced, private,
    // regular, and single-linked. Nothing fallible after this line may turn a
    // committed backup into an apparent retryable failure.
    staging.cleanup();
    let _ = rustix::fs::fsync(&staging.parent_fd);
    Ok(target)
}

#[cfg(not(target_os = "linux"))]
fn create_and_publish_backup(
    _connection: &Arc<Mutex<Connection>>,
    _key: &grok_application::DatabaseKey,
    _target: PathBuf,
    _before_publish: Option<BackupPublishHook>,
    _rename_hook: Option<BackupRenameHook>,
) -> Result<PathBuf, SqlCipherStoreError> {
    Err(SqlCipherStoreError::BackupTarget(
        "atomic no-replace backup publication is unavailable on this platform".into(),
    ))
}

const fn backup_publication_supported() -> bool {
    cfg!(target_os = "linux")
}

#[cfg(target_os = "linux")]
fn publish_backup_no_replace(
    staging: &BackupStaging,
    target: &Path,
    before_publish: Option<BackupPublishHook>,
    rename_hook: Option<BackupRenameHook>,
) -> Result<(), SqlCipherStoreError> {
    use rustix::fs::RenameFlags;

    let file_name = target.file_name().ok_or_else(|| {
        SqlCipherStoreError::BackupTarget("target must name one backup file".into())
    })?;
    // Tests use this barrier to create the destination after both directory
    // handles are bound but immediately before the no-replace commit.
    if let Some(before_publish) = before_publish {
        before_publish(target)?;
    }
    staging.verify_bindings()?;
    ensure_target_and_sidecars_absent(&staging.parent_fd, file_name)?;
    let rename_result = if let Some(rename_hook) = rename_hook {
        rename_hook(staging, file_name)
    } else {
        rustix::fs::renameat_with(
            &staging.directory_fd,
            BACKUP_SNAPSHOT_NAME,
            &staging.parent_fd,
            file_name,
            RenameFlags::NOREPLACE,
        )
    };
    match rename_result {
        Ok(()) => {
            // Publication has committed. This sync is a durability assist,
            // not a reason to report a retryable failure after visibility.
            let _ = rustix::fs::fsync(&staging.parent_fd);
            Ok(())
        }
        Err(error) => match reconcile_failed_publication(staging, file_name) {
            BackupPublicationState::Committed => {
                let _ = rustix::fs::fsync(&staging.parent_fd);
                Ok(())
            }
            BackupPublicationState::NotCommitted if error == rustix::io::Errno::EXIST => Err(
                SqlCipherStoreError::BackupTarget("target appeared before publication".into()),
            ),
            BackupPublicationState::NotCommitted => Err(rustix_io_error(error).into()),
            BackupPublicationState::Uncertain => {
                Err(SqlCipherStoreError::BackupPublicationUncertain)
            }
        },
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackupPublicationState {
    Committed,
    NotCommitted,
    Uncertain,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackupEntryState {
    Missing,
    Owned,
    Other,
    Unreadable,
}

#[cfg(target_os = "linux")]
fn backup_entry_state(
    directory_fd: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    expected: BackupIdentity,
) -> BackupEntryState {
    match rustix::fs::statat(directory_fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if backup_identity(&stat) == expected => BackupEntryState::Owned,
        Ok(_) => BackupEntryState::Other,
        Err(rustix::io::Errno::NOENT) => BackupEntryState::Missing,
        Err(_) => BackupEntryState::Unreadable,
    }
}

#[cfg(target_os = "linux")]
fn reconcile_failed_publication(
    staging: &BackupStaging,
    file_name: &std::ffi::OsStr,
) -> BackupPublicationState {
    let source = backup_entry_state(
        &staging.directory_fd,
        std::ffi::OsStr::new(BACKUP_SNAPSHOT_NAME),
        staging.database_identity,
    );
    let target = backup_entry_state(&staging.parent_fd, file_name, staging.database_identity);
    match (source, target) {
        (BackupEntryState::Missing, BackupEntryState::Owned) => BackupPublicationState::Committed,
        (BackupEntryState::Owned, BackupEntryState::Missing | BackupEntryState::Other) => {
            BackupPublicationState::NotCommitted
        }
        _ => BackupPublicationState::Uncertain,
    }
}

#[cfg(target_os = "linux")]
fn rustix_io_error(error: rustix::io::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(target_os = "linux")]
fn validate_private_backup_parent(parent: &Path) -> Result<(), SqlCipherStoreError> {
    use std::os::unix::fs::MetadataExt as _;

    let effective_uid = rustix::process::geteuid().as_raw();
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(SqlCipherStoreError::BackupTarget(
            "target parent must be owned by the current user with mode 0700".into(),
        ));
    }

    for ancestor in parent.ancestors().skip(1) {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        let owner_is_trusted = metadata.uid() == effective_uid || metadata.uid() == 0;
        let group_or_other_writable = metadata.mode() & 0o022 != 0;
        let protected_sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !owner_is_trusted
            || (group_or_other_writable && !protected_sticky_root)
        {
            return Err(SqlCipherStoreError::BackupTarget(
                "target parent has an unprotected ancestor".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn validate_private_backup_parent(_parent: &Path) -> Result<(), SqlCipherStoreError> {
    Err(SqlCipherStoreError::BackupTarget(
        "private backup staging is unavailable on this platform".into(),
    ))
}

#[cfg(target_os = "linux")]
fn open_backup_destination(
    path: &Path,
    key: &grok_application::DatabaseKey,
) -> Result<Connection, SqlCipherStoreError> {
    let connection = Connection::open(path)?;
    schema::apply_encryption_key(&connection, key)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode = MEMORY", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("memory") {
        return Err(SqlCipherStoreError::BackupTarget(
            "backup staging could not enter memory journal mode".into(),
        ));
    }
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         PRAGMA secure_delete = ON;
         PRAGMA cipher_memory_security = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(connection)
}

#[cfg(target_os = "linux")]
fn finalize_backup_destination(connection: Connection) -> Result<(), SqlCipherStoreError> {
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(SqlCipherStoreError::BackupTarget(
            "backup staging could not finalize delete journal mode".into(),
        ));
    }
    connection
        .close()
        .map_err(|(_, error)| SqlCipherStoreError::Sqlite(error))
}

#[cfg(target_os = "linux")]
fn verify_staged_snapshot_read_only(
    path: &Path,
    key: &grok_application::DatabaseKey,
) -> Result<(), SqlCipherStoreError> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    schema::apply_encryption_key(&connection, key)?;
    let journal_mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(SqlCipherStoreError::BackupTarget(
            "staged backup did not retain delete journal mode".into(),
        ));
    }
    let report = IntegrityReport {
        cipher_errors: collect_strings(&connection, "PRAGMA cipher_integrity_check")?,
        sqlite_results: collect_strings(&connection, "PRAGMA integrity_check")?,
    };
    if !report.is_healthy() {
        return Err(SqlCipherStoreError::BackupTarget(
            "staged backup failed integrity verification".into(),
        ));
    }
    connection
        .close()
        .map_err(|(_, error)| SqlCipherStoreError::Sqlite(error))
}

#[cfg(all(test, target_os = "linux"))]
fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(target_os = "linux")]
fn backup_identity(stat: &rustix::fs::Stat) -> BackupIdentity {
    BackupIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    }
}

#[cfg(target_os = "linux")]
fn open_private_backup_parent(
    parent: &Path,
) -> Result<(std::os::fd::OwnedFd, BackupIdentity), SqlCipherStoreError> {
    use rustix::fs::{Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let parent_fd = rustix::fs::open(parent, flags, Mode::empty()).map_err(rustix_io_error)?;
    let parent_stat = rustix::fs::fstat(&parent_fd).map_err(rustix_io_error)?;
    validate_private_directory_stat(&parent_stat, "target parent")?;
    Ok((parent_fd, backup_identity(&parent_stat)))
}

#[cfg(target_os = "linux")]
fn private_directory_open_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW
}

#[cfg(target_os = "linux")]
fn backup_staging_name() -> std::ffi::OsString {
    std::ffi::OsString::from(format!(".grok-backup-{}.staging", uuid::Uuid::new_v4()))
}

#[cfg(target_os = "linux")]
fn validate_private_directory_stat(
    stat: &rustix::fs::Stat,
    label: &str,
) -> Result<(), SqlCipherStoreError> {
    if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7777 != 0o700
    {
        return Err(SqlCipherStoreError::BackupTarget(format!(
            "{label} identity or permissions are unsafe"
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_private_database_stat(stat: &rustix::fs::Stat) -> Result<(), SqlCipherStoreError> {
    if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
    {
        return Err(SqlCipherStoreError::BackupTarget(
            "backup file identity or permissions are unsafe".into(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_entry_absent(
    directory_fd: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    reason: &str,
) -> Result<(), SqlCipherStoreError> {
    match rustix::fs::statat(directory_fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Ok(_) => Err(SqlCipherStoreError::BackupTarget(reason.into())),
        Err(error) => Err(rustix_io_error(error).into()),
    }
}

#[cfg(target_os = "linux")]
fn sidecar_name(file_name: &std::ffi::OsStr, suffix: &str) -> std::ffi::OsString {
    let mut name = file_name.to_os_string();
    name.push(suffix);
    name
}

#[cfg(target_os = "linux")]
fn ensure_target_and_sidecars_absent(
    parent_fd: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
) -> Result<(), SqlCipherStoreError> {
    ensure_entry_absent(parent_fd, file_name, "target already exists")?;
    for suffix in ["-wal", "-shm", "-journal"] {
        ensure_entry_absent(
            parent_fd,
            &sidecar_name(file_name, suffix),
            "target sidecar already exists",
        )?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_target_and_sidecars_absent_path(target: &Path) -> Result<(), SqlCipherStoreError> {
    use rustix::fs::{Mode, OFlags};

    let parent = target
        .parent()
        .ok_or_else(|| SqlCipherStoreError::BackupTarget("target has no parent".into()))?;
    let file_name = target.file_name().ok_or_else(|| {
        SqlCipherStoreError::BackupTarget("target must name one backup file".into())
    })?;
    let parent_fd = rustix::fs::open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(rustix_io_error)?;
    let parent_stat = rustix::fs::fstat(&parent_fd).map_err(rustix_io_error)?;
    validate_private_directory_stat(&parent_stat, "target parent")?;
    ensure_target_and_sidecars_absent(&parent_fd, file_name)
}

#[cfg(target_os = "linux")]
fn ensure_staging_sidecars_absent(
    directory_fd: &std::os::fd::OwnedFd,
) -> Result<(), SqlCipherStoreError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        ensure_entry_absent(
            directory_fd,
            &sidecar_name(std::ffi::OsStr::new(BACKUP_SNAPSHOT_NAME), suffix),
            "staged backup retained a sidecar",
        )?;
    }
    Ok(())
}

fn canonical_database_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute.exists() {
        return absolute.canonicalize();
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| std::io::Error::other("database path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    Ok(parent.canonicalize()?.join(
        absolute
            .file_name()
            .ok_or_else(|| std::io::Error::other("database path has no file name"))?,
    ))
}

fn collect_strings(
    connection: &Connection,
    statement: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = connection.prepare(statement)?;
    statement.query_map([], |row| row.get(0))?.collect()
}

pub(crate) fn number(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Internal("numeric value out of range".into()))
}

pub(crate) fn map_sqlite(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            StoreError::Conflict
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            StoreError::Unavailable("encrypted database is busy".into())
        }
        error => StoreError::Internal(error.to_string()),
    }
}

pub(crate) fn insert_run(connection: &Connection, run: &Run) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO runs(id, project_id, thread_id, state, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run.id.as_str(),
                run.project_id.as_str(),
                run.thread_id.as_str(),
                mapping::run_state_to_i64(run.state),
                number(run.revision)?,
                number(run.created_at)?,
                number(run.updated_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

pub(crate) fn update_run(
    connection: &Connection,
    run: &Run,
    expected_revision: u64,
) -> Result<(), StoreError> {
    let changed = connection
        .execute(
            "UPDATE runs SET state=?1, revision=?2, updated_at=?3
             WHERE id=?4 AND revision=?5",
            params![
                mapping::run_state_to_i64(run.state),
                number(run.revision)?,
                number(run.updated_at)?,
                run.id.as_str(),
                number(expected_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    if changed != 1 || run.revision != expected_revision.saturating_add(1) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn insert_approval(connection: &Connection, approval: &Approval) -> Result<(), StoreError> {
    let (scope, resource_id) = mapping::approval_scope_parts(&approval.scope);
    connection
        .execute(
            "INSERT INTO approvals(
                id, run_id, action, target, data_summary, risk, scope, resource_id,
                status, revision, created_at, expires_at, decided_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                approval.id.as_str(),
                approval.run_id.as_str(),
                approval.request.action,
                approval.request.target,
                approval.request.data_summary,
                mapping::approval_risk_to_i64(approval.request.risk),
                scope,
                resource_id,
                mapping::approval_status_to_i64(approval.status),
                number(approval.revision)?,
                number(approval.created_at)?,
                number(approval.expires_at)?,
                approval.decided_at.map(number).transpose()?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn update_approval(
    connection: &Connection,
    approval: &Approval,
    expected_revision: u64,
) -> Result<(), StoreError> {
    let changed = connection
        .execute(
            "UPDATE approvals SET status=?1, revision=?2, decided_at=?3
             WHERE id=?4 AND revision=?5",
            params![
                mapping::approval_status_to_i64(approval.status),
                number(approval.revision)?,
                approval.decided_at.map(number).transpose()?,
                approval.id.as_str(),
                number(expected_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    if changed != 1 || approval.revision != expected_revision.saturating_add(1) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

pub(crate) fn insert_effect(
    connection: &Connection,
    effect: &SideEffect,
) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO side_effects(
                id, run_id, kind, target, idempotency, state, revision, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                effect.id.as_str(),
                effect.run_id.as_str(),
                mapping::effect_kind_to_i64(effect.kind),
                effect.target,
                mapping::idempotency_to_i64(effect.idempotency),
                mapping::effect_state_to_i64(effect.state),
                number(effect.revision)?,
                number(effect.created_at)?,
                number(effect.updated_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

pub(crate) fn update_effect(
    connection: &Connection,
    effect: &SideEffect,
    expected_revision: u64,
) -> Result<(), StoreError> {
    let changed = connection
        .execute(
            "UPDATE side_effects SET state=?1, revision=?2, updated_at=?3
             WHERE id=?4 AND revision=?5",
            params![
                mapping::effect_state_to_i64(effect.state),
                number(effect.revision)?,
                number(effect.updated_at)?,
                effect.id.as_str(),
                number(expected_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    if changed != 1 || effect.revision != expected_revision.saturating_add(1) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn resolve_execution_command(
    connection: &Connection,
    command: &MutationCommand,
) -> Result<Option<ExecutionMutationOutcome>, StoreError> {
    let existing = connection
        .query_row(
            "SELECT request_fingerprint,outcome_json FROM execution_commands
             WHERE scope=?1 AND idempotency_key=?2",
            params![command.scope, command.key],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    match existing {
        Some((fingerprint, outcome)) if fingerprint == command.fingerprint => {
            decode_execution_outcome(&outcome).map(Some)
        }
        Some(_) => Err(StoreError::Conflict),
        None => Ok(None),
    }
}

fn record_execution_command(
    transaction: &Transaction<'_>,
    command: &MutationCommand,
    outcome: &ExecutionMutationOutcome,
) -> Result<(), StoreError> {
    let outcome = encode_execution_outcome(outcome)?;
    transaction
        .execute(
            "INSERT INTO execution_commands(
                scope,idempotency_key,request_fingerprint,outcome_json
             ) VALUES (?1,?2,?3,?4)",
            params![
                command.scope,
                command.key,
                command.fingerprint.as_slice(),
                outcome,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn encode_execution_outcome(outcome: &ExecutionMutationOutcome) -> Result<String, StoreError> {
    let stored = StoredExecutionOutcome {
        version: 1,
        result: match outcome {
            ExecutionMutationOutcome::Run(run) => StoredExecutionResult::Run {
                id: run.id.as_str().into(),
                project_id: run.project_id.as_str().into(),
                thread_id: run.thread_id.as_str().into(),
                state: run_state_name(run.state).into(),
                revision: run.revision,
                created_at: run.created_at,
                updated_at: run.updated_at,
            },
            ExecutionMutationOutcome::Approval(approval) => {
                let (scope, resource_id) = approval_scope_parts(&approval.scope);
                StoredExecutionResult::Approval {
                    id: approval.id.as_str().into(),
                    run_id: approval.run_id.as_str().into(),
                    action: approval.request.action.clone(),
                    target: approval.request.target.clone(),
                    data_summary: approval.request.data_summary.clone(),
                    risk: approval_risk_name(approval.request.risk).into(),
                    scope: scope.into(),
                    resource_id,
                    status: approval_status_name(approval.status).into(),
                    revision: approval.revision,
                    created_at: approval.created_at,
                    expires_at: approval.expires_at,
                    decided_at: approval.decided_at,
                }
            }
        },
    };
    let encoded = serde_json::to_string(&stored)
        .map_err(|_| StoreError::Internal("failed to encode execution command outcome".into()))?;
    if encoded.len() > MAX_EXECUTION_OUTCOME_BYTES {
        return Err(StoreError::Internal(
            "execution command outcome exceeded storage limit".into(),
        ));
    }
    Ok(encoded)
}

fn decode_execution_outcome(value: &str) -> Result<ExecutionMutationOutcome, StoreError> {
    if value.len() > MAX_EXECUTION_OUTCOME_BYTES {
        return Err(StoreError::Internal(
            "stored execution command outcome exceeded storage limit".into(),
        ));
    }
    let stored: StoredExecutionOutcome = serde_json::from_str(value)
        .map_err(|_| StoreError::Internal("stored execution command outcome is invalid".into()))?;
    if stored.version != 1 {
        return Err(StoreError::Internal(
            "stored execution command outcome version is unsupported".into(),
        ));
    }
    match stored.result {
        StoredExecutionResult::Run {
            id,
            project_id,
            thread_id,
            state,
            revision,
            created_at,
            updated_at,
        } => Ok(ExecutionMutationOutcome::Run(Run {
            id: RunId::new(id).map_err(|_| invalid_execution_outcome())?,
            project_id: ProjectId::new(project_id).map_err(|_| invalid_execution_outcome())?,
            thread_id: ThreadId::new(thread_id).map_err(|_| invalid_execution_outcome())?,
            state: parse_run_state(&state)?,
            revision,
            created_at,
            updated_at,
        })),
        StoredExecutionResult::Approval {
            id,
            run_id,
            action,
            target,
            data_summary,
            risk,
            scope,
            resource_id,
            status,
            revision,
            created_at,
            expires_at,
            decided_at,
        } => Ok(ExecutionMutationOutcome::Approval(Approval {
            id: ApprovalId::new(id).map_err(|_| invalid_execution_outcome())?,
            run_id: RunId::new(run_id).map_err(|_| invalid_execution_outcome())?,
            request: RequestedAction {
                action,
                target,
                data_summary,
                risk: parse_approval_risk(&risk)?,
            },
            scope: parse_approval_scope(&scope, resource_id)?,
            status: parse_approval_status(&status)?,
            revision,
            created_at,
            expires_at,
            decided_at,
        })),
    }
}

fn invalid_execution_outcome() -> StoreError {
    StoreError::Internal("stored execution command outcome is invalid".into())
}

const fn run_state_name(state: RunState) -> &'static str {
    match state {
        RunState::Queued => "queued",
        RunState::Planning => "planning",
        RunState::AwaitingApproval => "awaiting_approval",
        RunState::Running => "running",
        RunState::Paused => "paused",
        RunState::Completed => "completed",
        RunState::Failed => "failed",
        RunState::Cancelled => "cancelled",
        RunState::InterruptedNeedsReview => "interrupted_needs_review",
    }
}

fn parse_run_state(value: &str) -> Result<RunState, StoreError> {
    match value {
        "queued" => Ok(RunState::Queued),
        "planning" => Ok(RunState::Planning),
        "awaiting_approval" => Ok(RunState::AwaitingApproval),
        "running" => Ok(RunState::Running),
        "paused" => Ok(RunState::Paused),
        "completed" => Ok(RunState::Completed),
        "failed" => Ok(RunState::Failed),
        "cancelled" => Ok(RunState::Cancelled),
        "interrupted_needs_review" => Ok(RunState::InterruptedNeedsReview),
        _ => Err(invalid_execution_outcome()),
    }
}

const fn approval_risk_name(risk: ApprovalRisk) -> &'static str {
    match risk {
        ApprovalRisk::Low => "low",
        ApprovalRisk::Elevated => "elevated",
        ApprovalRisk::High => "high",
        ApprovalRisk::Critical => "critical",
    }
}

fn parse_approval_risk(value: &str) -> Result<ApprovalRisk, StoreError> {
    match value {
        "low" => Ok(ApprovalRisk::Low),
        "elevated" => Ok(ApprovalRisk::Elevated),
        "high" => Ok(ApprovalRisk::High),
        "critical" => Ok(ApprovalRisk::Critical),
        _ => Err(invalid_execution_outcome()),
    }
}

fn approval_scope_parts(scope: &ApprovalScope) -> (&'static str, Option<String>) {
    match scope {
        ApprovalScope::Once => ("once", None),
        ApprovalScope::Run => ("run", None),
        ApprovalScope::Resource(resource) => ("resource", Some(resource.clone())),
    }
}

fn parse_approval_scope(
    scope: &str,
    resource_id: Option<String>,
) -> Result<ApprovalScope, StoreError> {
    match (scope, resource_id) {
        ("once", None) => Ok(ApprovalScope::Once),
        ("run", None) => Ok(ApprovalScope::Run),
        ("resource", Some(resource)) if !resource.is_empty() => {
            Ok(ApprovalScope::Resource(resource))
        }
        _ => Err(invalid_execution_outcome()),
    }
}

const fn approval_status_name(status: ApprovalStatus) -> &'static str {
    match status {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Granted => "granted",
        ApprovalStatus::Denied => "denied",
        ApprovalStatus::Expired => "expired",
        ApprovalStatus::Cancelled => "cancelled",
    }
}

fn parse_approval_status(value: &str) -> Result<ApprovalStatus, StoreError> {
    match value {
        "pending" => Ok(ApprovalStatus::Pending),
        "granted" => Ok(ApprovalStatus::Granted),
        "denied" => Ok(ApprovalStatus::Denied),
        "expired" => Ok(ApprovalStatus::Expired),
        "cancelled" => Ok(ApprovalStatus::Cancelled),
        _ => Err(invalid_execution_outcome()),
    }
}

fn run_outcome(outcome: ExecutionMutationOutcome) -> Result<Run, StoreError> {
    match outcome {
        ExecutionMutationOutcome::Run(run) => Ok(run),
        ExecutionMutationOutcome::Approval(_) => Err(invalid_execution_outcome()),
    }
}

fn approval_outcome(outcome: ExecutionMutationOutcome) -> Result<Approval, StoreError> {
    match outcome {
        ExecutionMutationOutcome::Approval(approval) => Ok(approval),
        ExecutionMutationOutcome::Run(_) => Err(invalid_execution_outcome()),
    }
}

pub(crate) fn append_events(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    events: Vec<NewRunEvent>,
) -> Result<(), StoreError> {
    let mut sequence: u64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM run_events WHERE run_id=?1",
            [run_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite)?
        .try_into()
        .map_err(|_| StoreError::Internal("negative event sequence".into()))?;
    for event in events {
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| StoreError::Internal("event sequence exhausted".into()))?;
        let (kind, from_state, to_state, related_id) = mapping::event_parts(&event.kind);
        transaction
            .execute(
                "INSERT INTO run_events(
                    run_id, sequence, occurred_at, kind, from_state, to_state, related_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run_id.as_str(),
                    number(sequence)?,
                    number(event.occurred_at)?,
                    kind,
                    from_state,
                    to_state,
                    related_id,
                ],
            )
            .map_err(map_sqlite)?;
    }
    Ok(())
}

fn begin(connection: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)
}

fn commit(transaction: Transaction<'_>) -> Result<(), StoreError> {
    transaction.commit().map_err(map_sqlite)
}

#[async_trait]
impl ExecutionStore for SqlCipherStore {
    async fn resolve_execution_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ExecutionMutationOutcome>, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| resolve_execution_command(connection, &command))
            .await
    }

    async fn create_run(
        &self,
        run: Run,
        event: NewRunEvent,
        command: &MutationCommand,
    ) -> Result<Run, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(outcome) = resolve_execution_command(&transaction, &command)? {
                return run_outcome(outcome);
            }
            insert_run(&transaction, &run)?;
            append_events(&transaction, &run.id, vec![event])?;
            record_execution_command(
                &transaction,
                &command,
                &ExecutionMutationOutcome::Run(run.clone()),
            )?;
            commit(transaction)?;
            Ok(run)
        })
        .await
    }

    async fn get_run(&self, id: &RunId) -> Result<Run, StoreError> {
        let id = id.clone();
        self.with_store(move |connection| {
            connection
                .query_row(
                    &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id=?1"),
                    [id.as_str()],
                    mapping::run_from_row,
                )
                .map_err(map_sqlite)
        })
        .await
    }

    async fn save_run(
        &self,
        run: Run,
        expected_revision: u64,
        event: NewRunEvent,
        command: &MutationCommand,
    ) -> Result<Run, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(outcome) = resolve_execution_command(&transaction, &command)? {
                return run_outcome(outcome);
            }
            update_run(&transaction, &run, expected_revision)?;
            append_events(&transaction, &run.id, vec![event])?;
            record_execution_command(
                &transaction,
                &command,
                &ExecutionMutationOutcome::Run(run.clone()),
            )?;
            commit(transaction)?;
            Ok(run)
        })
        .await
    }

    async fn create_approval(
        &self,
        approval: Approval,
        run: Run,
        expected_run_revision: u64,
        events: Vec<NewRunEvent>,
        command: &MutationCommand,
    ) -> Result<Approval, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(outcome) = resolve_execution_command(&transaction, &command)? {
                return approval_outcome(outcome);
            }
            update_run(&transaction, &run, expected_run_revision)?;
            if approval.run_id != run.id {
                return Err(StoreError::Conflict);
            }
            insert_approval(&transaction, &approval)?;
            append_events(&transaction, &run.id, events)?;
            record_execution_command(
                &transaction,
                &command,
                &ExecutionMutationOutcome::Approval(approval.clone()),
            )?;
            commit(transaction)?;
            Ok(approval)
        })
        .await
    }

    async fn get_approval(&self, id: &ApprovalId) -> Result<Approval, StoreError> {
        let id = id.clone();
        self.with_store(move |connection| {
            connection
                .query_row(
                    &format!("SELECT {APPROVAL_COLUMNS} FROM approvals WHERE id=?1"),
                    [id.as_str()],
                    mapping::approval_from_row,
                )
                .map_err(map_sqlite)
        })
        .await
    }

    async fn decide_approval(
        &self,
        approval: Approval,
        expected_approval_revision: u64,
        run_update: Option<(Run, u64, NewRunEvent)>,
        command: &MutationCommand,
    ) -> Result<Approval, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(outcome) = resolve_execution_command(&transaction, &command)? {
                return approval_outcome(outcome);
            }
            update_approval(&transaction, &approval, expected_approval_revision)?;
            if let Some((run, expected_revision, event)) = run_update {
                if run.id != approval.run_id {
                    return Err(StoreError::Conflict);
                }
                update_run(&transaction, &run, expected_revision)?;
                append_events(&transaction, &run.id, vec![event])?;
            }
            record_execution_command(
                &transaction,
                &command,
                &ExecutionMutationOutcome::Approval(approval.clone()),
            )?;
            commit(transaction)?;
            Ok(approval)
        })
        .await
    }

    async fn create_effect(
        &self,
        effect: SideEffect,
        event: NewRunEvent,
    ) -> Result<(), StoreError> {
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            insert_effect(&transaction, &effect)?;
            append_events(&transaction, &effect.run_id, vec![event])?;
            commit(transaction)
        })
        .await
    }

    async fn get_effect(&self, id: &EffectId) -> Result<SideEffect, StoreError> {
        let id = id.clone();
        self.with_store(move |connection| {
            connection
                .query_row(
                    &format!("SELECT {EFFECT_COLUMNS} FROM side_effects WHERE id=?1"),
                    [id.as_str()],
                    mapping::effect_from_row,
                )
                .map_err(map_sqlite)
        })
        .await
    }

    async fn save_effect(
        &self,
        effect: SideEffect,
        expected_revision: u64,
    ) -> Result<(), StoreError> {
        self.with_store(move |connection| update_effect(connection, &effect, expected_revision))
            .await
    }

    async fn interrupt_effect(
        &self,
        effect: SideEffect,
        expected_effect_revision: u64,
        run: Run,
        expected_run_revision: u64,
        events: Vec<NewRunEvent>,
    ) -> Result<(), StoreError> {
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if effect.run_id != run.id {
                return Err(StoreError::Conflict);
            }
            update_effect(&transaction, &effect, expected_effect_revision)?;
            update_run(&transaction, &run, expected_run_revision)?;
            append_events(&transaction, &run.id, events)?;
            commit(transaction)
        })
        .await
    }

    async fn events_since(
        &self,
        run_id: &RunId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let run_id = run_id.clone();
        self.with_store(move |connection| {
            let exists: bool = connection
                .query_row("SELECT 1 FROM runs WHERE id=?1", [run_id.as_str()], |_| {
                    Ok(true)
                })
                .optional()
                .map_err(map_sqlite)?
                .unwrap_or(false);
            if !exists {
                return Err(StoreError::NotFound);
            }
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {EVENT_COLUMNS} FROM run_events
                     WHERE run_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3"
                ))
                .map_err(map_sqlite)?;
            statement
                .query_map(
                    params![
                        run_id.as_str(),
                        number(after_sequence)?,
                        i64::try_from(limit).unwrap_or(i64::MAX),
                    ],
                    mapping::event_from_row,
                )
                .map_err(map_sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite)
        })
        .await
    }
}

#[cfg(test)]
mod backup_tests {
    use std::sync::Arc;

    use grok_memory::EphemeralKeyProvider;

    use super::*;

    fn assert_no_staging(directory: &Path) {
        let staging = std::fs::read_dir(directory)
            .expect("read backup directory")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(".grok-backup-") && name.ends_with(".staging"))
            .collect::<Vec<_>>();
        assert!(staging.is_empty(), "staging entries remain: {staging:?}");
    }

    #[cfg(target_os = "linux")]
    fn find_staging(directory: &Path) -> PathBuf {
        std::fs::read_dir(directory)
            .expect("read backup directory")
            .filter_map(Result::ok)
            .find(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with(".grok-backup-") && name.ends_with(".staging")
                })
            })
            .expect("private staging directory")
            .path()
    }

    async fn store(directory: &Path) -> (SqlCipherStore, Arc<EphemeralKeyProvider>) {
        let key = Arc::new(EphemeralKeyProvider::new([211; 32]));
        let store = SqlCipherStore::open(directory.join("source.db"), key.clone())
            .await
            .expect("open source store");
        (store, key)
    }

    fn backup_directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("backup directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .expect("private backup directory");
        }
        directory
    }

    #[test]
    fn backup_platform_selector_fails_closed_outside_supported_unix() {
        assert_eq!(backup_publication_supported(), cfg!(target_os = "linux"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn post_file_staging_construction_failure_cleans_owned_entries() {
        let directory = backup_directory();
        let result = BackupStaging::create_with_post_file_failure(directory.path());

        assert!(matches!(
            result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "injected post-file creation failure"
        ));
        assert_no_staging(directory.path());
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn backup_fails_closed_where_atomic_no_replace_is_unsupported() {
        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;
        let result = store.backup_to(directory.path().join("backup.db")).await;

        match result {
            Err(SqlCipherStoreError::BackupTarget(reason)) => assert_eq!(
                reason,
                "atomic no-replace backup publication is unavailable on this platform"
            ),
            other => panic!("unexpected backup result: {other:?}"),
        }
        assert_no_staging(directory.path());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn target_parent_must_be_current_user_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
            .expect("make target parent non-private");

        let result = store.backup_to(directory.path().join("backup.db")).await;

        assert!(matches!(
            result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "target parent must be owned by the current user with mode 0700"
        ));
        assert_no_staging(directory.path());
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restore target parent permissions");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn target_parent_ancestors_must_be_protected() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;
        let unsafe_ancestor = directory.path().join("unsafe-ancestor");
        let private_parent = unsafe_ancestor.join("private-parent");
        std::fs::create_dir(&unsafe_ancestor).expect("unsafe ancestor");
        std::fs::set_permissions(&unsafe_ancestor, std::fs::Permissions::from_mode(0o777))
            .expect("make ancestor unsafe");
        std::fs::create_dir(&private_parent).expect("private target parent");
        std::fs::set_permissions(&private_parent, std::fs::Permissions::from_mode(0o700))
            .expect("make target parent private");

        let result = store.backup_to(private_parent.join("backup.db")).await;

        assert!(matches!(
            result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "target parent has an unprotected ancestor"
        ));
        assert_no_staging(&private_parent);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn existing_regular_and_symlink_targets_are_unchanged() {
        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;

        let regular = directory.path().join("existing.db");
        std::fs::write(&regular, b"existing-target").expect("existing target");
        assert!(matches!(
            store.backup_to(&regular).await,
            Err(SqlCipherStoreError::BackupTarget(_))
        ));
        assert_eq!(
            std::fs::read(&regular).expect("read existing target"),
            b"existing-target"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let outside = directory.path().join("outside.db");
            let symlink_target = directory.path().join("existing-link.db");
            std::fs::write(&outside, b"outside-target").expect("outside target");
            symlink(&outside, &symlink_target).expect("backup target symlink");
            assert!(matches!(
                store.backup_to(&symlink_target).await,
                Err(SqlCipherStoreError::BackupTarget(_))
            ));
            assert_eq!(
                std::fs::read_link(&symlink_target).expect("unchanged symlink"),
                outside
            );
            assert_eq!(
                std::fs::read(&outside).expect("unchanged outside target"),
                b"outside-target"
            );
        }
        assert_no_staging(directory.path());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn target_created_at_commit_wins_without_being_replaced() {
        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;
        let target = directory.path().join("raced.db");
        let result = store
            .backup_to_with_publish_hook(&target, |target| {
                std::fs::write(target, b"publication-racer")?;
                Ok(())
            })
            .await;

        assert!(matches!(result, Err(SqlCipherStoreError::BackupTarget(_))));
        assert_eq!(
            std::fs::read(&target).expect("racing target remains"),
            b"publication-racer"
        );
        assert_no_staging(directory.path());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn rename_error_is_reconciled_or_marked_uncertain() {
        let directory = backup_directory();
        let (store, key) = store(directory.path()).await;

        let committed_target = directory.path().join("committed-error.db");
        assert_eq!(
            store
                .backup_to_with_rename_fault(
                    &committed_target,
                    BackupRenameFault::CommitThenReportError,
                )
                .await
                .expect("reconcile committed rename"),
            committed_target
        );
        assert_no_staging(directory.path());
        let reopened = SqlCipherStore::open(&committed_target, key)
            .await
            .expect("reopen reconciled backup");
        assert!(
            reopened
                .verify_integrity()
                .await
                .expect("reconciled backup integrity")
                .is_healthy()
        );
        drop(reopened);

        let replayed_target = directory.path().join("committed-exist.db");
        assert_eq!(
            store
                .backup_to_with_rename_fault(
                    &replayed_target,
                    BackupRenameFault::CommitThenReportExist,
                )
                .await
                .expect("reconcile committed EEXIST rename"),
            replayed_target
        );
        assert!(replayed_target.is_file());
        assert_no_staging(directory.path());

        let uncertain_target = directory.path().join("uncertain-error.db");
        let result = store
            .backup_to_with_rename_fault(
                &uncertain_target,
                BackupRenameFault::LoseSourceThenReportError,
            )
            .await;
        assert!(matches!(
            result,
            Err(SqlCipherStoreError::BackupPublicationUncertain)
        ));
        assert!(std::fs::symlink_metadata(&uncertain_target).is_err());
        assert_no_staging(directory.path());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn destination_sidecars_block_initial_and_precommit_publication() {
        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;

        let initial_target = directory.path().join("initial-sidecar.db");
        let initial_sidecar = sidecar_path(&initial_target, "-wal");
        std::fs::write(&initial_sidecar, b"existing-sidecar").expect("initial sidecar");
        let initial_result = store.backup_to(&initial_target).await;
        assert!(matches!(
            initial_result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "target sidecar already exists"
        ));
        assert_eq!(
            std::fs::read(&initial_sidecar).expect("preserved initial sidecar"),
            b"existing-sidecar"
        );
        assert_no_staging(directory.path());

        let raced_target = directory.path().join("raced-sidecar.db");
        let raced_sidecar = sidecar_path(&raced_target, "-shm");
        let raced_sidecar_for_hook = raced_sidecar.clone();
        let raced_result = store
            .backup_to_with_publish_hook(&raced_target, move |_| {
                std::fs::write(&raced_sidecar_for_hook, b"racing-sidecar")?;
                Ok(())
            })
            .await;
        assert!(matches!(
            raced_result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "target sidecar already exists"
        ));
        assert_eq!(
            std::fs::read(&raced_sidecar).expect("preserved racing sidecar"),
            b"racing-sidecar"
        );
        assert!(std::fs::symlink_metadata(&raced_target).is_err());
        assert_no_staging(directory.path());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn parent_path_swap_is_detected_without_touching_replacement() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;
        let parent = directory.path().join("backup-parent");
        let relocated_parent = directory.path().join("relocated-parent");
        std::fs::create_dir(&parent).expect("private backup parent");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
            .expect("private backup parent mode");
        let target = parent.join("parent-swap.db");
        let parent_for_hook = parent.clone();
        let relocated_for_hook = relocated_parent.clone();

        let result = store
            .backup_to_with_publish_hook(&target, move |_| {
                std::fs::rename(&parent_for_hook, &relocated_for_hook)?;
                std::fs::create_dir(&parent_for_hook)?;
                std::fs::set_permissions(&parent_for_hook, std::fs::Permissions::from_mode(0o700))?;
                std::fs::write(parent_for_hook.join("replacement-marker"), b"replacement")?;
                Ok(())
            })
            .await;

        assert!(matches!(
            result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "target parent path identity changed"
        ));
        assert_eq!(
            std::fs::read(parent.join("replacement-marker")).expect("replacement marker"),
            b"replacement"
        );
        assert!(std::fs::symlink_metadata(&target).is_err());
        assert_no_staging(&relocated_parent);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn staging_directory_swap_is_detected_without_deleting_replacement() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;
        let target = directory.path().join("staging-swap.db");
        let parent = directory.path().to_path_buf();
        let parent_for_hook = parent.clone();
        let relocated = directory.path().join("relocated-staging");
        let relocated_for_hook = relocated.clone();

        let result = store
            .backup_to_with_publish_hook(&target, move |_| {
                let staging = find_staging(&parent_for_hook);
                std::fs::rename(&staging, &relocated_for_hook)?;
                std::fs::create_dir(&staging)?;
                std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
                std::fs::write(staging.join("replacement-marker"), b"replacement")?;
                Ok(())
            })
            .await;

        assert!(matches!(
            result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "backup staging path identity changed"
        ));
        let replacement = find_staging(&parent);
        assert_eq!(
            std::fs::read(replacement.join("replacement-marker"))
                .expect("replacement staging marker"),
            b"replacement"
        );
        assert!(std::fs::symlink_metadata(&target).is_err());
        assert!(
            relocated.is_dir(),
            "relocated held directory must remain identifiable"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn staged_snapshot_replacement_and_extra_link_are_detected() {
        use std::os::unix::fs::PermissionsExt as _;

        let replacement_directory = backup_directory();
        let (replacement_store, _key) = store(replacement_directory.path()).await;
        let replacement_target = replacement_directory.path().join("replacement.db");
        let replacement_parent = replacement_directory.path().to_path_buf();
        let replacement_parent_for_hook = replacement_parent.clone();
        let replacement_result = replacement_store
            .backup_to_with_publish_hook(&replacement_target, move |_| {
                let staging = find_staging(&replacement_parent_for_hook);
                let snapshot = staging.join(BACKUP_SNAPSHOT_NAME);
                std::fs::rename(&snapshot, staging.join("original-snapshot.db"))?;
                std::fs::write(&snapshot, b"replacement-snapshot")?;
                std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o600))?;
                Ok(())
            })
            .await;
        assert!(matches!(
            replacement_result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "backup snapshot path identity changed"
        ));
        let replacement_staging = find_staging(&replacement_parent);
        assert_eq!(
            std::fs::read(replacement_staging.join(BACKUP_SNAPSHOT_NAME))
                .expect("replacement snapshot"),
            b"replacement-snapshot"
        );
        assert!(std::fs::symlink_metadata(&replacement_target).is_err());

        let link_directory = backup_directory();
        let (link_store, _key) = store(link_directory.path()).await;
        let link_target = link_directory.path().join("extra-link.db");
        let link_parent = link_directory.path().to_path_buf();
        let link_parent_for_hook = link_parent.clone();
        let link_result = link_store
            .backup_to_with_publish_hook(&link_target, move |_| {
                let staging = find_staging(&link_parent_for_hook);
                std::fs::hard_link(
                    staging.join(BACKUP_SNAPSHOT_NAME),
                    staging.join("snapshot-alias.db"),
                )?;
                Ok(())
            })
            .await;
        assert!(matches!(
            link_result,
            Err(SqlCipherStoreError::BackupTarget(reason))
                if reason == "backup file identity or permissions are unsafe"
        ));
        let link_staging = find_staging(&link_parent);
        assert!(link_staging.join("snapshot-alias.db").is_file());
        assert!(std::fs::symlink_metadata(&link_target).is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn concurrent_publications_have_exactly_one_winner() {
        use std::sync::Barrier;

        let directory = backup_directory();
        let (store, key) = store(directory.path()).await;
        let target = directory.path().join("contended.db");
        let barrier = Arc::new(Barrier::new(2));

        let first_store = store.clone();
        let first_target = target.clone();
        let first_barrier = barrier.clone();
        let first = async move {
            first_store
                .backup_to_with_publish_hook(first_target, move |_| {
                    first_barrier.wait();
                    Ok(())
                })
                .await
        };
        let second_store = store.clone();
        let second_target = target.clone();
        let second = async move {
            second_store
                .backup_to_with_publish_hook(second_target, move |_| {
                    barrier.wait();
                    Ok(())
                })
                .await
        };
        let (first_result, second_result) = tokio::join!(first, second);

        assert_eq!(
            usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
            1
        );
        assert_eq!(
            usize::from(matches!(
                first_result,
                Err(SqlCipherStoreError::BackupTarget(_))
            )) + usize::from(matches!(
                second_result,
                Err(SqlCipherStoreError::BackupTarget(_))
            )),
            1
        );
        assert_no_staging(directory.path());
        let reopened = SqlCipherStore::open(&target, key)
            .await
            .expect("reopen winning backup");
        assert!(
            reopened
                .verify_integrity()
                .await
                .expect("winning backup integrity")
                .is_healthy()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn prepublication_failure_removes_private_staging_and_sidecars() {
        #[cfg(unix)]
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let directory = backup_directory();
        let (store, _key) = store(directory.path()).await;
        let target = directory.path().join("never-published.db");
        let parent = directory.path().to_path_buf();
        let result = store
            .backup_to_with_publish_hook(&target, move |_| {
                let staging = std::fs::read_dir(&parent)
                    .expect("read staging parent")
                    .filter_map(Result::ok)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| name.ends_with(".staging"))
                    })
                    .expect("private staging directory")
                    .path();
                #[cfg(unix)]
                {
                    let directory_metadata =
                        std::fs::symlink_metadata(&staging).expect("staging metadata");
                    assert_eq!(directory_metadata.permissions().mode() & 0o777, 0o700);
                    let database_metadata = std::fs::symlink_metadata(staging.join("snapshot.db"))
                        .expect("staged database metadata");
                    assert_eq!(database_metadata.permissions().mode() & 0o777, 0o600);
                    assert_eq!(database_metadata.nlink(), 1);
                }
                for suffix in ["-wal", "-shm", "-journal"] {
                    std::fs::write(sidecar_path(&staging.join("snapshot.db"), suffix), b"stale")?;
                }
                Err(SqlCipherStoreError::BackupTarget(
                    "injected prepublication failure".into(),
                ))
            })
            .await;

        assert!(matches!(result, Err(SqlCipherStoreError::BackupTarget(_))));
        assert!(std::fs::symlink_metadata(&target).is_err());
        assert_no_staging(directory.path());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn published_backup_is_private_single_link_clean_and_reopenable() {
        #[cfg(unix)]
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let directory = backup_directory();
        let (store, key) = store(directory.path()).await;
        let target = directory.path().join("published.db");
        assert_eq!(
            store.backup_to(&target).await.expect("publish backup"),
            target
        );
        assert_no_staging(directory.path());
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(
                std::fs::symlink_metadata(sidecar_path(&target, suffix)).is_err(),
                "published backup retained {suffix}"
            );
        }
        let raw = Connection::open_with_flags(&target, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open published backup read-only");
        let raw_key = key.database_key().expect("database key");
        schema::apply_encryption_key(&raw, &raw_key).expect("key published backup");
        let journal_mode: String = raw
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("published journal mode");
        assert_eq!(journal_mode.to_ascii_lowercase(), "delete");
        raw.close().expect("close raw backup connection");
        #[cfg(unix)]
        {
            let metadata = std::fs::symlink_metadata(&target).expect("backup metadata");
            assert!(metadata.is_file());
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.nlink(), 1);
        }

        let reopened = SqlCipherStore::open(&target, key)
            .await
            .expect("reopen encrypted backup");
        assert!(
            reopened
                .verify_integrity()
                .await
                .expect("backup integrity")
                .is_healthy()
        );
    }
}
