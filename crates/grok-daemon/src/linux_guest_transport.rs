//! Unix-socket guest-control transport for the Linux isolation broker.
//!
//! Wire contract (frozen): JSON-lines envelopes. Go `encoding/json` encodes
//! `[]byte` fields as **standard base64 strings**.
//!
//! Product path (`runner_health`): EnsureImage → CreateVm → StartVm → grant +
//! `runner.health`. Peer identity is resolved by the broker via SO_PEERCRED;
//! any `peerExe` field is diagnostic-only.

use std::{
    env, fs, io,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use grok_application::{PrivilegedGatewayError, PrivilegedGuestControlTransport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::spawn_blocking;

const ENVELOPE_VERSION: &str = "1.0.0";
const MAX_FRAME: usize = 64 * 1024;
const DEFAULT_IMAGE_ID: &str = "guest-v1";
const DEFAULT_IMAGE_REL: &str = "images/guest.raw";
const DEFAULT_LAB_IMAGE: &[u8] = b"grok-linux-lab-guest-image-v1\n";

/// Dials `GROK_LINUX_VM_SOCKET` and issues the product isolation lifecycle.
#[derive(Debug, Clone)]
pub struct LinuxVmServiceGuestTransport {
    socket_path: PathBuf,
    proof: String,
    peer_exe: PathBuf,
    image_root: Option<PathBuf>,
}

impl LinuxVmServiceGuestTransport {
    /// Builds a transport from environment. Returns `None` when unset.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let socket = env::var_os("GROK_LINUX_VM_SOCKET")?;
        let peer = env::current_exe().ok()?;
        let proof = env::var("GROK_LINUX_VM_POP_PROOF")
            .unwrap_or_else(|_| "proof-of-possession-token-isolation-runtime!!".into());
        let image_root = env::var_os("GROK_LINUX_VM_IMAGE_ROOT").map(PathBuf::from);
        Some(Self {
            socket_path: PathBuf::from(socket),
            proof,
            peer_exe: peer,
            image_root,
        })
    }

    /// Explicit constructor for tests and lab harnesses.
    #[must_use]
    pub fn new(socket_path: PathBuf, peer_exe: PathBuf, proof: impl Into<String>) -> Self {
        Self {
            socket_path,
            proof: proof.into(),
            peer_exe,
            image_root: env::var_os("GROK_LINUX_VM_IMAGE_ROOT").map(PathBuf::from),
        }
    }
}

#[async_trait]
impl PrivilegedGuestControlTransport for LinuxVmServiceGuestTransport {
    async fn runner_health(&self, vm_id: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
        let socket = self.socket_path.clone();
        let proof = self.proof.clone();
        let peer = self.peer_exe.clone();
        let image_root = self.image_root.clone();
        let vm = vm_id.to_owned();
        spawn_blocking(move || orchestrate_runner_health(&socket, &peer, image_root.as_deref(), &vm, &proof))
            .await
            .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestEnvelope<P: Serialize> {
    version: &'static str,
    id: String,
    operation: &'static str,
    deadline: String,
    /// Diagnostic only; broker authorizes via SO_PEERCRED + /proc/pid/exe.
    peer_exe: String,
    payload: P,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnsureImagePayload {
    image_id: String,
    relative_path: String,
    sha256: String,
    size_bytes: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateVmPayload {
    vm_id: String,
    image_id: String,
    vcpu_count: u16,
    memory_mi_b: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartVmPayload {
    vm_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GuestControlPayload<'a> {
    vm_id: &'a str,
    method: &'a str,
    proof: &'a str,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ResponseEnvelope {
    ok: bool,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct GuestControlResult {
    #[serde(default, deserialize_with = "deserialize_go_byte_slice")]
    body: Vec<u8>,
    #[serde(default)]
    method: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ResponseError {
    message: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    retryable: bool,
}

fn deserialize_go_byte_slice<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        None => Ok(Vec::new()),
        Some(encoded) if encoded.is_empty() => Ok(Vec::new()),
        Some(encoded) => B64
            .decode(encoded.as_bytes())
            .map_err(|error| serde::de::Error::custom(format!("invalid base64 body: {error}"))),
    }
}

/// Parses one newline-delimited guest_control success frame.
pub(crate) fn parse_response_frame(line: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
    let response = parse_envelope(line)?;
    if !response.ok {
        return Err(map_error(response.error));
    }
    let result = response
        .result
        .ok_or_else(|| PrivilegedGatewayError::Unavailable("empty guest control result".into()))?;
    let decoded: GuestControlResult = serde_json::from_value(result)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    if decoded.body.is_empty() {
        return Err(PrivilegedGatewayError::Unavailable("empty guest health body".into()));
    }
    Ok(decoded.body)
}

fn parse_envelope(line: &str) -> Result<ResponseEnvelope, PrivilegedGatewayError> {
    if line.len() > MAX_FRAME {
        return Err(PrivilegedGatewayError::Transport("response too large".into()));
    }
    serde_json::from_str(line.trim_end())
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))
}

fn map_error(error: Option<ResponseError>) -> PrivilegedGatewayError {
    let message = error
        .map(|error| format!("{}: {}", error.code, error.message))
        .unwrap_or_else(|| "guest control failed".into());
    PrivilegedGatewayError::Unavailable(message)
}

/// Product isolation path: ensure image → create/start VM → grant + health.
fn orchestrate_runner_health(
    socket: &Path,
    peer_exe: &Path,
    image_root: Option<&Path>,
    vm_id: &str,
    proof: &str,
) -> Result<Vec<u8>, PrivilegedGatewayError> {
    let image = prepare_lab_image(image_root)?;
    let mut stream = connect(socket)?;

    // 1) EnsureImage
    exchange_ok(
        &mut stream,
        peer_exe,
        "ensure_image",
        format!("ensure-{vm_id}"),
        &EnsureImagePayload {
            image_id: image.id.clone(),
            relative_path: image.relative_path.clone(),
            sha256: image.sha256.clone(),
            size_bytes: image.size_bytes,
        },
    )?;

    // 2) CreateVm (idempotent: exists is OK)
    let create = exchange_raw(
        &mut stream,
        peer_exe,
        "create_vm",
        format!("create-{vm_id}"),
        &CreateVmPayload {
            vm_id: vm_id.to_owned(),
            image_id: image.id,
            vcpu_count: 2,
            memory_mi_b: 1024,
        },
    )?;
    if !create.ok {
        let msg = create
            .error
            .as_ref()
            .map(|e| e.message.as_str())
            .unwrap_or("");
        if !msg.contains("vm exists") {
            return Err(map_error(create.error));
        }
    }

    // 3) StartVm (spawn required; lab injects fake process)
    exchange_ok(
        &mut stream,
        peer_exe,
        "start_vm",
        format!("start-{vm_id}"),
        &StartVmPayload {
            vm_id: vm_id.to_owned(),
        },
    )?;

    // 4) Grant + runner.health
    let health = exchange_raw(
        &mut stream,
        peer_exe,
        "guest_control",
        format!("guest-health-{vm_id}"),
        &GuestControlPayload {
            vm_id,
            method: "runner.health",
            proof,
        },
    )?;
    if !health.ok {
        return Err(map_error(health.error));
    }
    let result = health
        .result
        .ok_or_else(|| PrivilegedGatewayError::Unavailable("empty guest health result".into()))?;
    let decoded: GuestControlResult = serde_json::from_value(result)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    if decoded.body.is_empty() {
        return Err(PrivilegedGatewayError::Unavailable("empty guest health body".into()));
    }
    Ok(decoded.body)
}

struct LabImage {
    id: String,
    relative_path: String,
    sha256: String,
    size_bytes: i64,
}

fn prepare_lab_image(image_root: Option<&Path>) -> Result<LabImage, PrivilegedGatewayError> {
    let id = env::var("GROK_LINUX_VM_IMAGE_ID").unwrap_or_else(|_| DEFAULT_IMAGE_ID.into());
    let relative_path =
        env::var("GROK_LINUX_VM_IMAGE_REL").unwrap_or_else(|_| DEFAULT_IMAGE_REL.into());
    if let (Ok(sha), Ok(size)) = (
        env::var("GROK_LINUX_VM_IMAGE_SHA256"),
        env::var("GROK_LINUX_VM_IMAGE_SIZE"),
    ) {
        let size_bytes: i64 = size
            .parse()
            .map_err(|_| PrivilegedGatewayError::Unavailable("invalid image size env".into()))?;
        return Ok(LabImage {
            id,
            relative_path,
            sha256: sha,
            size_bytes,
        });
    }
    let root = image_root
        .map(Path::to_path_buf)
        .or_else(|| env::var_os("GROK_LINUX_VM_IMAGE_ROOT").map(PathBuf::from))
        .ok_or_else(|| {
            PrivilegedGatewayError::Unavailable(
                "GROK_LINUX_VM_IMAGE_ROOT required for ensure_image orchestration".into(),
            )
        })?;
    let full = root.join(&relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            PrivilegedGatewayError::Unavailable(format!("image directory: {error}"))
        })?;
    }
    if !full.exists() {
        fs::write(&full, DEFAULT_LAB_IMAGE).map_err(|error| {
            PrivilegedGatewayError::Unavailable(format!("write lab image: {error}"))
        })?;
    }
    let bytes = fs::read(&full)
        .map_err(|error| PrivilegedGatewayError::Unavailable(format!("read lab image: {error}")))?;
    let digest = hex::encode(Sha256::digest(&bytes));
    Ok(LabImage {
        id,
        relative_path,
        sha256: digest,
        size_bytes: i64::try_from(bytes.len()).unwrap_or(0),
    })
}

fn connect(socket: &Path) -> Result<UnixStream, PrivilegedGatewayError> {
    let stream = UnixStream::connect(socket).map_err(|error| {
        PrivilegedGatewayError::Unavailable(format!("linux vm socket connect: {error}"))
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    Ok(stream)
}

fn exchange_ok<P: Serialize>(
    stream: &mut UnixStream,
    peer_exe: &Path,
    operation: &'static str,
    id: String,
    payload: &P,
) -> Result<ResponseEnvelope, PrivilegedGatewayError> {
    let response = exchange_raw(stream, peer_exe, operation, id, payload)?;
    if !response.ok {
        return Err(map_error(response.error));
    }
    Ok(response)
}

fn exchange_raw<P: Serialize>(
    stream: &mut UnixStream,
    peer_exe: &Path,
    operation: &'static str,
    id: String,
    payload: &P,
) -> Result<ResponseEnvelope, PrivilegedGatewayError> {
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() + 10_000)
        .unwrap_or(0);
    let request = RequestEnvelope {
        version: ENVELOPE_VERSION,
        id,
        operation,
        deadline: deadline.to_string(),
        peer_exe: peer_exe.display().to_string(),
        payload,
    };
    let mut encoded = serde_json::to_vec(&request)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    if encoded.len() >= MAX_FRAME {
        return Err(PrivilegedGatewayError::Transport("request too large".into()));
    }
    encoded.push(b'\n');
    io::Write::write_all(stream, &encoded)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;

    let mut reader = io::BufReader::new(&*stream);
    let mut line = String::new();
    io::BufRead::read_line(&mut reader, &mut line)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    parse_envelope(&line)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        process::{Child, Command, Stdio},
        thread,
        time::Duration,
    };

    const SUCCESS_FIXTURE: &str =
        include_str!("../../../native/linux-vm-service/testdata/wire/guest_control_success.jsonl");
    const ERROR_FIXTURE: &str =
        include_str!("../../../native/linux-vm-service/testdata/wire/guest_control_error.jsonl");

    #[test]
    fn deserializes_go_base64_success_body_from_fixture() {
        let body = parse_response_frame(SUCCESS_FIXTURE.trim()).expect("success fixture");
        assert_eq!(
            body,
            br#"{"status":"ok","vm":"work-vm","source":"lab-hook"}"#
        );
    }

    #[test]
    fn deserializes_typed_error_envelope_from_fixture() {
        let error = parse_response_frame(ERROR_FIXTURE.trim()).expect_err("error fixture");
        match error {
            PrivilegedGatewayError::Unavailable(message) => {
                assert!(
                    message.contains("not_found") || message.contains("vm"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn rejects_json_number_array_body_as_transport_error() {
        let bad = r#"{"ok":true,"result":{"method":"runner.health","body":[123,34]}}"#;
        let error = parse_response_frame(bad).expect_err("array body");
        assert!(
            matches!(error, PrivilegedGatewayError::Transport(_)),
            "expected Transport parse failure, got {error:?}"
        );
    }

    /// Live product path: EnsureImage → Create/StartVm → grant + health via socket.
    #[test]
    fn socket_smoke_orchestrates_ensure_create_start_health() {
        let socket = std::env::temp_dir().join(format!(
            "grok-linux-vm-orch-smoke-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let image_root = tempfile::tempdir().expect("image root");
        let peer = std::env::current_exe().expect("peer exe");
        // Resolve peer path the way /proc/pid/exe will (symlink-eval).
        let peer = peer.canonicalize().unwrap_or(peer);

        let mut child = spawn_vm_service(&socket, image_root.path(), &peer);
        wait_for_socket(&socket, Duration::from_secs(8));

        // Ensure ALLOWED_DAEMON matches SO_PEERCRED peer of this test process.
        let transport = LinuxVmServiceGuestTransport::new(
            socket.clone(),
            peer.clone(),
            "proof-of-possession-token-isolation-runtime!!",
        );
        let body = orchestrate_runner_health(
            &transport.socket_path,
            &transport.peer_exe,
            Some(image_root.path()),
            "work-vm",
            &transport.proof,
        )
        .expect("orchestrated health must succeed on lab spawn path");
        assert!(
            !body.is_empty(),
            "health body must be non-empty after orchestration"
        );
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("ok") || text.contains("work-vm"),
            "unexpected health body: {text}"
        );

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&socket);
    }

    fn spawn_vm_service(socket: &Path, image_root: &Path, peer: &Path) -> Child {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        Command::new("go")
            .current_dir(workspace.join("native/linux-vm-service"))
            .env("GROK_LINUX_VM_SOCKET", socket)
            .env("GROK_LINUX_VM_IMAGE_ROOT", image_root)
            .env("GROK_LINUX_VM_ALLOWED_DAEMON", peer)
            .env("GROK_LINUX_VM_LAB_HEALTH", "ok")
            .env("GROK_LINUX_VM_LAB_SPAWN", "1")
            .env("GROK_LINUX_VM_REQUIRE_KVM", "0")
            .arg("run")
            .arg("./cmd/grok-linux-vm-service")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start grok-linux-vm-service")
    }

    fn wait_for_socket(path: &Path, timeout: Duration) {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if path.exists() {
                thread::sleep(Duration::from_millis(80));
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("socket {} did not appear", path.display());
    }
}
