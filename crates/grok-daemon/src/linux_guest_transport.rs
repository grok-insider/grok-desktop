//! Unix-socket guest-control transport for the Linux isolation broker.
//!
//! Wire contract (frozen): JSON-lines envelopes. Go `encoding/json` encodes
//! `[]byte` fields as **standard base64 strings**. Rust must decode those
//! strings; never expect a JSON array of numbers for `body`.

use std::{
    env, io,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use grok_application::{PrivilegedGatewayError, PrivilegedGuestControlTransport};
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

const ENVELOPE_VERSION: &str = "1.0.0";
const MAX_FRAME: usize = 64 * 1024;

/// Dials `GROK_LINUX_VM_SOCKET` and issues grant + `runner.health`.
#[derive(Debug, Clone)]
pub struct LinuxVmServiceGuestTransport {
    socket_path: PathBuf,
    proof: String,
    peer_exe: PathBuf,
}

impl LinuxVmServiceGuestTransport {
    /// Builds a transport from environment. Returns `None` when unset.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let socket = env::var_os("GROK_LINUX_VM_SOCKET")?;
        let peer = env::current_exe().ok()?;
        let proof = env::var("GROK_LINUX_VM_POP_PROOF")
            .unwrap_or_else(|_| "proof-of-possession-token-isolation-runtime!!".into());
        Some(Self {
            socket_path: PathBuf::from(socket),
            proof,
            peer_exe: peer,
        })
    }

    /// Explicit constructor for tests and lab harnesses.
    #[must_use]
    pub fn new(socket_path: PathBuf, peer_exe: PathBuf, proof: impl Into<String>) -> Self {
        Self {
            socket_path,
            proof: proof.into(),
            peer_exe,
        }
    }
}

#[async_trait]
impl PrivilegedGuestControlTransport for LinuxVmServiceGuestTransport {
    async fn runner_health(&self, vm_id: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
        let socket = self.socket_path.clone();
        let proof = self.proof.clone();
        let peer = self.peer_exe.clone();
        let vm = vm_id.to_owned();
        spawn_blocking(move || exchange_runner_health(&socket, &peer, &vm, &proof))
            .await
            .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestEnvelope<'a> {
    version: &'a str,
    id: String,
    operation: &'a str,
    deadline: String,
    /// Absolute path of the connecting daemon binary for broker peer allowlist.
    /// Residual: production should migrate to SCM_CREDENTIALS (not client-supplied path alone).
    peer_exe: String,
    payload: GuestControlPayload<'a>,
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
    result: Option<GuestControlResult>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct GuestControlResult {
    /// Go `encoding/json` marshals `[]byte` as a standard base64 string.
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
    // Accept omitted/null as empty, string as standard base64 (Go default).
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        None => Ok(Vec::new()),
        Some(encoded) if encoded.is_empty() => Ok(Vec::new()),
        Some(encoded) => B64
            .decode(encoded.as_bytes())
            .map_err(|error| serde::de::Error::custom(format!("invalid base64 body: {error}"))),
    }
}

/// Parses one newline-delimited response frame (fixture-tested).
pub(crate) fn parse_response_frame(line: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
    if line.len() > MAX_FRAME {
        return Err(PrivilegedGatewayError::Transport("response too large".into()));
    }
    let response: ResponseEnvelope = serde_json::from_str(line.trim_end())
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    if !response.ok {
        let message = response
            .error
            .map(|error| format!("{}: {}", error.code, error.message))
            .unwrap_or_else(|| "guest control failed".into());
        return Err(PrivilegedGatewayError::Unavailable(message));
    }
    let body = response
        .result
        .map(|result| result.body)
        .filter(|body| !body.is_empty())
        .ok_or_else(|| PrivilegedGatewayError::Unavailable("empty guest health body".into()))?;
    Ok(body)
}

fn exchange_runner_health(
    socket: &Path,
    peer_exe: &Path,
    vm_id: &str,
    proof: &str,
) -> Result<Vec<u8>, PrivilegedGatewayError> {
    let mut stream = UnixStream::connect(socket).map_err(|error| {
        PrivilegedGatewayError::Unavailable(format!("linux vm socket connect: {error}"))
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;

    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() + 5_000)
        .unwrap_or(0);
    let request = RequestEnvelope {
        version: ENVELOPE_VERSION,
        id: format!("guest-health-{vm_id}"),
        operation: "guest_control",
        deadline: deadline.to_string(),
        peer_exe: peer_exe.display().to_string(),
        payload: GuestControlPayload {
            vm_id,
            method: "runner.health",
            proof,
        },
    };
    let mut encoded = serde_json::to_vec(&request)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    if encoded.len() >= MAX_FRAME {
        return Err(PrivilegedGatewayError::Transport("request too large".into()));
    }
    encoded.push(b'\n');
    io::Write::write_all(&mut stream, &encoded)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;

    let mut reader = io::BufReader::new(&stream);
    let mut line = String::new();
    io::BufRead::read_line(&mut reader, &mut line)
        .map_err(|error| PrivilegedGatewayError::Transport(error.to_string()))?;
    parse_response_frame(&line)
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
        // Document the frozen contract: array bodies are invalid (would have been the old bug).
        let bad = r#"{"ok":true,"result":{"method":"runner.health","body":[123,34]}}"#;
        let error = parse_response_frame(bad).expect_err("array body");
        assert!(
            matches!(error, PrivilegedGatewayError::Transport(_)),
            "expected Transport parse failure, got {error:?}"
        );
    }

    /// Live socket smoke: parse typed broker errors (no QEMU required).
    #[test]
    fn socket_smoke_parses_guest_control_error_envelope() {
        let socket = std::env::temp_dir().join(format!(
            "grok-linux-vm-wire-smoke-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let image_root = tempfile::tempdir().expect("image root");
        let peer = std::env::current_exe().expect("peer exe");
        let mut child = spawn_vm_service(&socket, image_root.path(), &peer);
        wait_for_socket(&socket, Duration::from_secs(5));

        let transport = LinuxVmServiceGuestTransport::new(
            socket.clone(),
            peer,
            "proof-of-possession-token-isolation-runtime!!",
        );
        let result = exchange_runner_health(
            &transport.socket_path,
            &transport.peer_exe,
            "work-vm",
            &transport.proof,
        );
        // Without a created/running VM the broker returns typed unavailable/not_found.
        // Parse failure (Transport) would mean the wire codec is still wrong.
        match result {
            Err(PrivilegedGatewayError::Unavailable(message)) => {
                assert!(
                    message.contains("not_found")
                        || message.contains("unavailable")
                        || message.contains("unauthorized")
                        || message.contains("vm"),
                    "unexpected broker message: {message}"
                );
            }
            Ok(body) => {
                // Lab may inject health; body must still be real bytes, not empty.
                assert!(!body.is_empty());
            }
            Err(other) => panic!("socket smoke must not fail codec: {other:?}"),
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&socket);
    }

    fn spawn_vm_service(socket: &Path, image_root: &Path, peer: &Path) -> Child {
        // Prefer workspace-relative go module entry.
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        Command::new("go")
            .current_dir(workspace.join("native/linux-vm-service"))
            .env("GROK_LINUX_VM_SOCKET", socket)
            .env("GROK_LINUX_VM_IMAGE_ROOT", image_root)
            .env("GROK_LINUX_VM_ALLOWED_DAEMON", peer)
            .env("GROK_LINUX_VM_LAB_HEALTH", "ok")
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
                // brief settle for listen
                thread::sleep(Duration::from_millis(50));
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("socket {} did not appear", path.display());
    }
}
