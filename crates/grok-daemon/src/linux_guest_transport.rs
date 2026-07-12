//! Unix-socket guest-control transport for the Linux isolation broker.

use std::{
    env, io,
    os::unix::net::UnixStream,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
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

#[derive(Deserialize)]
struct ResponseEnvelope {
    ok: bool,
    #[serde(default)]
    result: Option<GuestControlResult>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Deserialize)]
struct GuestControlResult {
    #[serde(default)]
    body: Vec<u8>,
    #[serde(default)]
    #[allow(dead_code)]
    method: String,
}

#[derive(Deserialize)]
struct ResponseError {
    message: String,
    #[serde(default)]
    code: String,
}

fn exchange_runner_health(
    socket: &PathBuf,
    peer_exe: &PathBuf,
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
        id: format!("guest-health-{}", vm_id),
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
