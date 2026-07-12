//! Read-only static qualification probe for the Linux isolation broker.
//!
//! A reachable QEMU/KVM broker is insufficient. The response must bind the
//! packaged broker, signed guest catalog, selected image, and hardware
//! qualification evidence before this adapter returns qualified capabilities.

use std::{
    fs::File,
    io::{self, Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use grok_application::{
    IsolationBackend, IsolationBrokerCapabilities, IsolationBrokerOperation,
    IsolationContractVersion, IsolationProbe, IsolationProbeError, IsolationWorkspaceMode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::spawn_blocking;

const WIRE_VERSION: &str = "1.0.0";
const CONTRACT_VERSION: &str = "1.1.0";
const MAX_FRAME_BYTES: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const PRODUCTION_SOCKET: &str = "/run/grok-desktop/linux-vm-service.sock";
const PRODUCTION_BROKER: &str = "/usr/libexec/grok-desktop/grok-linux-vm-service";
const BUILD_BROKER_SHA256: Option<&str> = option_env!("GROK_LINUX_VM_SERVICE_SHA256");
const BUILD_BROKER_BINDING: Option<&str> = option_env!("GROK_LINUX_VM_SERVICE_TRUST_BINDING");
const BROKER_BINDING_PREFIX: &str = "grok-linux-vm-service-trust-v1:";
const EXPECTED_OPERATIONS: [&str; 7] = [
    "attach_workspace",
    "create_vm",
    "delete_vm",
    "ensure_image",
    "get_capabilities",
    "start_vm",
    "stop_vm",
];

#[derive(Debug, Clone)]
pub(crate) struct LinuxVmServiceIsolationProbe {
    socket_path: PathBuf,
    expected_broker: Option<ExpectedBroker>,
}

#[derive(Debug, Clone)]
struct ExpectedBroker {
    executable: PathBuf,
    sha256: [u8; 32],
}

impl LinuxVmServiceIsolationProbe {
    pub(crate) fn production() -> Option<Self> {
        let digest = BUILD_BROKER_SHA256?;
        let binding = BUILD_BROKER_BINDING?;
        let expected_binding = format!(
            "{BROKER_BINDING_PREFIX}{}",
            hex::encode(Sha256::digest(digest.as_bytes()))
        );
        if binding != expected_binding {
            return None;
        }
        let bytes = hex::decode(digest).ok()?;
        let sha256: [u8; 32] = bytes.try_into().ok()?;
        Some(Self {
            socket_path: PathBuf::from(PRODUCTION_SOCKET),
            expected_broker: Some(ExpectedBroker {
                executable: PathBuf::from(PRODUCTION_BROKER),
                sha256,
            }),
        })
    }

    #[cfg(all(debug_assertions, feature = "debug-linux-vm-service"))]
    pub(crate) fn from_env() -> Option<Self> {
        let socket_path = std::env::var_os("GROK_LINUX_VM_SOCKET").map(PathBuf::from)?;
        if !socket_path.is_absolute() {
            return None;
        }
        Some(Self {
            socket_path,
            expected_broker: None,
        })
    }

    #[cfg(not(all(debug_assertions, feature = "debug-linux-vm-service")))]
    pub(crate) const fn from_env() -> Option<Self> {
        None
    }
}

#[async_trait]
impl IsolationProbe for LinuxVmServiceIsolationProbe {
    async fn probe(&self) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
        let socket = self.socket_path.clone();
        let expected_broker = self.expected_broker.clone();
        spawn_blocking(move || {
            let capabilities = exchange_capabilities(&socket, expected_broker.as_ref())?;
            if expected_broker.is_none() {
                return Err(IsolationProbeError::Unqualified);
            }
            Ok(capabilities)
        })
        .await
        .map_err(|_| IsolationProbeError::Unavailable)?
    }
}

#[derive(Serialize)]
struct RequestEnvelope<'a> {
    version: &'a str,
    id: &'a str,
    operation: &'a str,
    deadline: String,
    payload: EmptyPayload,
}

#[derive(Serialize)]
struct EmptyPayload {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseEnvelope {
    version: String,
    id: String,
    ok: bool,
    #[serde(default)]
    result: Option<CapabilitiesResponse>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CapabilitiesResponse {
    contract_version: String,
    backend: String,
    simulated: bool,
    available: bool,
    operations: Vec<String>,
    workspace_mode: String,
    #[serde(default)]
    reason: String,
    qualification: QualificationEvidence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct QualificationEvidence {
    broker_package_verified: bool,
    signed_guest_catalog_verified: bool,
    guest_image_verified: bool,
    hardware_qualified: bool,
    #[serde(default)]
    evidence_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseError {
    code: String,
    message: String,
    retryable: bool,
}

fn exchange_capabilities(
    socket: &std::path::Path,
    expected_broker: Option<&ExpectedBroker>,
) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
    let socket_identity = expected_broker
        .map(|_| validate_production_socket(socket))
        .transpose()?;
    let mut stream = UnixStream::connect(socket).map_err(|_| IsolationProbeError::Unavailable)?;
    if let (Some(expected), Some(identity)) = (expected_broker, socket_identity) {
        validate_server_identity(&stream, socket, identity, expected)?;
    }
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|_| IsolationProbeError::Unavailable)?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|_| IsolationProbeError::Unavailable)?;
    let id = format!("linux-probe-{}", uuid::Uuid::new_v4());
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| value.as_millis().checked_add(IO_TIMEOUT.as_millis()))
        .ok_or(IsolationProbeError::Protocol)?;
    let mut request = serde_json::to_vec(&RequestEnvelope {
        version: WIRE_VERSION,
        id: &id,
        operation: "get_capabilities",
        deadline: deadline.to_string(),
        payload: EmptyPayload {},
    })
    .map_err(|_| IsolationProbeError::Protocol)?;
    if request.len() >= MAX_FRAME_BYTES {
        return Err(IsolationProbeError::Protocol);
    }
    request.push(b'\n');
    stream
        .write_all(&request)
        .map_err(|_| IsolationProbeError::Unavailable)?;

    let mut reader = io::BufReader::new(stream);
    let mut frame = Vec::new();
    let mut bounded = io::Read::take(
        io::Read::by_ref(&mut reader),
        u64::try_from(MAX_FRAME_BYTES).unwrap_or(u64::MAX) + 1,
    );
    io::BufRead::read_until(&mut bounded, b'\n', &mut frame)
        .map_err(|_| IsolationProbeError::Unavailable)?;
    if frame.len() > MAX_FRAME_BYTES || !frame.ends_with(b"\n") {
        return Err(IsolationProbeError::Protocol);
    }
    decode_response(&frame, &id)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn validate_production_socket(path: &std::path::Path) -> Result<FileIdentity, IsolationProbeError> {
    if path != std::path::Path::new(PRODUCTION_SOCKET) {
        return Err(IsolationProbeError::Unqualified);
    }
    let parent = path.parent().ok_or(IsolationProbeError::Unqualified)?;
    let parent_metadata =
        std::fs::symlink_metadata(parent).map_err(|_| IsolationProbeError::Unavailable)?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != 0
        || parent_metadata.mode() & 0o027 != 0
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| IsolationProbeError::Unavailable)?;
    if !metadata.file_type().is_socket() || metadata.uid() != 0 || metadata.mode() & 0o777 != 0o660
    {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn validate_server_identity(
    stream: &UnixStream,
    socket: &std::path::Path,
    socket_identity: FileIdentity,
    expected: &ExpectedBroker,
) -> Result<(), IsolationProbeError> {
    let credentials = rustix::net::sockopt::socket_peercred(stream)
        .map_err(|_| IsolationProbeError::Unqualified)?;
    if credentials.uid.as_raw() != 0 {
        return Err(IsolationProbeError::Unqualified);
    }
    let pid = credentials.pid.as_raw_nonzero().get();
    let proc_exe = PathBuf::from(format!("/proc/{pid}/exe"));
    let resolved = std::fs::read_link(&proc_exe).map_err(|_| IsolationProbeError::Unqualified)?;
    if resolved != expected.executable {
        return Err(IsolationProbeError::Unqualified);
    }
    let mut executable = File::open(&proc_exe).map_err(|_| IsolationProbeError::Unqualified)?;
    let metadata = executable
        .metadata()
        .map_err(|_| IsolationProbeError::Unqualified)?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(IsolationProbeError::Unqualified);
    }
    let mut hasher = Sha256::new();
    let copied = io::copy(
        &mut std::io::Read::by_ref(&mut executable).take(128 * 1024 * 1024 + 1),
        &mut hasher,
    )
    .map_err(|_| IsolationProbeError::Unqualified)?;
    let actual: [u8; 32] = hasher.finalize().into();
    if copied > 128 * 1024 * 1024 || actual != expected.sha256 {
        return Err(IsolationProbeError::Unqualified);
    }
    let current_socket = validate_production_socket(socket)?;
    if current_socket != socket_identity {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(())
}

fn decode_response(
    frame: &[u8],
    request_id: &str,
) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
    let response: ResponseEnvelope =
        serde_json::from_slice(frame).map_err(|_| IsolationProbeError::Protocol)?;
    if response.version != WIRE_VERSION || response.id != request_id {
        return Err(IsolationProbeError::Protocol);
    }
    if !response.ok {
        let error = response.error.ok_or(IsolationProbeError::Protocol)?;
        if response.result.is_some()
            || error.code.is_empty()
            || error.message.len() > 1024
            || error.retryable && error.code == "protocol"
        {
            return Err(IsolationProbeError::Protocol);
        }
        return Err(IsolationProbeError::Unavailable);
    }
    if response.error.is_some() {
        return Err(IsolationProbeError::Protocol);
    }
    let result = response.result.ok_or(IsolationProbeError::Protocol)?;
    if result.contract_version != CONTRACT_VERSION {
        return Err(IsolationProbeError::Incompatible);
    }
    let mut operations = result.operations;
    operations.sort_unstable();
    if result.backend != "qemu-kvm"
        || result.simulated
        || result.workspace_mode != "read-only-virtio-9p"
        || operations.iter().map(String::as_str).collect::<Vec<_>>() != EXPECTED_OPERATIONS
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let evidence = result.qualification;
    if !result.available
        || !result.reason.is_empty()
        || !evidence.broker_package_verified
        || !evidence.signed_guest_catalog_verified
        || !evidence.guest_image_verified
        || !evidence.hardware_qualified
        || !is_sha256(&evidence.evidence_sha256)
    {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(IsolationBrokerCapabilities {
        contract_version: IsolationContractVersion {
            major: 1,
            minor: 1,
            patch: 0,
        },
        backend: IsolationBackend::QemuKvm,
        hcs_schema: String::new(),
        workspace_mode: IsolationWorkspaceMode::ReadOnlyVirtio9p,
        operations: vec![
            IsolationBrokerOperation::AttachWorkspace,
            IsolationBrokerOperation::CreateVm,
            IsolationBrokerOperation::DeleteVm,
            IsolationBrokerOperation::EnsureImage,
            IsolationBrokerOperation::GetCapabilities,
            IsolationBrokerOperation::StartVm,
            IsolationBrokerOperation::StopVm,
        ],
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn response(overrides: serde_json::Value) -> Vec<u8> {
        let mut value = serde_json::json!({
            "version": "1.0.0",
            "id": "probe-1",
            "ok": true,
            "result": {
                "contractVersion": "1.1.0",
                "backend": "qemu-kvm",
                "simulated": false,
                "available": true,
                "operations": EXPECTED_OPERATIONS,
                "workspaceMode": "read-only-virtio-9p",
                "reason": "",
                "qualification": {
                    "brokerPackageVerified": true,
                    "signedGuestCatalogVerified": true,
                    "guestImageVerified": true,
                    "hardwareQualified": true,
                    "evidenceSha256": "a".repeat(64)
                }
            }
        });
        merge(&mut value, overrides);
        let mut encoded = serde_json::to_vec(&value).expect("response");
        encoded.push(b'\n');
        encoded
    }

    fn merge(target: &mut serde_json::Value, source: serde_json::Value) {
        if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
            for (key, value) in source {
                if let Some(existing) = target.get_mut(key) {
                    merge(existing, value.clone());
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        } else {
            *target = source;
        }
    }

    #[test]
    fn accepts_only_complete_release_qualification_evidence() {
        let capabilities = decode_response(&response(serde_json::json!({})), "probe-1")
            .expect("qualified response");
        assert_eq!(capabilities.backend, IsolationBackend::QemuKvm);
        for field in [
            "brokerPackageVerified",
            "signedGuestCatalogVerified",
            "guestImageVerified",
            "hardwareQualified",
        ] {
            let mut override_value = serde_json::json!({"result":{"qualification":{}}});
            override_value["result"]["qualification"][field] = serde_json::Value::Bool(false);
            let error = decode_response(&response(override_value), "probe-1");
            assert_eq!(error, Err(IsolationProbeError::Unqualified));
        }
    }

    #[test]
    fn rejects_absent_evidence_unavailable_and_protocol_drift() {
        for override_value in [
            serde_json::json!({"result":{"available":false,"reason":"signed_release_evidence_unavailable"}}),
            serde_json::json!({"result":{"qualification":{"evidenceSha256":""}}}),
            serde_json::json!({"result":{"operations":["get_capabilities"]}}),
            serde_json::json!({"id":"wrong"}),
            serde_json::json!({"unknown":true}),
        ] {
            assert!(decode_response(&response(override_value), "probe-1").is_err());
        }
    }

    #[tokio::test]
    async fn same_user_fake_server_cannot_qualify_development_socket() {
        let directory = tempfile::tempdir().expect("socket directory");
        let socket = directory.path().join("fake.sock");
        let listener = UnixListener::bind(&socket).expect("fake listener");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = String::new();
            io::BufRead::read_line(
                &mut io::BufReader::new(stream.try_clone().expect("clone")),
                &mut request,
            )
            .expect("request");
            let request: serde_json::Value = serde_json::from_str(&request).expect("request json");
            let mut forged = response(serde_json::json!({"id": request["id"]}));
            stream.write_all(&forged).expect("forged response");
            forged.fill(0);
        });
        let probe = LinuxVmServiceIsolationProbe {
            socket_path: socket,
            expected_broker: None,
        };
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Unqualified));
        server.join().expect("fake server");
    }
}
