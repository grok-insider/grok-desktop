use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use same_file::Handle;
use semver::Version;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) const MAX_COMPONENT_BYTES: u64 = 1024 * 1024 * 1024;

/// Legacy caller-asserted descriptor retained for daemon migration and tests.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalGrokComponent {
    /// Absolute executable path selected by the component manager.
    pub executable: PathBuf,
    /// Exact semantic version from the verified component manifest.
    pub version: String,
    /// Lower- or uppercase hexadecimal SHA-256 digest from that manifest.
    pub sha256: String,
    /// Manifest publisher, required to be `xAI`.
    pub publisher: String,
}

/// Verified immutable spawn configuration for the external CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGrokComponent {
    executable: PathBuf,
    version: Version,
    digest: [u8; 32],
    size: u64,
    identity: Arc<Handle>,
    managed_location: Option<ManagedLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedLocation {
    install_root: PathBuf,
    relative_executable: String,
}

impl VerifiedGrokComponent {
    /// Verifies a legacy descriptor used by development and contract fixtures.
    ///
    /// Production component discovery must use the signed official catalog
    /// verifier. This compatibility API remains while daemon composition moves
    /// to the catalog boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ComponentVerificationError`] for any missing or mismatched
    /// component property. The executable is never invoked during verification.
    #[doc(hidden)]
    pub fn verify(descriptor: &ExternalGrokComponent) -> Result<Self, ComponentVerificationError> {
        if descriptor.publisher != "xAI" {
            return Err(ComponentVerificationError::WrongPublisher);
        }
        if !descriptor.executable.is_absolute() {
            return Err(ComponentVerificationError::PathNotAbsolute);
        }
        let executable = descriptor
            .executable
            .canonicalize()
            .map_err(ComponentVerificationError::Io)?;
        verify_filename(&executable)?;
        let version = Version::parse(&descriptor.version)
            .map_err(|_| ComponentVerificationError::InvalidVersion)?;
        let expected = decode_digest(&descriptor.sha256)?;
        let (identity, size) = open_and_verify(&executable, None, expected)?;
        Ok(Self {
            executable,
            version,
            digest: expected,
            size,
            identity,
            managed_location: None,
        })
    }

    pub(crate) fn from_managed_manifest(
        executable: PathBuf,
        install_root: PathBuf,
        relative_executable: String,
        version: Version,
        digest: [u8; 32],
        size: u64,
    ) -> Result<Self, ComponentVerificationError> {
        let (identity, actual_size) = open_and_verify(&executable, Some(size), digest)?;
        Ok(Self {
            executable,
            version,
            digest,
            size: actual_size,
            identity,
            managed_location: Some(ManagedLocation {
                install_root,
                relative_executable,
            }),
        })
    }

    /// Canonical path passed directly to the OS process API.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Manifest semantic version.
    #[must_use]
    pub const fn version(&self) -> &Version {
        &self.version
    }

    /// Rechecks file identity and digest immediately before every spawn.
    ///
    /// # Errors
    ///
    /// Returns [`ComponentVerificationError`] if the component changed after
    /// initial verification.
    pub fn reverify(&self) -> Result<(), ComponentVerificationError> {
        if let Some(location) = &self.managed_location {
            let resolved = crate::catalog::resolve_install_relative(
                &location.install_root,
                &location.relative_executable,
            )?;
            if resolved != self.executable {
                return Err(ComponentVerificationError::IntegrityMismatch);
            }
        }
        let canonical = self
            .executable
            .canonicalize()
            .map_err(ComponentVerificationError::Io)?;
        if canonical != self.executable {
            return Err(ComponentVerificationError::IntegrityMismatch);
        }
        let (identity, size) = open_and_verify(&canonical, Some(self.size), self.digest).map_err(
            |error| match error {
                ComponentVerificationError::Io(error) => ComponentVerificationError::Io(error),
                _ => ComponentVerificationError::IntegrityMismatch,
            },
        )?;
        if identity.as_ref() != self.identity.as_ref() || size != self.size {
            return Err(ComponentVerificationError::IntegrityMismatch);
        }
        Ok(())
    }
}

/// External component verification failure without exposing file contents.
#[derive(Debug, Error)]
pub enum ComponentVerificationError {
    /// Descriptor does not claim the official publisher.
    #[error("component publisher is not xAI")]
    WrongPublisher,
    /// Relative executable paths are never accepted.
    #[error("component executable path must be absolute")]
    PathNotAbsolute,
    /// Executable basename is not exactly `grok` or `grok.exe`.
    #[error("component executable is not the official grok command name")]
    WrongExecutableName,
    /// Component is empty, not a regular file, or exceeds the size bound.
    #[error("component executable is not a bounded regular file")]
    InvalidFile,
    /// Catalog path is not a canonical install-relative path.
    #[error("component executable install path is invalid")]
    InvalidInstallPath,
    /// Catalog path resolves through a symbolic link or outside its root.
    #[error("component executable path crosses an untrusted link")]
    SymlinkEscape,
    /// Signed component size differs from the opened executable.
    #[error("component executable size does not match the catalog")]
    SizeMismatch,
    /// Unix executable bits are absent.
    #[error("component file is not executable")]
    NotExecutable,
    /// Manifest version is not semantic version syntax.
    #[error("component version is invalid")]
    InvalidVersion,
    /// Manifest digest is not exactly 32 hexadecimal bytes.
    #[error("component SHA-256 digest is invalid")]
    InvalidDigest,
    /// Executable bytes differ from the verified manifest.
    #[error("component integrity check failed")]
    IntegrityMismatch,
    /// Filesystem operation failed.
    #[error("component filesystem operation failed: {0}")]
    Io(std::io::Error),
}

fn verify_filename(path: &Path) -> Result<(), ComponentVerificationError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name != "grok" && name != "grok.exe" {
        return Err(ComponentVerificationError::WrongExecutableName);
    }
    Ok(())
}

#[cfg(unix)]
fn verify_executable_mode(metadata: &std::fs::Metadata) -> Result<(), ComponentVerificationError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(ComponentVerificationError::NotExecutable);
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep one fallible verifier contract across platforms.
fn verify_executable_mode(_metadata: &std::fs::Metadata) -> Result<(), ComponentVerificationError> {
    Ok(())
}

fn decode_digest(value: &str) -> Result<[u8; 32], ComponentVerificationError> {
    let decoded = hex::decode(value).map_err(|_| ComponentVerificationError::InvalidDigest)?;
    decoded
        .try_into()
        .map_err(|_| ComponentVerificationError::InvalidDigest)
}

fn open_and_verify(
    path: &Path,
    expected_size: Option<u64>,
    expected_digest: [u8; 32],
) -> Result<(Arc<Handle>, u64), ComponentVerificationError> {
    let identity = Arc::new(Handle::from_path(path).map_err(ComponentVerificationError::Io)?);
    let metadata = identity
        .as_file()
        .metadata()
        .map_err(ComponentVerificationError::Io)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_COMPONENT_BYTES {
        return Err(ComponentVerificationError::InvalidFile);
    }
    if expected_size.is_some_and(|expected| expected != metadata.len()) {
        return Err(ComponentVerificationError::SizeMismatch);
    }
    verify_executable_mode(&metadata)?;
    if hash_handle(identity.as_file(), metadata.len())? != expected_digest {
        return Err(ComponentVerificationError::IntegrityMismatch);
    }
    let after = identity
        .as_file()
        .metadata()
        .map_err(ComponentVerificationError::Io)?;
    if after.len() != metadata.len() || !after.is_file() {
        return Err(ComponentVerificationError::IntegrityMismatch);
    }
    Ok((identity, metadata.len()))
}

fn hash_handle(file: &File, expected_size: u64) -> Result<[u8; 32], ComponentVerificationError> {
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(0))
        .map_err(ComponentVerificationError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024].into_boxed_slice();
    let mut total = 0_u64;
    let mut limited = reader.take(expected_size.saturating_add(1));
    loop {
        let read = limited
            .read(&mut buffer)
            .map_err(ComponentVerificationError::Io)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    if total != expected_size {
        return Err(ComponentVerificationError::IntegrityMismatch);
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn verifies_named_external_component_and_detects_replacement() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory
            .path()
            .join(if cfg!(windows) { "grok.exe" } else { "grok" });
        let mut file = File::create(&path).expect("create");
        file.write_all(b"verified executable").expect("write");
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("permissions");
        }
        let digest = hex::encode(Sha256::digest(b"verified executable"));
        let component = VerifiedGrokComponent::verify(&ExternalGrokComponent {
            executable: path.clone(),
            version: "0.2.95".into(),
            sha256: digest,
            publisher: "xAI".into(),
        })
        .expect("verify");
        assert_eq!(component.version(), &Version::new(0, 2, 95));
        std::fs::write(path, b"replacement").expect("replace");
        assert!(matches!(
            component.reverify(),
            Err(ComponentVerificationError::IntegrityMismatch)
        ));
    }
}
