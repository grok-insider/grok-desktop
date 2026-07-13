//! Capability-rooted host filesystem adapter.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use grok_application::{
    HostDirectoryEntry, HostFilesystemError, HostFilesystemErrorKind, HostFilesystemReader,
    HostFilesystemWriter, HostProcessError, HostProcessErrorKind, HostProcessExecutor,
    HostProcessOutput, HostProcessRequest,
};
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Maximum directory entries returned by one call.
pub const MAX_DIRECTORY_ENTRIES: usize = 500;
/// Maximum UTF-8 file bytes returned by one call.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;
/// Hard maximum bytes returned when a caller explicitly raises the read cap.
pub const MAX_READ_HARD_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum UTF-8 bytes accepted by one atomic write.
pub const MAX_WRITE_BYTES: usize = 8 * 1024 * 1024;
/// Shared stdout and stderr retention cap.
pub const MAX_PROCESS_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum process duration accepted by the adapter.
pub const MAX_PROCESS_DURATION: Duration = Duration::from_mins(5);

struct CapabilityRoot {
    canonical: PathBuf,
    directory: Dir,
}

/// Filesystem reader confined beneath pre-opened enrolled directory capabilities.
#[derive(Clone)]
pub struct CapabilityHostFilesystem {
    roots: Arc<Vec<CapabilityRoot>>,
    denied_roots: Arc<Vec<PathBuf>>,
}

impl std::fmt::Debug for CapabilityHostFilesystem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityHostFilesystem")
            .field("root_count", &self.roots.len())
            .field("denied_root_count", &self.denied_roots.len())
            .finish()
    }
}

impl CapabilityHostFilesystem {
    /// Opens canonical enrolled roots as OS filesystem capabilities.
    ///
    /// # Errors
    ///
    /// Returns a path-free error if a root is relative, duplicated, missing, or
    /// cannot be opened as a directory capability.
    pub fn open(roots: &[String]) -> Result<Self, HostFilesystemError> {
        Self::open_with_denied_roots(roots, &[])
    }

    /// Opens enrolled roots while excluding daemon-private subtrees even when
    /// a broad parent such as the user's home directory was enrolled.
    ///
    /// # Errors
    ///
    /// Returns a path-free error when a denied root is relative, missing, or
    /// cannot be canonicalized.
    pub fn open_with_denied_roots(
        roots: &[String],
        denied_roots: &[String],
    ) -> Result<Self, HostFilesystemError> {
        if roots.is_empty() || roots.len() > 8 {
            return Err(invalid("invalid Host Tools root count"));
        }
        let mut opened = Vec::with_capacity(roots.len());
        for root in roots {
            let path = PathBuf::from(root);
            if !path.is_absolute()
                || opened
                    .iter()
                    .any(|item: &CapabilityRoot| item.canonical == path)
            {
                return Err(invalid("invalid Host Tools root"));
            }
            let directory = Dir::open_ambient_dir(&path, ambient_authority())
                .map_err(|_| unavailable("Host Tools root is unavailable"))?;
            opened.push(CapabilityRoot {
                canonical: path,
                directory,
            });
        }
        opened.sort_by(|left, right| {
            right
                .canonical
                .components()
                .count()
                .cmp(&left.canonical.components().count())
        });
        let mut denied = Vec::with_capacity(denied_roots.len());
        for value in denied_roots {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(invalid("invalid Host Tools denied root"));
            }
            let canonical = std::fs::canonicalize(path)
                .map_err(|_| unavailable("Host Tools denied root is unavailable"))?;
            if !canonical.is_dir() {
                return Err(invalid("invalid Host Tools denied root"));
            }
            if !denied.contains(&canonical) {
                denied.push(canonical);
            }
        }
        denied.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        Ok(Self {
            roots: Arc::new(opened),
            denied_roots: Arc::new(denied),
        })
    }

    fn reject_denied(&self, path: &Path) -> Result<(), HostFilesystemError> {
        if self
            .denied_roots
            .iter()
            .any(|denied| path.starts_with(denied))
        {
            return Err(denied("Host Tools path is daemon-private"));
        }
        if let Ok(canonical) = std::fs::canonicalize(path)
            && self
                .denied_roots
                .iter()
                .any(|denied| canonical.starts_with(denied))
        {
            return Err(denied("Host Tools path is daemon-private"));
        }
        Ok(())
    }

    fn resolve(&self, path: &Path) -> Result<(&Dir, PathBuf), HostFilesystemError> {
        if !path.is_absolute() {
            return Err(invalid("Host Tools path must be absolute"));
        }
        self.reject_denied(path)?;
        for root in self.roots.iter() {
            if let Ok(relative) = path.strip_prefix(&root.canonical) {
                let relative = if relative.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    relative.to_path_buf()
                };
                return Ok((&root.directory, relative));
            }
        }
        Err(denied("Host Tools path is outside enrolled roots"))
    }

    fn list_blocking(&self, path: &Path) -> Result<Vec<HostDirectoryEntry>, HostFilesystemError> {
        let (root, relative) = self.resolve(path)?;
        let entries = root
            .read_dir(&relative)
            .map_err(|_| unavailable("Host Tools directory is unavailable"))?;
        let mut result = Vec::new();
        for entry in entries.take(MAX_DIRECTORY_ENTRIES + 1) {
            if result.len() == MAX_DIRECTORY_ENTRIES {
                return Err(invalid("Host Tools directory exceeds the entry limit"));
            }
            let entry = entry.map_err(|_| unavailable("Host Tools directory changed"))?;
            let metadata = entry
                .metadata()
                .map_err(|_| unavailable("Host Tools directory entry is unavailable"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("Host Tools file name is not UTF-8"))?;
            if name.is_empty() || name.len() > 4096 {
                return Err(invalid("Host Tools file name is invalid"));
            }
            result.push(HostDirectoryEntry {
                name,
                is_directory: metadata.is_dir(),
                size: metadata.len(),
            });
        }
        result.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(result)
    }

    fn read_blocking(&self, path: &Path, max_bytes: u64) -> Result<Vec<u8>, HostFilesystemError> {
        if max_bytes == 0 || max_bytes > MAX_READ_HARD_BYTES {
            return Err(invalid("Host Tools read limit is invalid"));
        }
        let (root, relative) = self.resolve(path)?;
        let file = root
            .open(&relative)
            .map_err(|_| unavailable("Host Tools file is unavailable"))?;
        let metadata = file
            .metadata()
            .map_err(|_| unavailable("Host Tools file is unavailable"))?;
        if !metadata.is_file() || metadata.len() > max_bytes {
            return Err(invalid("Host Tools file exceeds the readable contract"));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        file.take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable("Host Tools file read failed"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(invalid("Host Tools file exceeds the readable contract"));
        }
        Ok(bytes)
    }

    fn write_blocking(&self, path: &Path, content: &[u8]) -> Result<(), HostFilesystemError> {
        if content.len() > MAX_WRITE_BYTES {
            return Err(invalid("Host Tools write exceeds the byte limit"));
        }
        let (root, relative) = self.resolve(path)?;
        let absolute_parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| invalid("Host Tools write target is invalid"))?;
        self.reject_denied(absolute_parent)?;
        let parent = relative
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = relative
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid("Host Tools write target is invalid"))?;
        let directory = root
            .open_dir(parent)
            .map_err(|_| denied("Host Tools write parent is unavailable"))?;
        let temporary = format!(".grok-write-{}", uuid::Uuid::new_v4());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = directory
            .open_with(&temporary, &options)
            .map_err(|_| unavailable("Host Tools temporary file is unavailable"))?;
        let result = (|| {
            file.write_all(content)
                .map_err(|_| unavailable("Host Tools file write failed"))?;
            file.sync_all()
                .map_err(|_| unavailable("Host Tools file sync failed"))?;
            directory
                .rename(&temporary, &directory, name)
                .map_err(|_| unavailable("Host Tools atomic replace failed"))?;
            directory
                .dir_metadata()
                .map_err(|_| unavailable("Host Tools write parent changed"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = directory.remove_file(&temporary);
        }
        result
    }

    fn canonical_directory(&self, path: &Path) -> Result<PathBuf, HostProcessError> {
        self.resolve(path).map_err(filesystem_process_error)?;
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| process_unavailable("Host process working directory is unavailable"))?;
        let (root, relative) = self.resolve(&canonical).map_err(filesystem_process_error)?;
        root.open_dir(relative)
            .map_err(|_| process_denied("Host process working directory is unavailable"))?;
        Ok(canonical)
    }
}

#[async_trait]
impl HostFilesystemReader for CapabilityHostFilesystem {
    async fn list(&self, path: &Path) -> Result<Vec<HostDirectoryEntry>, HostFilesystemError> {
        let this = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || this.list_blocking(&path))
            .await
            .map_err(|_| unavailable("Host Tools filesystem task failed"))?
    }

    async fn read_text(&self, path: &Path) -> Result<String, HostFilesystemError> {
        let this = self.clone();
        let path = path.to_path_buf();
        let bytes = tokio::task::spawn_blocking(move || this.read_blocking(&path, MAX_READ_BYTES))
            .await
            .map_err(|_| unavailable("Host Tools filesystem task failed"))??;
        String::from_utf8(bytes).map_err(|_| invalid("Host Tools file is not UTF-8"))
    }

    async fn read_bytes(
        &self,
        path: &Path,
        max_bytes: u64,
    ) -> Result<Vec<u8>, HostFilesystemError> {
        let this = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || this.read_blocking(&path, max_bytes))
            .await
            .map_err(|_| unavailable("Host Tools filesystem task failed"))?
    }
}

#[async_trait]
impl HostFilesystemWriter for CapabilityHostFilesystem {
    async fn write_text(&self, path: &Path, content: String) -> Result<(), HostFilesystemError> {
        let this = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || this.write_blocking(&path, content.as_bytes()))
            .await
            .map_err(|_| unavailable("Host Tools filesystem task failed"))?
    }
}

#[async_trait]
impl HostProcessExecutor for CapabilityHostFilesystem {
    async fn validate(
        &self,
        mut request: HostProcessRequest,
    ) -> Result<HostProcessRequest, HostProcessError> {
        validate_process_request(&request)?;
        request.cwd = self
            .canonical_directory(Path::new(&request.cwd))?
            .to_string_lossy()
            .into_owned();
        request.argv[0] = resolve_executable(&request.argv[0])?
            .to_string_lossy()
            .into_owned();
        Ok(request)
    }

    async fn execute(
        &self,
        request: HostProcessRequest,
        cancellation: CancellationToken,
    ) -> Result<HostProcessOutput, HostProcessError> {
        let request = self.validate(request).await?;
        let mut command = CommandWrap::with_new(&request.argv[0], |command| {
            command
                .args(&request.argv[1..])
                .current_dir(&request.cwd)
                .env_clear()
                .env("PATH", platform_path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(false);
            add_safe_locale_environment(command);
            add_safe_user_environment(command);
        });
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(process_wrap::tokio::JobObject);
        let mut child = command
            .spawn()
            .map_err(|_| process_unavailable("Host process could not be started"))?;
        let pid = child.id();
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| process_unavailable("Host process stdout is unavailable"))?;
        let stderr = child
            .stderr()
            .take()
            .ok_or_else(|| process_unavailable("Host process stderr is unavailable"))?;
        let retained = Arc::new(tokio::sync::Mutex::new(RetainedOutput::default()));
        let stdout_task = tokio::spawn(drain_output(stdout, retained.clone(), true));
        let stderr_task = tokio::spawn(drain_output(stderr, retained.clone(), false));
        let terminal = tokio::select! {
            status = child.wait() => ProcessTerminal::Exited(status),
            () = cancellation.cancelled() => ProcessTerminal::Cancelled,
            () = tokio::time::sleep(request.timeout) => ProcessTerminal::TimedOut,
        };
        let status = match terminal {
            ProcessTerminal::Exited(status) => {
                status.map_err(|_| process_unavailable("Host process wait failed"))?
            }
            ProcessTerminal::Cancelled => {
                kill_process_tree(pid, child.as_mut());
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(process_interrupted("Host process was cancelled"));
            }
            ProcessTerminal::TimedOut => {
                kill_process_tree(pid, child.as_mut());
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(process_timed_out("Host process exceeded its time limit"));
            }
        };
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        let retained = Arc::try_unwrap(retained)
            .map_err(|_| process_unavailable("Host process output task did not finish"))?
            .into_inner();
        Ok(HostProcessOutput {
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&retained.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&retained.stderr).into_owned(),
            truncated: retained.truncated,
        })
    }
}

enum ProcessTerminal {
    Exited(std::io::Result<std::process::ExitStatus>),
    Cancelled,
    TimedOut,
}

#[derive(Default)]
struct RetainedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

async fn drain_output<R>(
    mut reader: R,
    retained: Arc<tokio::sync::Mutex<RetainedOutput>>,
    stdout: bool,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 8192];
    loop {
        let Ok(read) = reader.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        let mut output = retained.lock().await;
        let used = output.stdout.len().saturating_add(output.stderr.len());
        let available = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(used);
        let retain = available.min(read);
        if stdout {
            output.stdout.extend_from_slice(&buffer[..retain]);
        } else {
            output.stderr.extend_from_slice(&buffer[..retain]);
        }
        output.truncated |= retain < read;
    }
}

fn validate_process_request(request: &HostProcessRequest) -> Result<(), HostProcessError> {
    let total = request.argv.iter().map(String::len).sum::<usize>();
    if request.argv.is_empty()
        || request.argv.len() > 64
        || request
            .argv
            .iter()
            .any(|argument| argument.len() > 8 * 1024)
        || total > 64 * 1024
        || request.cwd.is_empty()
        || request.cwd.len() > 4096
        || request.timeout.is_zero()
        || request.timeout > MAX_PROCESS_DURATION
    {
        return Err(process_invalid("Host process request exceeds its bounds"));
    }
    Ok(())
}

fn resolve_executable(value: &str) -> Result<PathBuf, HostProcessError> {
    let candidate = Path::new(value);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if candidate.components().count() == 1 {
        platform_search_directories()
            .into_iter()
            .map(|directory| directory.join(candidate))
            .find(|path| path.is_file())
            .ok_or_else(|| process_invalid("Host process executable was not found"))?
    } else {
        return Err(process_invalid(
            "Host process executable must be absolute or a bare name",
        ));
    };
    std::fs::canonicalize(&resolved)
        .map_err(|_| process_invalid("Host process executable is unavailable"))?;
    Ok(resolved)
}

#[cfg(unix)]
fn platform_search_directories() -> Vec<PathBuf> {
    let mut directories = vec![
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(
            std::env::split_paths(&path)
                .filter(|directory| directory.is_absolute() && directory.is_dir()),
        );
    }
    directories.sort();
    directories.dedup();
    directories
}

#[cfg(windows)]
fn platform_search_directories() -> Vec<PathBuf> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    vec![PathBuf::from(root).join("System32")]
}

#[cfg(not(any(unix, windows)))]
fn platform_search_directories() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(unix)]
fn platform_path() -> std::ffi::OsString {
    std::env::join_paths(platform_search_directories()).unwrap_or_default()
}

#[cfg(windows)]
fn platform_path() -> std::ffi::OsString {
    platform_search_directories()
        .first()
        .map_or_else(std::ffi::OsString::new, |path| path.as_os_str().to_owned())
}

#[cfg(not(any(unix, windows)))]
const fn platform_path() -> &'static str {
    ""
}

fn add_safe_locale_environment(command: &mut tokio::process::Command) {
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if (name == "LANG" || name.starts_with("LC_"))
            && value.len() <= 256
            && value
                .to_string_lossy()
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_.@-".contains(character))
        {
            command.env(name.as_ref(), value);
        }
    }
}

fn add_safe_user_environment(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    for name in ["HOME", "USER"] {
        if let Some(value) = std::env::var_os(name).filter(|value| value.len() <= 4096) {
            command.env(name, value);
        }
    }
    #[cfg(windows)]
    for name in ["USERPROFILE", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name).filter(|value| value.len() <= 4096) {
            command.env(name, value);
        }
    }
}

#[cfg(unix)]
fn kill_process_tree(pid: Option<u32>, child: &mut dyn ChildWrapper) {
    if let Some(pid) = pid
        .and_then(|value| i32::try_from(value).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    } else {
        let _ = child.start_kill();
    }
}

#[cfg(not(unix))]
fn kill_process_tree(_pid: Option<u32>, child: &mut dyn ChildWrapper) {
    let _ = child.start_kill();
}

fn filesystem_process_error(error: HostFilesystemError) -> HostProcessError {
    HostProcessError {
        kind: match error.kind {
            HostFilesystemErrorKind::Denied => HostProcessErrorKind::Denied,
            HostFilesystemErrorKind::Invalid => HostProcessErrorKind::Invalid,
            HostFilesystemErrorKind::Unavailable => HostProcessErrorKind::Unavailable,
        },
        message: error.message,
    }
}

fn process_invalid(message: &str) -> HostProcessError {
    HostProcessError {
        kind: HostProcessErrorKind::Invalid,
        message: message.into(),
    }
}

fn process_denied(message: &str) -> HostProcessError {
    HostProcessError {
        kind: HostProcessErrorKind::Denied,
        message: message.into(),
    }
}

fn process_unavailable(message: &str) -> HostProcessError {
    HostProcessError {
        kind: HostProcessErrorKind::Unavailable,
        message: message.into(),
    }
}

fn process_timed_out(message: &str) -> HostProcessError {
    HostProcessError {
        kind: HostProcessErrorKind::TimedOut,
        message: message.into(),
    }
}

fn process_interrupted(message: &str) -> HostProcessError {
    HostProcessError {
        kind: HostProcessErrorKind::Interrupted,
        message: message.into(),
    }
}

fn denied(message: &str) -> HostFilesystemError {
    HostFilesystemError {
        kind: HostFilesystemErrorKind::Denied,
        message: message.into(),
    }
}

fn invalid(message: &str) -> HostFilesystemError {
    HostFilesystemError {
        kind: HostFilesystemErrorKind::Invalid,
        message: message.into(),
    }
}

fn unavailable(message: &str) -> HostFilesystemError {
    HostFilesystemError {
        kind: HostFilesystemErrorKind::Unavailable,
        message: message.into(),
    }
}
