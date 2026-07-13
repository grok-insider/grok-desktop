//! Capability-rooted host filesystem adapter.

use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use cap_std::{ambient_authority, fs::Dir};
use grok_application::{
    HostDirectoryEntry, HostFilesystemError, HostFilesystemErrorKind, HostFilesystemReader,
};

/// Maximum directory entries returned by one call.
pub const MAX_DIRECTORY_ENTRIES: usize = 1_000;
/// Maximum UTF-8 file bytes returned by one call.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

struct CapabilityRoot {
    canonical: PathBuf,
    directory: Dir,
}

/// Filesystem reader confined beneath pre-opened enrolled directory capabilities.
#[derive(Clone)]
pub struct CapabilityHostFilesystem {
    roots: Arc<Vec<CapabilityRoot>>,
}

impl std::fmt::Debug for CapabilityHostFilesystem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityHostFilesystem")
            .field("root_count", &self.roots.len())
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
        Ok(Self {
            roots: Arc::new(opened),
        })
    }

    fn resolve(&self, path: &Path) -> Result<(&Dir, PathBuf), HostFilesystemError> {
        if !path.is_absolute() {
            return Err(invalid("Host Tools path must be absolute"));
        }
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

    fn read_blocking(&self, path: &Path) -> Result<String, HostFilesystemError> {
        let (root, relative) = self.resolve(path)?;
        let file = root
            .open(&relative)
            .map_err(|_| unavailable("Host Tools file is unavailable"))?;
        let metadata = file
            .metadata()
            .map_err(|_| unavailable("Host Tools file is unavailable"))?;
        if !metadata.is_file() || metadata.len() > MAX_READ_BYTES {
            return Err(invalid("Host Tools file exceeds the readable contract"));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        file.take(MAX_READ_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable("Host Tools file read failed"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_READ_BYTES {
            return Err(invalid("Host Tools file exceeds the readable contract"));
        }
        String::from_utf8(bytes).map_err(|_| invalid("Host Tools file is not UTF-8"))
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
        tokio::task::spawn_blocking(move || this.read_blocking(&path))
            .await
            .map_err(|_| unavailable("Host Tools filesystem task failed"))?
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
