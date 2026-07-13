use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt as _;
#[cfg(unix)]
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const CONFIG_TOML: &str = r#"# Managed by Grok Desktop. Manual changes make the runtime unavailable.
[models]
default = "grok-build"

[cli]
auto_update = false

[ui]
permission_mode = "ask"

[skills]
paths = []

[plugins]
paths = []

[compat.cursor]
skills = false
rules = false
agents = false
mcps = false
hooks = false

[compat.claude]
skills = false
rules = false
agents = false
mcps = false
hooks = false

[marketplace]
official_marketplace_auto_installed = true

[[marketplace.sources]]
name = "xAI Official"
git = "https://github.com/xai-org/plugin-marketplace.git"
"#;

const REQUIREMENTS_TOML: &str = r#"# Managed by Grok Desktop. Manual changes make the runtime unavailable.
[grok_com_config]
disable_api_key_auth = true

[ui]
disable_bypass_permissions_mode = true

[models]
default = "grok-build"

[cli]
auto_update = false

[sandbox]
profile = "strict"

[skills]
paths = []

[plugins]
paths = []

[compat.cursor]
skills = false
rules = false
agents = false
mcps = false
hooks = false

[compat.claude]
skills = false
rules = false
agents = false
mcps = false
hooks = false
"#;

const FORBIDDEN_HOME_ENTRIES: &[&str] = &[
    "managed_config.toml",
    "sandbox.toml",
    "mcp_credentials.json",
    "hooks-paths",
    "plugins",
    "hooks",
    ".claude",
    ".cursor",
    ".agents",
    ".claude.json",
    ".mcp.json",
];

const FORBIDDEN_LAUNCH_ENTRIES: &[&str] = &[
    ".grok",
    ".claude",
    ".cursor",
    ".agents",
    ".mcp.json",
    "AGENTS.md",
    "Agents.md",
    "AGENT.md",
    "CLAUDE.md",
    "Claude.md",
    "CLAUDE.local.md",
];

/// Stable location policy for one per-user, per-install Grok Build home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokHomeSpec {
    base_directory: PathBuf,
    installation_id: String,
}

impl GrokHomeSpec {
    /// Creates a path policy rooted below a trusted per-user application-data directory.
    ///
    /// # Errors
    ///
    /// Returns [`GrokHomeError`] when the base is relative or the installation
    /// identifier could introduce path syntax.
    pub fn new(
        base_directory: impl Into<PathBuf>,
        installation_id: impl Into<String>,
    ) -> Result<Self, GrokHomeError> {
        let base_directory = base_directory.into();
        let installation_id = installation_id.into();
        if !base_directory.is_absolute() {
            return Err(GrokHomeError::InvalidLocation);
        }
        if installation_id.is_empty()
            || installation_id.len() > 64
            || !installation_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(GrokHomeError::InvalidInstallationId);
        }
        Ok(Self {
            base_directory,
            installation_id,
        })
    }

    /// Deterministic `GROK_HOME` path for this installation.
    #[must_use]
    pub fn home_path(&self) -> PathBuf {
        self.base_directory
            .join(&self.installation_id)
            .join("grok-home")
    }

    pub(crate) fn provision(&self) -> Result<ProvisionedGrokHome, GrokHomeError> {
        ensure_base_directory(&self.base_directory)?;
        let installation = self.base_directory.join(&self.installation_id);
        ensure_private_directory(&installation)?;
        let home = installation.join("grok-home");
        ensure_private_directory(&home)?;
        verify_containment(&self.base_directory, &home)?;

        let lock = open_runtime_lock(&home.join(".runtime.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| GrokHomeError::RuntimeBusy)?;

        reject_entries(&home, FORBIDDEN_HOME_ENTRIES)?;
        verify_optional_runtime_directory(&home.join("skills"))?;
        let launch_directory = home.join("launch");
        ensure_private_directory(&launch_directory)?;
        reject_entries(&launch_directory, FORBIDDEN_LAUNCH_ENTRIES)?;

        let os_directory = home.join("os");
        ensure_private_directory(&os_directory)?;
        let roaming_directory = os_directory.join("roaming");
        let local_directory = os_directory.join("local");
        let config_directory = os_directory.join("config");
        let data_directory = os_directory.join("data");
        let temporary_directory = os_directory.join("temp");
        for directory in [
            &roaming_directory,
            &local_directory,
            &config_directory,
            &data_directory,
            &temporary_directory,
        ] {
            ensure_private_directory(directory)?;
        }

        ensure_managed_file(&home.join("config.toml"), CONFIG_TOML.as_bytes())?;
        ensure_managed_file(
            &home.join("requirements.toml"),
            REQUIREMENTS_TOML.as_bytes(),
        )?;
        reject_entries(&home, FORBIDDEN_HOME_ENTRIES)?;
        verify_optional_runtime_directory(&home.join("skills"))?;

        Ok(ProvisionedGrokHome {
            home,
            launch_directory,
            roaming_directory,
            local_directory,
            config_directory,
            data_directory,
            temporary_directory,
            _lock: lock,
        })
    }
}

/// Failure to establish the closed Grok Build configuration boundary.
#[derive(Debug, Error)]
pub enum GrokHomeError {
    /// The configured root was not an absolute application-data path.
    #[error("isolated Grok home location is invalid")]
    InvalidLocation,
    /// The installation identifier was not a bounded path-safe token.
    #[error("isolated Grok home installation identity is invalid")]
    InvalidInstallationId,
    /// A symlink, reparse point, hard link, or containment escape was found.
    #[error("isolated Grok home contains an unsafe filesystem object")]
    UnsafeFilesystemObject,
    /// An existing managed node grants access outside the current user.
    #[error("isolated Grok home permissions are unsafe")]
    UnsafePermissions,
    /// Existing configuration differs from the compiled closed policy.
    #[error("isolated Grok home configuration is not managed")]
    UnexpectedConfiguration,
    /// Another daemon already owns this installation's runtime.
    #[error("isolated Grok home is already in use")]
    RuntimeBusy,
    /// A filesystem operation failed. The path is deliberately not displayed.
    #[error("isolated Grok home filesystem operation failed")]
    Io(#[source] io::Error),
}

pub(crate) struct ProvisionedGrokHome {
    home: PathBuf,
    launch_directory: PathBuf,
    roaming_directory: PathBuf,
    local_directory: PathBuf,
    config_directory: PathBuf,
    data_directory: PathBuf,
    temporary_directory: PathBuf,
    _lock: File,
}

impl std::fmt::Debug for ProvisionedGrokHome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProvisionedGrokHome")
            .finish_non_exhaustive()
    }
}

impl ProvisionedGrokHome {
    #[cfg(test)]
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn launch_directory(&self) -> &Path {
        &self.launch_directory
    }

    pub(crate) fn environment(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("GROK_HOME", self.home.clone()),
            ("HOME", self.home.clone()),
            ("USERPROFILE", self.home.clone()),
            ("APPDATA", self.roaming_directory.clone()),
            ("LOCALAPPDATA", self.local_directory.clone()),
            ("XDG_CONFIG_HOME", self.config_directory.clone()),
            ("XDG_DATA_HOME", self.data_directory.clone()),
            ("TEMP", self.temporary_directory.clone()),
            ("TMP", self.temporary_directory.clone()),
        ]
    }

    #[cfg(unix)]
    pub(crate) fn install_runtime_ca_bundle(
        &self,
        source: &Path,
    ) -> Result<PathBuf, GrokHomeError> {
        const MAX_CA_BUNDLE_BYTES: u64 = 4 * 1024 * 1024;

        let canonical_source = source.canonicalize().map_err(GrokHomeError::Io)?;
        let metadata = fs::metadata(&canonical_source).map_err(GrokHomeError::Io)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CA_BUNDLE_BYTES {
            return Err(GrokHomeError::UnexpectedConfiguration);
        }
        let bundle = fs::read(&canonical_source).map_err(GrokHomeError::Io)?;
        if bundle.is_empty() || bundle.len() as u64 > MAX_CA_BUNDLE_BYTES {
            return Err(GrokHomeError::UnexpectedConfiguration);
        }
        let digest = hex::encode(Sha256::digest(&bundle));
        let destination = self.home.join(format!("runtime-ca-{digest}.pem"));
        ensure_managed_file(&destination, &bundle)?;
        Ok(destination)
    }
}

fn ensure_base_directory(path: &Path) -> Result<(), GrokHomeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure_existing_private_directory(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(GrokHomeError::InvalidLocation)?;
            let metadata = fs::symlink_metadata(parent).map_err(GrokHomeError::Io)?;
            verify_directory_type(&metadata)?;
            create_private_directory(path)?;
            set_private_directory_permissions(path)?;
            verify_directory(
                path,
                &fs::symlink_metadata(path).map_err(GrokHomeError::Io)?,
            )
        }
        Err(error) => Err(GrokHomeError::Io(error)),
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), GrokHomeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure_existing_private_directory(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directory(path)?;
            set_private_directory_permissions(path)?;
            verify_directory(
                path,
                &fs::symlink_metadata(path).map_err(GrokHomeError::Io)?,
            )
        }
        Err(error) => Err(GrokHomeError::Io(error)),
    }
}

/// Reuses an existing directory after confirming it is a normal directory,
/// tightening mode bits to the private policy when needed.
///
/// The application data root may already exist at `0755` from earlier
/// `create_dir_all` database layout work; fail closed when type/link checks
/// fail or when chmod cannot restore the private mode (e.g. foreign owner).
fn ensure_existing_private_directory(
    path: &Path,
    metadata: &Metadata,
) -> Result<(), GrokHomeError> {
    verify_directory_type(metadata)?;
    if verify_directory_permissions(path, metadata).is_ok() {
        return Ok(());
    }
    set_private_directory_permissions(path)?;
    verify_directory(
        path,
        &fs::symlink_metadata(path).map_err(GrokHomeError::Io)?,
    )
}

fn verify_directory(path: &Path, metadata: &Metadata) -> Result<(), GrokHomeError> {
    verify_directory_type(metadata)?;
    verify_directory_permissions(path, metadata)
}

fn verify_directory_type(metadata: &Metadata) -> Result<(), GrokHomeError> {
    if !metadata.is_dir() || is_symlink_or_reparse(metadata) {
        return Err(GrokHomeError::UnsafeFilesystemObject);
    }
    Ok(())
}

fn verify_containment(base: &Path, child: &Path) -> Result<(), GrokHomeError> {
    let base = base.canonicalize().map_err(GrokHomeError::Io)?;
    let child = child.canonicalize().map_err(GrokHomeError::Io)?;
    if !child.starts_with(base) {
        return Err(GrokHomeError::UnsafeFilesystemObject);
    }
    Ok(())
}

fn reject_entries(parent: &Path, names: &[&str]) -> Result<(), GrokHomeError> {
    for name in names {
        match fs::symlink_metadata(parent.join(name)) {
            Ok(_) => return Err(GrokHomeError::UnexpectedConfiguration),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(GrokHomeError::Io(error)),
        }
    }
    Ok(())
}

/// The official Grok component extracts its bundled skills below `GROK_HOME`
/// during initialization. Treat that directory as runtime-owned state rather
/// than caller configuration, but accept it only as a private, normal
/// directory. The daemon-private home remains outside Host Tools authority,
/// while the managed configuration disables project/vendor compatibility
/// surfaces and additional caller-supplied skill paths.
fn verify_optional_runtime_directory(path: &Path) -> Result<(), GrokHomeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure_existing_private_directory(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GrokHomeError::Io(error)),
    }
}

fn open_runtime_lock(path: &Path) -> Result<File, GrokHomeError> {
    let file = match secure_open_runtime_lock(path, true) {
        Ok(file) => {
            set_private_file_permissions(&file)?;
            file
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let file = secure_open_runtime_lock(path, false).map_err(GrokHomeError::Io)?;
            verify_regular_file(&file, Some(0))?;
            file
        }
        Err(error) => return Err(GrokHomeError::Io(error)),
    };
    verify_path_is_not_link(path)?;
    verify_regular_file(&file, Some(0))?;
    Ok(file)
}

fn secure_open_runtime_lock(path: &Path, create_new: bool) -> io::Result<File> {
    #[cfg(windows)]
    {
        grok_windows_acl::open_private_lock_file(path, create_new)
    }

    #[cfg(not(windows))]
    {
        secure_open(path, true, create_new, create_new)
    }
}

fn ensure_managed_file(path: &Path, expected: &[u8]) -> Result<(), GrokHomeError> {
    match secure_open(path, true, false, false) {
        Ok(mut file) => {
            verify_path_is_not_link(path)?;
            verify_regular_file(&file, Some(expected.len() as u64))?;
            let mut actual = Vec::with_capacity(expected.len());
            file.read_to_end(&mut actual).map_err(GrokHomeError::Io)?;
            if actual != expected {
                return Err(GrokHomeError::UnexpectedConfiguration);
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => atomic_create_file(path, expected),
        Err(error) => Err(GrokHomeError::Io(error)),
    }
}

fn atomic_create_file(path: &Path, expected: &[u8]) -> Result<(), GrokHomeError> {
    let parent = path.parent().ok_or(GrokHomeError::InvalidLocation)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(GrokHomeError::InvalidLocation)?;
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = secure_open(&temporary, false, true, true).map_err(GrokHomeError::Io)?;
        set_private_file_permissions(&file)?;
        file.write_all(expected).map_err(GrokHomeError::Io)?;
        file.sync_all().map_err(GrokHomeError::Io)?;
        atomic_publish_file(&file, &temporary, path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                GrokHomeError::UnexpectedConfiguration
            } else {
                GrokHomeError::Io(error)
            }
        })?;
        sync_directory(parent)?;
        verify_path_is_not_link(path)?;
        verify_regular_file(&file, Some(expected.len() as u64))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn atomic_publish_file(file: &File, _temporary: &Path, path: &Path) -> io::Result<()> {
    grok_windows_acl::publish_private_file(file, path)
}

#[cfg(not(windows))]
fn atomic_publish_file(_file: &File, temporary: &Path, path: &Path) -> io::Result<()> {
    fs::hard_link(temporary, path)?;
    fs::remove_file(temporary)
}

fn verify_path_is_not_link(path: &Path) -> Result<(), GrokHomeError> {
    let metadata = fs::symlink_metadata(path).map_err(GrokHomeError::Io)?;
    if is_symlink_or_reparse(&metadata) {
        return Err(GrokHomeError::UnsafeFilesystemObject);
    }
    Ok(())
}

fn verify_regular_file(file: &File, expected_len: Option<u64>) -> Result<(), GrokHomeError> {
    let metadata = file.metadata().map_err(GrokHomeError::Io)?;
    if !metadata.is_file()
        || is_symlink_or_reparse(&metadata)
        || expected_len.is_some_and(|length| metadata.len() != length)
        || has_multiple_links(file, &metadata)?
    {
        return Err(GrokHomeError::UnsafeFilesystemObject);
    }
    verify_file_permissions(file, &metadata)
}

fn secure_open(path: &Path, read: bool, write: bool, create_new: bool) -> io::Result<File> {
    #[cfg(windows)]
    {
        grok_windows_acl::open_private_file(path, read, write, create_new)
    }

    #[cfg(not(windows))]
    {
        let mut options = fs::OpenOptions::new();
        options.read(read).write(write).create_new(create_new);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        options.open(path)
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> Result<(), GrokHomeError> {
    fs::create_dir(path).map_err(GrokHomeError::Io)
}

#[cfg(windows)]
fn create_private_directory(path: &Path) -> Result<(), GrokHomeError> {
    grok_windows_acl::create_private_directory(path).map_err(GrokHomeError::Io)
}

#[cfg(all(not(unix), not(windows)))]
fn create_private_directory(_path: &Path) -> Result<(), GrokHomeError> {
    Err(unsupported_permissions_platform())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), GrokHomeError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(GrokHomeError::Io)
}

#[cfg(windows)]
fn set_private_directory_permissions(path: &Path) -> Result<(), GrokHomeError> {
    grok_windows_acl::apply_private_directory_acl(path).map_err(GrokHomeError::Io)
}

#[cfg(all(not(unix), not(windows)))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), GrokHomeError> {
    Err(unsupported_permissions_platform())
}

#[cfg(unix)]
fn set_private_file_permissions(file: &File) -> Result<(), GrokHomeError> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(GrokHomeError::Io)
}

#[cfg(windows)]
fn set_private_file_permissions(file: &File) -> Result<(), GrokHomeError> {
    grok_windows_acl::apply_private_acl(file, grok_windows_acl::PrivateObjectKind::File)
        .map_err(GrokHomeError::Io)
}

#[cfg(all(not(unix), not(windows)))]
fn set_private_file_permissions(_file: &File) -> Result<(), GrokHomeError> {
    Err(unsupported_permissions_platform())
}

#[cfg(unix)]
fn verify_directory_permissions(_path: &Path, metadata: &Metadata) -> Result<(), GrokHomeError> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(GrokHomeError::UnsafePermissions);
    }
    Ok(())
}

#[cfg(windows)]
fn verify_directory_permissions(path: &Path, _metadata: &Metadata) -> Result<(), GrokHomeError> {
    match grok_windows_acl::verify_private_directory(path) {
        Ok(true) => Ok(()),
        Ok(false) => Err(GrokHomeError::UnsafePermissions),
        Err(error) => Err(GrokHomeError::Io(error)),
    }
}

#[cfg(all(not(unix), not(windows)))]
fn verify_directory_permissions(_path: &Path, _metadata: &Metadata) -> Result<(), GrokHomeError> {
    Err(unsupported_permissions_platform())
}

#[cfg(unix)]
fn verify_file_permissions(_file: &File, metadata: &Metadata) -> Result<(), GrokHomeError> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(GrokHomeError::UnsafePermissions);
    }
    Ok(())
}

#[cfg(windows)]
fn verify_file_permissions(file: &File, _metadata: &Metadata) -> Result<(), GrokHomeError> {
    match grok_windows_acl::verify_private_acl(file, grok_windows_acl::PrivateObjectKind::File) {
        Ok(true) => Ok(()),
        Ok(false) => Err(GrokHomeError::UnsafePermissions),
        Err(error) => Err(GrokHomeError::Io(error)),
    }
}

#[cfg(all(not(unix), not(windows)))]
fn verify_file_permissions(_file: &File, _metadata: &Metadata) -> Result<(), GrokHomeError> {
    Err(unsupported_permissions_platform())
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn has_multiple_links(_file: &File, metadata: &Metadata) -> Result<bool, GrokHomeError> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(metadata.nlink() != 1)
}

#[cfg(windows)]
fn has_multiple_links(file: &File, _metadata: &Metadata) -> Result<bool, GrokHomeError> {
    grok_windows_acl::file_has_single_link(file)
        .map(|single| !single)
        .map_err(GrokHomeError::Io)
}

#[cfg(all(not(unix), not(windows)))]
fn has_multiple_links(_file: &File, _metadata: &Metadata) -> Result<bool, GrokHomeError> {
    Err(unsupported_permissions_platform())
}

#[cfg(windows)]
fn is_symlink_or_reparse(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_symlink_or_reparse(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), GrokHomeError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(GrokHomeError::Io)
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep one fallible durability contract across platforms.
fn sync_directory(_path: &Path) -> Result<(), GrokHomeError> {
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn unsupported_permissions_platform() -> GrokHomeError {
    GrokHomeError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "private filesystem permissions are unavailable",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_syntax_in_installation_identity() {
        let base = std::env::temp_dir();
        assert!(matches!(
            GrokHomeSpec::new(base, "../other"),
            Err(GrokHomeError::InvalidInstallationId)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn tightens_existing_world_readable_base_directory_to_private_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("tempdir");
        let base = directory.path().join("app-data");
        fs::create_dir(&base).expect("create base");
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).expect("mode 755");
        assert_eq!(
            fs::metadata(&base).expect("metadata").permissions().mode() & 0o777,
            0o755
        );
        let specification = GrokHomeSpec::new(&base, "installation-tighten").expect("spec");
        let provisioned = specification
            .provision()
            .expect("provision despite prior 0755");
        drop(provisioned);
        assert_eq!(
            fs::metadata(&base).expect("metadata").permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn provisions_closed_configuration_and_detects_tampering() {
        let directory = tempfile::tempdir().expect("tempdir");
        let specification = GrokHomeSpec::new(directory.path().join("app-data"), "installation-1")
            .expect("specification");
        let provisioned = specification.provision().expect("provision");
        let requirements =
            fs::read_to_string(provisioned.home().join("requirements.toml")).expect("requirements");
        assert!(requirements.contains("disable_api_key_auth = true"));
        assert!(requirements.contains("disable_bypass_permissions_mode = true"));
        assert_eq!(
            provisioned.launch_directory(),
            provisioned.home().join("launch")
        );
        let environment = provisioned.environment();
        assert_eq!(environment.len(), 9);
        assert!(environment.iter().all(|(name, value)| {
            !name.contains("API_KEY")
                && !name.contains("TOKEN")
                && value.starts_with(provisioned.home())
        }));
        drop(provisioned);

        let config = specification.home_path().join("config.toml");
        fs::write(config, b"[models]\ndefault = \"custom\"\n").expect("tamper");
        assert!(matches!(
            specification.provision(),
            Err(GrokHomeError::UnexpectedConfiguration | GrokHomeError::UnsafeFilesystemObject)
        ));
    }

    #[test]
    fn accepts_canonical_official_runtime_state_across_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        let specification = GrokHomeSpec::new(directory.path().join("app-data"), "runtime-state")
            .expect("specification");
        let provisioned = specification.provision().expect("first provision");
        let home = provisioned.home().to_path_buf();
        assert!(
            fs::read_to_string(home.join("config.toml"))
                .expect("managed configuration")
                .contains("git = \"https://github.com/xai-org/plugin-marketplace.git\"")
        );
        drop(provisioned);

        fs::create_dir(home.join("skills")).expect("official runtime skills");
        fs::write(
            home.join("skills").join("bundled.md"),
            b"official runtime asset",
        )
        .expect("official runtime skill asset");
        specification
            .provision()
            .expect("runtime-owned skills survive restart");
    }

    #[test]
    fn rejects_unmanaged_plugins_and_modified_marketplace_configuration() {
        let directory = tempfile::tempdir().expect("tempdir");
        let specification = GrokHomeSpec::new(directory.path().join("app-data"), "runtime-state")
            .expect("specification");
        let provisioned = specification.provision().expect("first provision");
        let home = provisioned.home().to_path_buf();
        drop(provisioned);

        fs::create_dir(home.join("plugins")).expect("unmanaged plugins");
        assert!(matches!(
            specification.provision(),
            Err(GrokHomeError::UnexpectedConfiguration)
        ));

        fs::remove_dir(home.join("plugins")).expect("remove unmanaged plugins");
        let config = home.join("config.toml");
        let modified = CONFIG_TOML.replace(
            "https://github.com/xai-org/plugin-marketplace.git",
            "https://example.invalid/untrusted-marketplace.git",
        );
        fs::write(config, modified).expect("modify marketplace source");
        assert!(matches!(
            specification.provision(),
            Err(GrokHomeError::UnexpectedConfiguration | GrokHomeError::UnsafeFilesystemObject)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_link_substituted_for_runtime_owned_skills() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tempdir");
        let specification = GrokHomeSpec::new(directory.path().join("app-data"), "linked-skills")
            .expect("specification");
        let provisioned = specification.provision().expect("first provision");
        let home = provisioned.home().to_path_buf();
        drop(provisioned);

        symlink(directory.path(), home.join("skills")).expect("skills link");
        assert!(matches!(
            specification.provision(),
            Err(GrokHomeError::UnsafeFilesystemObject)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn provisions_owner_only_protected_windows_acls() {
        let directory = tempfile::tempdir().expect("tempdir");
        let specification = GrokHomeSpec::new(directory.path().join("app-data"), "windows-acl")
            .expect("specification");
        let provisioned = specification.provision().expect("provision");
        for path in [
            &provisioned.home,
            &provisioned.launch_directory,
            &provisioned.roaming_directory,
            &provisioned.local_directory,
            &provisioned.config_directory,
            &provisioned.data_directory,
            &provisioned.temporary_directory,
        ] {
            assert!(
                grok_windows_acl::verify_private_directory(path).expect("verify directory ACL")
            );
        }
        for name in [".runtime.lock", "config.toml", "requirements.toml"] {
            let file = secure_open(&provisioned.home.join(name), true, false, false)
                .expect("open managed file");
            assert!(
                grok_windows_acl::verify_private_acl(
                    &file,
                    grok_windows_acl::PrivateObjectKind::File,
                )
                .expect("verify file ACL")
            );
        }
    }

    #[test]
    fn exclusive_runtime_lock_prevents_parallel_owners() {
        let directory = tempfile::tempdir().expect("tempdir");
        let specification =
            GrokHomeSpec::new(directory.path().join("app-data"), "one").expect("specification");
        let first = specification.provision().expect("first");
        assert!(matches!(
            specification.provision(),
            Err(GrokHomeError::RuntimeBusy)
        ));
        drop(first);
        specification.provision().expect("lock released");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_managed_configuration() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tempdir");
        let specification =
            GrokHomeSpec::new(directory.path().join("app-data"), "one").expect("specification");
        let provisioned = specification.provision().expect("provision");
        let home = provisioned.home().to_path_buf();
        drop(provisioned);
        fs::remove_file(home.join("config.toml")).expect("remove");
        symlink(home.join("requirements.toml"), home.join("config.toml")).expect("symlink");
        assert!(matches!(
            specification.provision(),
            Err(GrokHomeError::Io(_) | GrokHomeError::UnsafeFilesystemObject)
        ));
    }
}
