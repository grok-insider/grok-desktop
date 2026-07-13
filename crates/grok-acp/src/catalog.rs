use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, de};
use thiserror::Error;

use crate::component::{ComponentVerificationError, MAX_COMPONENT_BYTES, VerifiedGrokComponent};

const ENVELOPE_SCHEMA: &str = "grok.official-component-catalog-envelope/v1";
const PAYLOAD_SCHEMA: &str = "grok.official-component-catalog/v1";
const SIGNATURE_DOMAIN: &[u8] = b"grok.desktop.official-component-catalog.v1\0";
const OFFICIAL_COMPONENT_NAME: &str = "grok-build";
const OFFICIAL_PUBLISHER: &str = "xAI";
/// Maximum encoded signed catalog envelope accepted at the file boundary.
pub const MAX_SIGNED_CATALOG_ENVELOPE_BYTES: usize = 512 * 1024;
const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_COMPONENTS: usize = 32;
const MAX_TRUSTED_KEYS: usize = 16;
const MAX_KEY_ID_BYTES: usize = 64;
const MAX_VERSION_BYTES: usize = 128;
const MAX_RELATIVE_PATH_BYTES: usize = 260;
const MAX_PATH_SEGMENTS: usize = 16;

/// One pinned Ed25519 release key accepted for official component catalogs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedCatalogKey {
    key_id: String,
    public_key: [u8; 32],
}

impl TrustedCatalogKey {
    /// Creates a pinned key after validating its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogVerificationError::InvalidTrustedKey`] for an invalid
    /// identifier or Ed25519 public key.
    pub fn new(
        key_id: impl Into<String>,
        public_key: [u8; 32],
    ) -> Result<Self, CatalogVerificationError> {
        let key_id = key_id.into();
        validate_key_id(&key_id)?;
        VerifyingKey::from_bytes(&public_key)
            .map_err(|_| CatalogVerificationError::InvalidTrustedKey)?;
        Ok(Self { key_id, public_key })
    }

    /// Stable identifier included in the signed envelope.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

/// A verified official Grok component and its anti-rollback catalog sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCatalogComponent {
    component: VerifiedGrokComponent,
    sequence: u64,
}

impl VerifiedCatalogComponent {
    /// Verified component safe to pass to the ACP runtime.
    #[must_use]
    pub const fn component(&self) -> &VerifiedGrokComponent {
        &self.component
    }

    /// Monotonic signed catalog sequence to persist as the next watermark.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Consumes the result and returns the verified component.
    #[must_use]
    pub fn into_component(self) -> VerifiedGrokComponent {
        self.component
    }
}

/// Verifies bounded signed catalogs against pinned keys and the current target.
///
/// The envelope contains exactly `schema`, `keyId`, base64 `payload`, and
/// base64 `signature`. The Ed25519 signature covers the protocol domain, the
/// big-endian key-ID length, the key ID, and the exact decoded payload bytes.
/// Payload JSON is authenticated before it is parsed.
#[derive(Debug)]
pub struct OfficialGrokCatalogVerifier {
    install_root: PathBuf,
    platform: ComponentPlatform,
    trusted_keys: HashMap<String, VerifyingKey>,
}

impl OfficialGrokCatalogVerifier {
    /// Creates a verifier for the running operating system and architecture.
    ///
    /// The installation root is canonicalized once. Catalog paths remain
    /// relative to this root and are checked for links on every verification.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogVerificationError`] when the root or trusted key set is
    /// invalid, empty, duplicated, or outside supported product platforms.
    pub fn new(
        install_root: impl AsRef<Path>,
        trusted_keys: impl IntoIterator<Item = TrustedCatalogKey>,
    ) -> Result<Self, CatalogVerificationError> {
        Self::new_for_platform(
            install_root.as_ref(),
            trusted_keys,
            ComponentPlatform::current()?,
        )
    }

    fn new_for_platform(
        install_root: &Path,
        trusted_keys: impl IntoIterator<Item = TrustedCatalogKey>,
        platform: ComponentPlatform,
    ) -> Result<Self, CatalogVerificationError> {
        if !install_root.is_absolute() {
            return Err(CatalogVerificationError::InvalidInstallRoot);
        }
        let root_metadata =
            std::fs::symlink_metadata(install_root).map_err(CatalogVerificationError::Io)?;
        if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
            return Err(CatalogVerificationError::InvalidInstallRoot);
        }
        let install_root = install_root
            .canonicalize()
            .map_err(CatalogVerificationError::Io)?;

        let mut keys = HashMap::new();
        for trusted in trusted_keys {
            validate_key_id(&trusted.key_id)?;
            let key = VerifyingKey::from_bytes(&trusted.public_key)
                .map_err(|_| CatalogVerificationError::InvalidTrustedKey)?;
            if keys.insert(trusted.key_id, key).is_some() {
                return Err(CatalogVerificationError::DuplicateTrustedKey);
            }
            if keys.len() > MAX_TRUSTED_KEYS {
                return Err(CatalogVerificationError::TooManyTrustedKeys);
            }
        }
        if keys.is_empty() {
            return Err(CatalogVerificationError::EmptyTrustedKeySet);
        }
        Ok(Self {
            install_root,
            platform,
            trusted_keys: keys,
        })
    }

    /// Verifies an envelope using the current system clock.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogVerificationError`] for malformed, untrusted, expired,
    /// rolled-back, platform-incompatible, or filesystem-mismatched catalogs.
    pub fn verify(
        &self,
        envelope: &[u8],
        rollback_watermark: u64,
    ) -> Result<VerifiedCatalogComponent, CatalogVerificationError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| CatalogVerificationError::InvalidSystemTime)?
            .as_secs();
        self.verify_at(envelope, rollback_watermark, now)
    }

    /// Verifies an envelope at a caller-supplied trusted Unix timestamp.
    ///
    /// Equality with the rollback watermark is accepted for restart-safe
    /// revalidation; lower catalog sequences are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogVerificationError`] for any failed trust, schema,
    /// rollback, platform, path, or executable integrity check.
    pub fn verify_at(
        &self,
        envelope: &[u8],
        rollback_watermark: u64,
        now_unix_seconds: u64,
    ) -> Result<VerifiedCatalogComponent, CatalogVerificationError> {
        ensure_strict_json(envelope, MAX_SIGNED_CATALOG_ENVELOPE_BYTES)?;
        let signed: CatalogEnvelope = serde_json::from_slice(envelope)
            .map_err(|_| CatalogVerificationError::InvalidEnvelope)?;
        if signed.schema != ENVELOPE_SCHEMA {
            return Err(CatalogVerificationError::InvalidEnvelope);
        }
        validate_key_id(&signed.key_id).map_err(|_| CatalogVerificationError::InvalidEnvelope)?;
        let key = self
            .trusted_keys
            .get(&signed.key_id)
            .ok_or(CatalogVerificationError::UnknownSigningKey)?;
        let payload = decode_canonical_base64(&signed.payload, MAX_PAYLOAD_BYTES)
            .map_err(|()| CatalogVerificationError::InvalidEnvelope)?;
        let signature_bytes = decode_canonical_base64(&signed.signature, 64)
            .map_err(|()| CatalogVerificationError::InvalidSignature)?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| CatalogVerificationError::InvalidSignature)?;
        let signed_bytes = signature_message(&signed.key_id, &payload)?;
        key.verify_strict(&signed_bytes, &signature)
            .map_err(|_| CatalogVerificationError::InvalidSignature)?;

        ensure_strict_json(&payload, MAX_PAYLOAD_BYTES)?;
        let catalog: CatalogPayload = serde_json::from_slice(&payload)
            .map_err(|_| CatalogVerificationError::InvalidCatalog)?;
        if catalog.schema != PAYLOAD_SCHEMA || catalog.sequence == 0 {
            return Err(CatalogVerificationError::InvalidCatalog);
        }
        if catalog.sequence < rollback_watermark {
            return Err(CatalogVerificationError::Rollback);
        }
        if catalog.expires_at_unix_seconds <= now_unix_seconds {
            return Err(CatalogVerificationError::Expired);
        }
        if catalog.components.is_empty() || catalog.components.len() > MAX_COMPONENTS {
            return Err(CatalogVerificationError::InvalidCatalog);
        }

        let mut platforms = HashSet::new();
        let mut selected = None;
        for record in catalog.components {
            let validated = validate_record(record)?;
            if !platforms.insert(validated.platform) {
                return Err(CatalogVerificationError::DuplicateComponent);
            }
            if validated.platform == self.platform {
                selected = Some(validated);
            }
        }
        let selected = selected.ok_or(CatalogVerificationError::WrongPlatform)?;
        let executable =
            resolve_install_relative(&self.install_root, &selected.relative_executable)?;
        let component = VerifiedGrokComponent::from_managed_manifest(
            executable,
            self.install_root.clone(),
            selected.relative_executable,
            selected.version,
            selected.digest,
            selected.size,
        )?;
        Ok(VerifiedCatalogComponent {
            component,
            sequence: catalog.sequence,
        })
    }
}

/// Signed-catalog verification failure without exposing catalog contents.
#[derive(Debug, Error)]
pub enum CatalogVerificationError {
    /// Installation root is not an absolute real directory.
    #[error("official component installation root is invalid")]
    InvalidInstallRoot,
    /// Running target is not an official supported platform.
    #[error("official component platform is unsupported")]
    UnsupportedPlatform,
    /// Trusted key record is malformed.
    #[error("official component trusted key is invalid")]
    InvalidTrustedKey,
    /// Trusted key identifier appears more than once.
    #[error("official component trusted key is duplicated")]
    DuplicateTrustedKey,
    /// No pinned release key was supplied.
    #[error("official component trusted key set is empty")]
    EmptyTrustedKeySet,
    /// Pinned key count exceeds the supported rotation window.
    #[error("official component trusted key set exceeds its limit")]
    TooManyTrustedKeys,
    /// JSON is oversized, malformed, duplicated, or excessively nested.
    #[error("official component catalog JSON is invalid")]
    InvalidJson,
    /// Signed envelope fields or base64 encodings are invalid.
    #[error("official component catalog envelope is invalid")]
    InvalidEnvelope,
    /// Envelope names a key outside the pinned set.
    #[error("official component catalog signing key is not trusted")]
    UnknownSigningKey,
    /// Ed25519 signature is malformed or does not verify.
    #[error("official component catalog signature is invalid")]
    InvalidSignature,
    /// Signed payload header or component bounds are invalid.
    #[error("official component catalog payload is invalid")]
    InvalidCatalog,
    /// Catalog sequence is below the persisted rollback watermark.
    #[error("official component catalog rollback was rejected")]
    Rollback,
    /// Catalog is expired at the trusted clock value.
    #[error("official component catalog is expired")]
    Expired,
    /// Catalog contains multiple records for one platform.
    #[error("official component catalog contains a duplicate component")]
    DuplicateComponent,
    /// Catalog does not contain the running OS and architecture.
    #[error("official component catalog does not match this platform")]
    WrongPlatform,
    /// Component name is not the official Grok Build identity.
    #[error("official component catalog name is invalid")]
    WrongComponentName,
    /// Component publisher is not exactly xAI.
    #[error("official component catalog publisher is invalid")]
    WrongPublisher,
    /// Component semantic version is malformed or oversized.
    #[error("official component catalog version is invalid")]
    InvalidVersion,
    /// Component executable path is not canonical and install-relative.
    #[error("official component catalog executable path is invalid")]
    InvalidExecutablePath,
    /// Component digest is not canonical SHA-256 hexadecimal.
    #[error("official component catalog digest is invalid")]
    InvalidDigest,
    /// Component size is zero or exceeds the executable bound.
    #[error("official component catalog size is invalid")]
    InvalidSize,
    /// Local system clock cannot be represented as Unix time.
    #[error("official component verification clock is invalid")]
    InvalidSystemTime,
    /// Verified component filesystem identity or integrity failed.
    #[error("official component catalog executable verification failed: {0}")]
    Component(#[from] ComponentVerificationError),
    /// Filesystem operation failed.
    #[error("official component catalog filesystem operation failed: {0}")]
    Io(std::io::Error),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogEnvelope {
    schema: String,
    key_id: String,
    payload: String,
    signature: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogPayload {
    schema: String,
    sequence: u64,
    expires_at_unix_seconds: u64,
    components: Vec<ComponentRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ComponentRecord {
    name: String,
    publisher: String,
    version: String,
    os: String,
    architecture: String,
    executable: String,
    sha256: String,
    size: u64,
}

struct ValidatedRecord {
    platform: ComponentPlatform,
    version: Version,
    relative_executable: String,
    digest: [u8; 32],
    size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ComponentPlatform {
    pub(crate) operating_system: OperatingSystem,
    pub(crate) architecture: Architecture,
}

impl ComponentPlatform {
    pub(crate) fn current() -> Result<Self, CatalogVerificationError> {
        let operating_system = if cfg!(target_os = "windows") {
            OperatingSystem::Windows
        } else if cfg!(target_os = "linux") {
            OperatingSystem::Linux
        } else {
            return Err(CatalogVerificationError::UnsupportedPlatform);
        };
        let architecture = if cfg!(target_arch = "x86_64") {
            Architecture::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Architecture::Aarch64
        } else {
            return Err(CatalogVerificationError::UnsupportedPlatform);
        };
        Ok(Self {
            operating_system,
            architecture,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OperatingSystem {
    Windows,
    Linux,
}

impl OperatingSystem {
    pub(crate) fn parse(value: &str) -> Result<Self, CatalogVerificationError> {
        match value {
            "windows" => Ok(Self::Windows),
            "linux" => Ok(Self::Linux),
            _ => Err(CatalogVerificationError::WrongPlatform),
        }
    }

    pub(crate) const fn executable_name(self) -> &'static str {
        match self {
            Self::Windows => "grok.exe",
            Self::Linux => "grok",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Architecture {
    X86_64,
    Aarch64,
}

impl Architecture {
    pub(crate) fn parse(value: &str) -> Result<Self, CatalogVerificationError> {
        match value {
            "x86_64" => Ok(Self::X86_64),
            "aarch64" => Ok(Self::Aarch64),
            _ => Err(CatalogVerificationError::WrongPlatform),
        }
    }
}

fn validate_record(record: ComponentRecord) -> Result<ValidatedRecord, CatalogVerificationError> {
    if record.name != OFFICIAL_COMPONENT_NAME {
        return Err(CatalogVerificationError::WrongComponentName);
    }
    if record.publisher != OFFICIAL_PUBLISHER {
        return Err(CatalogVerificationError::WrongPublisher);
    }
    if record.version.is_empty() || record.version.len() > MAX_VERSION_BYTES {
        return Err(CatalogVerificationError::InvalidVersion);
    }
    let version =
        Version::parse(&record.version).map_err(|_| CatalogVerificationError::InvalidVersion)?;
    let platform = ComponentPlatform {
        operating_system: OperatingSystem::parse(&record.os)?,
        architecture: Architecture::parse(&record.architecture)?,
    };
    validate_relative_path(
        &record.executable,
        platform.operating_system.executable_name(),
    )?;
    if record.size == 0 || record.size > MAX_COMPONENT_BYTES {
        return Err(CatalogVerificationError::InvalidSize);
    }
    let digest = decode_catalog_digest(&record.sha256)?;
    Ok(ValidatedRecord {
        platform,
        version,
        relative_executable: record.executable,
        digest,
        size: record.size,
    })
}

fn validate_key_id(key_id: &str) -> Result<(), CatalogVerificationError> {
    if key_id.is_empty()
        || key_id.len() > MAX_KEY_ID_BYTES
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CatalogVerificationError::InvalidTrustedKey);
    }
    Ok(())
}

pub(crate) fn validate_relative_path(
    relative: &str,
    executable_name: &str,
) -> Result<(), CatalogVerificationError> {
    if relative.is_empty()
        || relative.len() > MAX_RELATIVE_PATH_BYTES
        || relative.contains('\\')
        || relative.starts_with('/')
    {
        return Err(CatalogVerificationError::InvalidExecutablePath);
    }
    let segments: Vec<_> = relative.split('/').collect();
    if segments.is_empty()
        || segments.len() > MAX_PATH_SEGMENTS
        || segments.last().copied() != Some(executable_name)
        || segments.iter().any(|segment| {
            segment.is_empty()
                || *segment == "."
                || *segment == ".."
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return Err(CatalogVerificationError::InvalidExecutablePath);
    }
    Ok(())
}

pub(crate) fn resolve_install_relative(
    install_root: &Path,
    relative: &str,
) -> Result<PathBuf, ComponentVerificationError> {
    let mut candidate = install_root.to_path_buf();
    let segments: Vec<_> = relative.split('/').collect();
    if segments.is_empty() {
        return Err(ComponentVerificationError::InvalidInstallPath);
    }
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() || *segment == "." || *segment == ".." {
            return Err(ComponentVerificationError::InvalidInstallPath);
        }
        candidate.push(segment);
        let metadata =
            std::fs::symlink_metadata(&candidate).map_err(ComponentVerificationError::Io)?;
        if metadata.file_type().is_symlink() || (index + 1 < segments.len() && !metadata.is_dir()) {
            return Err(ComponentVerificationError::SymlinkEscape);
        }
        let canonical = candidate
            .canonicalize()
            .map_err(ComponentVerificationError::Io)?;
        if !canonical.starts_with(install_root) || canonical != candidate {
            return Err(ComponentVerificationError::SymlinkEscape);
        }
    }
    Ok(candidate)
}

pub(crate) fn decode_catalog_digest(value: &str) -> Result<[u8; 32], CatalogVerificationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(CatalogVerificationError::InvalidDigest);
    }
    let decoded = hex::decode(value).map_err(|_| CatalogVerificationError::InvalidDigest)?;
    decoded
        .try_into()
        .map_err(|_| CatalogVerificationError::InvalidDigest)
}

fn decode_canonical_base64(value: &str, maximum: usize) -> Result<Vec<u8>, ()> {
    let decoded = STANDARD.decode(value).map_err(|_| ())?;
    if decoded.is_empty() || decoded.len() > maximum || STANDARD.encode(&decoded) != value {
        return Err(());
    }
    Ok(decoded)
}

fn signature_message(key_id: &str, payload: &[u8]) -> Result<Vec<u8>, CatalogVerificationError> {
    let key_length =
        u16::try_from(key_id.len()).map_err(|_| CatalogVerificationError::InvalidEnvelope)?;
    let mut message = Vec::with_capacity(
        SIGNATURE_DOMAIN.len() + size_of::<u16>() + key_id.len() + payload.len(),
    );
    message.extend_from_slice(SIGNATURE_DOMAIN);
    message.extend_from_slice(&key_length.to_be_bytes());
    message.extend_from_slice(key_id.as_bytes());
    message.extend_from_slice(payload);
    Ok(message)
}

pub(crate) fn ensure_strict_json(
    data: &[u8],
    maximum: usize,
) -> Result<(), CatalogVerificationError> {
    if data.is_empty() || data.len() > maximum {
        return Err(CatalogVerificationError::InvalidJson);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(data);
    StrictJson::deserialize(&mut deserializer)
        .and_then(|_| deserializer.end())
        .map_err(|_| CatalogVerificationError::InvalidJson)
}

struct StrictJson;

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> de::Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("one strict JSON value")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJson)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        while sequence.next_element::<StrictJson>()?.is_some() {}
        Ok(StrictJson)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<StrictJson>()?;
        }
        Ok(StrictJson)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write};

    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use super::*;

    const NOW: u64 = 1_800_000_000;
    const KEY_ID: &str = "xai-release-2026";
    const EXECUTABLE_BYTES: &[u8] = b"official grok build component";

    type ErrorMatcher = fn(&CatalogVerificationError) -> bool;
    type MetadataCase = (&'static str, Value, ErrorMatcher);

    struct Fixture {
        _directory: tempfile::TempDir,
        root: PathBuf,
        executable: PathBuf,
        signing_key: SigningKey,
        verifier: OfficialGrokCatalogVerifier,
        payload: Value,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("temporary directory");
            let root = directory.path().join("official-install");
            let bin = root.join("bin");
            std::fs::create_dir_all(&bin).expect("installation directory");
            let platform = ComponentPlatform::current().expect("test platform");
            let executable_name = platform.operating_system.executable_name();
            let executable = bin.join(executable_name);
            write_executable(&executable, EXECUTABLE_BYTES);
            let signing_key = SigningKey::from_bytes(&[7; 32]);
            let trusted = TrustedCatalogKey::new(KEY_ID, signing_key.verifying_key().to_bytes())
                .expect("trusted key");
            let verifier =
                OfficialGrokCatalogVerifier::new_for_platform(&root, [trusted], platform)
                    .expect("verifier");
            let payload = json!({
                "schema": PAYLOAD_SCHEMA,
                "sequence": 42,
                "expiresAtUnixSeconds": NOW + 3600,
                "components": [{
                    "name": OFFICIAL_COMPONENT_NAME,
                    "publisher": OFFICIAL_PUBLISHER,
                    "version": "0.2.95",
                    "os": os_name(platform.operating_system),
                    "architecture": architecture_name(platform.architecture),
                    "executable": format!("bin/{executable_name}"),
                    "sha256": hex::encode(Sha256::digest(EXECUTABLE_BYTES)),
                    "size": EXECUTABLE_BYTES.len(),
                }],
            });
            Self {
                _directory: directory,
                root,
                executable,
                signing_key,
                verifier,
                payload,
            }
        }

        fn signed(&self, payload: &Value) -> Vec<u8> {
            let bytes = serde_json::to_vec(payload).expect("payload JSON");
            signed_envelope(KEY_ID, &bytes, &self.signing_key)
        }

        fn verify_payload(
            &self,
            payload: &Value,
        ) -> Result<VerifiedCatalogComponent, CatalogVerificationError> {
            self.verifier.verify_at(&self.signed(payload), 0, NOW)
        }
    }

    #[test]
    fn verifies_signed_official_catalog_and_accepts_equal_watermark() {
        let fixture = Fixture::new();
        let envelope = fixture.signed(&fixture.payload);
        let verified = fixture
            .verifier
            .verify_at(&envelope, 42, NOW)
            .expect("verified catalog");
        assert_eq!(verified.sequence(), 42);
        assert_eq!(verified.component().version(), &Version::new(0, 2, 95));
        assert_eq!(
            verified.component().executable(),
            fixture
                .executable
                .canonicalize()
                .expect("canonical fixture executable")
        );
        verified.component().reverify().expect("reverify");
    }

    #[test]
    fn rejects_unknown_key_and_invalid_signature() {
        let fixture = Fixture::new();
        let payload = serde_json::to_vec(&fixture.payload).expect("payload");
        let unknown = signed_envelope("unknown-release", &payload, &fixture.signing_key);
        assert!(matches!(
            fixture.verifier.verify_at(&unknown, 0, NOW),
            Err(CatalogVerificationError::UnknownSigningKey)
        ));

        let other = SigningKey::from_bytes(&[9; 32]);
        let invalid = signed_envelope(KEY_ID, &payload, &other);
        assert!(matches!(
            fixture.verifier.verify_at(&invalid, 0, NOW),
            Err(CatalogVerificationError::InvalidSignature)
        ));
    }

    #[test]
    fn rejects_unknown_and_duplicate_json_fields() {
        let fixture = Fixture::new();
        let mut payload = fixture.payload.clone();
        payload["unexpected"] = json!(true);
        assert!(matches!(
            fixture.verify_payload(&payload),
            Err(CatalogVerificationError::InvalidCatalog)
        ));

        let mut component = fixture.payload.clone();
        component["components"][0]["unexpected"] = json!(true);
        assert!(matches!(
            fixture.verify_payload(&component),
            Err(CatalogVerificationError::InvalidCatalog)
        ));

        let mut missing = fixture.payload.clone();
        missing["components"][0]
            .as_object_mut()
            .expect("component")
            .remove("publisher");
        assert!(matches!(
            fixture.verify_payload(&missing),
            Err(CatalogVerificationError::InvalidCatalog)
        ));

        let payload_bytes = serde_json::to_vec(&fixture.payload).expect("payload");
        let envelope = signed_envelope_value(KEY_ID, &payload_bytes, &fixture.signing_key);
        let mut envelope_object = envelope.as_object().expect("envelope").clone();
        envelope_object.insert("unexpected".into(), json!(true));
        let unknown_envelope = serde_json::to_vec(&envelope_object).expect("envelope JSON");
        assert!(matches!(
            fixture.verifier.verify_at(&unknown_envelope, 0, NOW),
            Err(CatalogVerificationError::InvalidEnvelope)
        ));

        let duplicate_payload = format!(
            r#"{{"schema":"{PAYLOAD_SCHEMA}","sequence":42,"sequence":43,"expiresAtUnixSeconds":{},"components":[]}}"#,
            NOW + 3600
        );
        let duplicate = signed_envelope(KEY_ID, duplicate_payload.as_bytes(), &fixture.signing_key);
        assert!(matches!(
            fixture.verifier.verify_at(&duplicate, 0, NOW),
            Err(CatalogVerificationError::InvalidJson)
        ));

        let valid = signed_envelope_value(KEY_ID, &payload_bytes, &fixture.signing_key);
        let key = valid["keyId"].as_str().expect("key ID");
        let raw_duplicate_envelope = format!(
            r#"{{"schema":"{ENVELOPE_SCHEMA}","keyId":"{key}","keyId":"{key}","payload":{},"signature":{}}}"#,
            serde_json::to_string(&valid["payload"]).expect("payload string"),
            serde_json::to_string(&valid["signature"]).expect("signature string"),
        );
        assert!(matches!(
            fixture
                .verifier
                .verify_at(raw_duplicate_envelope.as_bytes(), 0, NOW),
            Err(CatalogVerificationError::InvalidJson)
        ));
    }

    #[test]
    fn rejects_rollback_expiry_and_duplicate_components() {
        let fixture = Fixture::new();
        let envelope = fixture.signed(&fixture.payload);
        assert!(matches!(
            fixture.verifier.verify_at(&envelope, 43, NOW),
            Err(CatalogVerificationError::Rollback)
        ));

        let mut expired = fixture.payload.clone();
        expired["expiresAtUnixSeconds"] = json!(NOW);
        assert!(matches!(
            fixture.verify_payload(&expired),
            Err(CatalogVerificationError::Expired)
        ));

        let mut zero_sequence = fixture.payload.clone();
        zero_sequence["sequence"] = json!(0);
        assert!(matches!(
            fixture.verify_payload(&zero_sequence),
            Err(CatalogVerificationError::InvalidCatalog)
        ));

        let mut duplicate = fixture.payload.clone();
        let record = duplicate["components"][0].clone();
        duplicate["components"]
            .as_array_mut()
            .expect("components")
            .push(record);
        assert!(matches!(
            fixture.verify_payload(&duplicate),
            Err(CatalogVerificationError::DuplicateComponent)
        ));
    }

    #[test]
    fn rejects_signed_identity_platform_and_metadata_mismatches() {
        let fixture = Fixture::new();
        let cases: Vec<MetadataCase> = vec![
            ("name", json!("other-component"), |error| {
                matches!(error, CatalogVerificationError::WrongComponentName)
            }),
            ("publisher", json!("SpaceXAI"), |error| {
                matches!(error, CatalogVerificationError::WrongPublisher)
            }),
            ("version", json!("not-semver"), |error| {
                matches!(error, CatalogVerificationError::InvalidVersion)
            }),
            ("executable", json!("../grok"), |error| {
                matches!(error, CatalogVerificationError::InvalidExecutablePath)
            }),
            ("executable", json!("bin/not-grok"), |error| {
                matches!(error, CatalogVerificationError::InvalidExecutablePath)
            }),
            ("executable", json!("/bin/grok"), |error| {
                matches!(error, CatalogVerificationError::InvalidExecutablePath)
            }),
            ("os", json!("macos"), |error| {
                matches!(error, CatalogVerificationError::WrongPlatform)
            }),
            ("sha256", json!("A".repeat(64)), |error| {
                matches!(error, CatalogVerificationError::InvalidDigest)
            }),
            ("size", json!(0), |error| {
                matches!(error, CatalogVerificationError::InvalidSize)
            }),
            ("size", json!(MAX_COMPONENT_BYTES + 1), |error| {
                matches!(error, CatalogVerificationError::InvalidSize)
            }),
        ];
        for (field, value, matches_error) in cases {
            let mut payload = fixture.payload.clone();
            payload["components"][0][field] = value;
            let error = fixture.verify_payload(&payload).expect_err(field);
            assert!(matches_error(&error), "{field}: {error:?}");
        }

        let mut wrong_architecture = fixture.payload.clone();
        wrong_architecture["components"][0]["architecture"] = json!(
            match ComponentPlatform::current().expect("platform").architecture {
                Architecture::X86_64 => "aarch64",
                Architecture::Aarch64 => "x86_64",
            }
        );
        assert!(matches!(
            fixture.verify_payload(&wrong_architecture),
            Err(CatalogVerificationError::WrongPlatform)
        ));

        let mut wrong_digest = fixture.payload.clone();
        wrong_digest["components"][0]["sha256"] = json!("0".repeat(64));
        assert!(matches!(
            fixture.verify_payload(&wrong_digest),
            Err(CatalogVerificationError::Component(
                ComponentVerificationError::IntegrityMismatch
            ))
        ));

        let mut wrong_size = fixture.payload.clone();
        wrong_size["components"][0]["size"] = json!(EXECUTABLE_BYTES.len() + 1);
        assert!(matches!(
            fixture.verify_payload(&wrong_size),
            Err(CatalogVerificationError::Component(
                ComponentVerificationError::SizeMismatch
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_and_byte_identical_replacement() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let fixture = Fixture::new();
        let verified = fixture
            .verify_payload(&fixture.payload)
            .expect("verified component");
        let replacement = fixture.root.join("replacement-grok");
        write_executable(&replacement, EXECUTABLE_BYTES);
        std::fs::rename(&replacement, &fixture.executable).expect("replace executable");
        assert!(matches!(
            verified.component().reverify(),
            Err(ComponentVerificationError::IntegrityMismatch)
        ));

        let escaped = Fixture::new();
        std::fs::remove_file(&escaped.executable).expect("remove executable");
        std::fs::remove_dir(escaped.root.join("bin")).expect("remove bin");
        let outside = escaped.root.parent().expect("parent").join("outside");
        std::fs::create_dir(&outside).expect("outside");
        let outside_executable = outside.join("grok");
        write_executable(&outside_executable, EXECUTABLE_BYTES);
        symlink(&outside, escaped.root.join("bin")).expect("symlink");
        assert!(matches!(
            escaped.verify_payload(&escaped.payload),
            Err(CatalogVerificationError::Component(
                ComponentVerificationError::SymlinkEscape
            ))
        ));

        let mode = std::fs::metadata(&outside_executable)
            .expect("metadata")
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0);
    }

    #[test]
    fn rejects_invalid_trusted_key_sets() {
        let fixture = Fixture::new();
        assert!(matches!(
            OfficialGrokCatalogVerifier::new_for_platform(
                &fixture.root,
                [],
                ComponentPlatform::current().expect("platform"),
            ),
            Err(CatalogVerificationError::EmptyTrustedKeySet)
        ));
        assert!(matches!(
            TrustedCatalogKey::new("bad key id", fixture.signing_key.verifying_key().to_bytes()),
            Err(CatalogVerificationError::InvalidTrustedKey)
        ));
        let trusted =
            TrustedCatalogKey::new(KEY_ID, fixture.signing_key.verifying_key().to_bytes())
                .expect("trusted key");
        assert!(matches!(
            OfficialGrokCatalogVerifier::new_for_platform(
                &fixture.root,
                [trusted.clone(), trusted],
                ComponentPlatform::current().expect("platform"),
            ),
            Err(CatalogVerificationError::DuplicateTrustedKey)
        ));

        let excessive = (0..=MAX_TRUSTED_KEYS).map(|index| {
            TrustedCatalogKey::new(
                format!("release-{index}"),
                fixture.signing_key.verifying_key().to_bytes(),
            )
            .expect("trusted key")
        });
        assert!(matches!(
            OfficialGrokCatalogVerifier::new_for_platform(
                &fixture.root,
                excessive,
                ComponentPlatform::current().expect("platform"),
            ),
            Err(CatalogVerificationError::TooManyTrustedKeys)
        ));
    }

    #[test]
    fn rejects_catalog_and_envelope_size_and_encoding_bounds() {
        let fixture = Fixture::new();
        assert!(matches!(
            fixture
                .verifier
                .verify_at(&vec![b' '; MAX_SIGNED_CATALOG_ENVELOPE_BYTES + 1], 0, NOW,),
            Err(CatalogVerificationError::InvalidJson)
        ));

        let mut empty = fixture.payload.clone();
        empty["components"] = json!([]);
        assert!(matches!(
            fixture.verify_payload(&empty),
            Err(CatalogVerificationError::InvalidCatalog)
        ));

        let mut excessive = fixture.payload.clone();
        let record = excessive["components"][0].clone();
        excessive["components"] = Value::Array(vec![record; MAX_COMPONENTS + 1]);
        assert!(matches!(
            fixture.verify_payload(&excessive),
            Err(CatalogVerificationError::InvalidCatalog)
        ));

        let payload = serde_json::to_vec(&fixture.payload).expect("payload");
        let mut envelope = signed_envelope_value(KEY_ID, &payload, &fixture.signing_key);
        envelope["payload"] = json!(format!(
            "{}=",
            envelope["payload"].as_str().expect("base64")
        ));
        assert!(matches!(
            fixture
                .verifier
                .verify_at(&serde_json::to_vec(&envelope).expect("envelope"), 0, NOW,),
            Err(CatalogVerificationError::InvalidEnvelope)
        ));

        let mut short_signature = signed_envelope_value(KEY_ID, &payload, &fixture.signing_key);
        short_signature["signature"] = json!(STANDARD.encode([0_u8; 63]));
        assert!(matches!(
            fixture.verifier.verify_at(
                &serde_json::to_vec(&short_signature).expect("envelope"),
                0,
                NOW,
            ),
            Err(CatalogVerificationError::InvalidSignature)
        ));
    }

    fn signed_envelope(key_id: &str, payload: &[u8], signing_key: &SigningKey) -> Vec<u8> {
        serde_json::to_vec(&signed_envelope_value(key_id, payload, signing_key))
            .expect("envelope JSON")
    }

    fn signed_envelope_value(key_id: &str, payload: &[u8], signing_key: &SigningKey) -> Value {
        let message = signature_message(key_id, payload).expect("signature message");
        let signature = signing_key.sign(&message);
        json!({
            "schema": ENVELOPE_SCHEMA,
            "keyId": key_id,
            "payload": STANDARD.encode(payload),
            "signature": STANDARD.encode(signature.to_bytes()),
        })
    }

    fn write_executable(path: &Path, contents: &[u8]) {
        let mut file = File::create(path).expect("create executable");
        file.write_all(contents).expect("write executable");
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("executable permissions");
        }
    }

    const fn os_name(operating_system: OperatingSystem) -> &'static str {
        match operating_system {
            OperatingSystem::Windows => "windows",
            OperatingSystem::Linux => "linux",
        }
    }

    const fn architecture_name(architecture: Architecture) -> &'static str {
        match architecture {
            Architecture::X86_64 => "x86_64",
            Architecture::Aarch64 => "aarch64",
        }
    }
}
