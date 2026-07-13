use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    catalog::{
        Architecture, ComponentPlatform, OperatingSystem, decode_catalog_digest,
        ensure_strict_json, resolve_install_relative, validate_relative_path,
    },
    component::{ComponentVerificationError, MAX_COMPONENT_BYTES, VerifiedGrokComponent},
};

const MANIFEST_SCHEMA: &str = "grok.official-component-pin/v1";
const BINDING_PREFIX: &str = "grok-acp-pinned-manifest-v1:";
const OFFICIAL_COMPONENT_NAME: &str = "grok-build";
const OFFICIAL_PUBLISHER: &str = "xAI";
const MAX_VERSION_BYTES: usize = 128;
const MAX_SOURCE_URL_BYTES: usize = 512;

/// Maximum source-pinned manifest accepted at the package boundary.
pub const MAX_PINNED_COMPONENT_MANIFEST_BYTES: usize = 8 * 1024;

/// Verifies an immutable official component manifest bound into the daemon.
#[derive(Debug)]
pub struct OfficialGrokPinnedComponentVerifier {
    install_root: PathBuf,
    platform: ComponentPlatform,
    expected_binding: String,
}

impl OfficialGrokPinnedComponentVerifier {
    /// Creates a verifier for the current target and a compile-time manifest binding.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid install root, target, or binding syntax.
    pub fn new(
        install_root: impl AsRef<Path>,
        expected_binding: impl Into<String>,
    ) -> Result<Self, PinnedComponentVerificationError> {
        Self::new_for_platform(
            install_root.as_ref(),
            expected_binding.into(),
            ComponentPlatform::current()
                .map_err(|_| PinnedComponentVerificationError::UnsupportedPlatform)?,
        )
    }

    fn new_for_platform(
        install_root: &Path,
        expected_binding: String,
        platform: ComponentPlatform,
    ) -> Result<Self, PinnedComponentVerificationError> {
        if !install_root.is_absolute() {
            return Err(PinnedComponentVerificationError::InvalidInstallRoot);
        }
        let metadata = std::fs::symlink_metadata(install_root)
            .map_err(PinnedComponentVerificationError::Io)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PinnedComponentVerificationError::InvalidInstallRoot);
        }
        let install_root = install_root
            .canonicalize()
            .map_err(PinnedComponentVerificationError::Io)?;
        if !valid_binding(&expected_binding) {
            return Err(PinnedComponentVerificationError::InvalidBinding);
        }
        Ok(Self {
            install_root,
            platform,
            expected_binding,
        })
    }

    /// Verifies the exact manifest bytes, official source, target, and local executable.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest differs from the daemon binding or any
    /// manifest, platform, path, size, or digest property is invalid.
    pub fn verify(
        &self,
        manifest: &[u8],
    ) -> Result<VerifiedGrokComponent, PinnedComponentVerificationError> {
        ensure_strict_json(manifest, MAX_PINNED_COMPONENT_MANIFEST_BYTES)
            .map_err(|_| PinnedComponentVerificationError::InvalidJson)?;
        if manifest_binding(manifest) != self.expected_binding {
            return Err(PinnedComponentVerificationError::BindingMismatch);
        }
        let record: PinnedComponentManifest = serde_json::from_slice(manifest)
            .map_err(|_| PinnedComponentVerificationError::InvalidManifest)?;
        if record.schema != MANIFEST_SCHEMA
            || record.name != OFFICIAL_COMPONENT_NAME
            || record.publisher != OFFICIAL_PUBLISHER
        {
            return Err(PinnedComponentVerificationError::InvalidManifest);
        }
        if record.version.is_empty() || record.version.len() > MAX_VERSION_BYTES {
            return Err(PinnedComponentVerificationError::InvalidVersion);
        }
        let version = Version::parse(&record.version)
            .map_err(|_| PinnedComponentVerificationError::InvalidVersion)?;
        let platform = ComponentPlatform {
            operating_system: OperatingSystem::parse(&record.os)
                .map_err(|_| PinnedComponentVerificationError::WrongPlatform)?,
            architecture: Architecture::parse(&record.architecture)
                .map_err(|_| PinnedComponentVerificationError::WrongPlatform)?,
        };
        if platform != self.platform {
            return Err(PinnedComponentVerificationError::WrongPlatform);
        }
        validate_relative_path(
            &record.executable,
            platform.operating_system.executable_name(),
        )
        .map_err(|_| PinnedComponentVerificationError::InvalidExecutablePath)?;
        validate_source_url(&record.source_url, &record.version, platform)?;
        if record.size == 0 || record.size > MAX_COMPONENT_BYTES {
            return Err(PinnedComponentVerificationError::InvalidSize);
        }
        let digest = decode_catalog_digest(&record.sha256)
            .map_err(|_| PinnedComponentVerificationError::InvalidDigest)?;
        let executable = resolve_install_relative(&self.install_root, &record.executable)?;
        VerifiedGrokComponent::from_managed_manifest(
            executable,
            self.install_root.clone(),
            record.executable,
            version,
            digest,
            record.size,
        )
        .map_err(PinnedComponentVerificationError::Component)
    }
}

/// Computes the build binding for exact source-pinned manifest bytes.
#[must_use]
pub fn manifest_binding(manifest: &[u8]) -> String {
    format!("{BINDING_PREFIX}{}", hex::encode(Sha256::digest(manifest)))
}

fn valid_binding(binding: &str) -> bool {
    binding.len() == BINDING_PREFIX.len() + 64
        && binding.starts_with(BINDING_PREFIX)
        && binding[BINDING_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn validate_source_url(
    value: &str,
    version: &str,
    platform: ComponentPlatform,
) -> Result<(), PinnedComponentVerificationError> {
    if value.is_empty() || value.len() > MAX_SOURCE_URL_BYTES {
        return Err(PinnedComponentVerificationError::InvalidSourceUrl);
    }
    let url = Url::parse(value).map_err(|_| PinnedComponentVerificationError::InvalidSourceUrl)?;
    let expected_suffix = match (platform.operating_system, platform.architecture) {
        (OperatingSystem::Linux, Architecture::X86_64) => "linux-x86_64",
        (OperatingSystem::Windows, Architecture::X86_64) => "windows-x86_64.exe",
        _ => return Err(PinnedComponentVerificationError::UnsupportedPlatform),
    };
    let expected_path = format!("/cli/grok-{version}-{expected_suffix}");
    if url.scheme() != "https"
        || url.host_str() != Some("x.ai")
        || url.port().is_some()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != expected_path
    {
        return Err(PinnedComponentVerificationError::InvalidSourceUrl);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PinnedComponentManifest {
    schema: String,
    name: String,
    publisher: String,
    version: String,
    os: String,
    architecture: String,
    executable: String,
    source_url: String,
    sha256: String,
    size: u64,
}

/// Source-pinned component verification failure without exposing file contents.
#[derive(Debug, Error)]
pub enum PinnedComponentVerificationError {
    /// Installation root is not an absolute, real directory.
    #[error("official pinned component installation root is invalid")]
    InvalidInstallRoot,
    /// Running target is outside the supported package matrix.
    #[error("official pinned component platform is unsupported")]
    UnsupportedPlatform,
    /// Compile-time binding has invalid syntax.
    #[error("official pinned component build binding is invalid")]
    InvalidBinding,
    /// Manifest bytes differ from the compile-time binding.
    #[error("official pinned component manifest does not match this daemon")]
    BindingMismatch,
    /// JSON is malformed, duplicated, nested too deeply, or oversized.
    #[error("official pinned component manifest JSON is invalid")]
    InvalidJson,
    /// Manifest schema or official identity is invalid.
    #[error("official pinned component manifest is invalid")]
    InvalidManifest,
    /// Component semantic version is malformed or oversized.
    #[error("official pinned component version is invalid")]
    InvalidVersion,
    /// Component target does not match the running package.
    #[error("official pinned component platform does not match")]
    WrongPlatform,
    /// Executable path is not the fixed canonical install-relative path.
    #[error("official pinned component executable path is invalid")]
    InvalidExecutablePath,
    /// Download evidence is not an official fixed x.ai CLI URL.
    #[error("official pinned component source URL is invalid")]
    InvalidSourceUrl,
    /// SHA-256 digest is not canonical lowercase hexadecimal.
    #[error("official pinned component digest is invalid")]
    InvalidDigest,
    /// Executable size is empty or exceeds the component boundary.
    #[error("official pinned component size is invalid")]
    InvalidSize,
    /// Local executable identity or integrity verification failed.
    #[error("official pinned component verification failed: {0}")]
    Component(#[from] ComponentVerificationError),
    /// Filesystem operation failed.
    #[error("official pinned component filesystem operation failed: {0}")]
    Io(std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use tempfile::TempDir;

    use super::*;

    struct Fixture {
        _directory: TempDir,
        root: PathBuf,
        manifest: Vec<u8>,
    }

    impl Fixture {
        fn linux() -> Self {
            let directory = tempfile::tempdir().expect("temporary directory");
            let root = directory.path().join("component");
            fs::create_dir_all(root.join("bin")).expect("component tree");
            let executable = root.join("bin/grok");
            let mut file = fs::File::create(&executable).expect("component");
            file.write_all(b"official-grok-fixture")
                .expect("fixture bytes");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
                    .expect("executable mode");
            }
            let digest = hex::encode(Sha256::digest(b"official-grok-fixture"));
            let manifest = format!(
                "{{\"schema\":\"{MANIFEST_SCHEMA}\",\"name\":\"grok-build\",\"publisher\":\"xAI\",\"version\":\"0.2.99\",\"os\":\"linux\",\"architecture\":\"x86_64\",\"executable\":\"bin/grok\",\"sourceUrl\":\"https://x.ai/cli/grok-0.2.99-linux-x86_64\",\"sha256\":\"{digest}\",\"size\":21}}"
            )
            .into_bytes();
            Self {
                _directory: directory,
                root,
                manifest,
            }
        }

        fn verifier(&self) -> OfficialGrokPinnedComponentVerifier {
            OfficialGrokPinnedComponentVerifier::new_for_platform(
                &self.root,
                manifest_binding(&self.manifest),
                ComponentPlatform {
                    operating_system: OperatingSystem::Linux,
                    architecture: Architecture::X86_64,
                },
            )
            .expect("verifier")
        }
    }

    #[test]
    fn verifies_exact_official_manifest_and_component() {
        let fixture = Fixture::linux();
        let component = fixture
            .verifier()
            .verify(&fixture.manifest)
            .expect("component");
        component.reverify().expect("spawn-time verification");
        assert_eq!(component.version(), &Version::new(0, 2, 99));
    }

    #[test]
    fn rejects_modified_manifest_and_unofficial_source() {
        let fixture = Fixture::linux();
        let mut changed = fixture.manifest.clone();
        changed.push(b' ');
        assert!(matches!(
            fixture.verifier().verify(&changed),
            Err(PinnedComponentVerificationError::BindingMismatch)
        ));

        let unofficial = String::from_utf8(fixture.manifest.clone())
            .expect("manifest")
            .replace("https://x.ai/", "https://example.com/")
            .into_bytes();
        let verifier = OfficialGrokPinnedComponentVerifier::new_for_platform(
            &fixture.root,
            manifest_binding(&unofficial),
            ComponentPlatform {
                operating_system: OperatingSystem::Linux,
                architecture: Architecture::X86_64,
            },
        )
        .expect("verifier");
        assert!(matches!(
            verifier.verify(&unofficial),
            Err(PinnedComponentVerificationError::InvalidSourceUrl)
        ));
    }
}
