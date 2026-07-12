//! Closed scheduled-work guest dispatcher for the qualified Linux broker.

use std::{
    io::{self, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use grok_application::{
    ScheduledGuestDispatchError, ScheduledGuestDispatcher, ScheduledGuestOutcome,
    ScheduledGuestRequest,
};
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const WIRE_VERSION: &str = "1.0.0";
const METHOD: &str = "scheduled.run";
const PAYLOAD_MAGIC: &[u8; 8] = b"GRKSCH01";
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_FRAME_BYTES: usize = 128 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(all(debug_assertions, feature = "debug-linux-vm-service"))]
const PRODUCTION_SOCKET: &str = "/run/grok-desktop/linux-vm-service.sock";

#[derive(Clone)]
pub(crate) struct LinuxScheduledGuestDispatcher {
    socket_path: PathBuf,
    vm_id: String,
    proof: Zeroizing<String>,
}

impl std::fmt::Debug for LinuxScheduledGuestDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxScheduledGuestDispatcher")
            .field("socket_path", &self.socket_path)
            .field("vm_id", &self.vm_id)
            .field("proof", &"[REDACTED]")
            .finish()
    }
}

impl LinuxScheduledGuestDispatcher {
    #[cfg(all(debug_assertions, feature = "debug-linux-vm-service"))]
    pub(crate) fn from_env() -> Option<Self> {
        let proof = std::env::var("GROK_LINUX_VM_POP_PROOF").ok()?;
        let socket_path = std::env::var_os("GROK_LINUX_VM_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(PRODUCTION_SOCKET));
        Self::new(socket_path, "work-vm".into(), proof)
    }

    #[cfg(not(all(debug_assertions, feature = "debug-linux-vm-service")))]
    pub(crate) const fn from_env() -> Option<Self> {
        None
    }

    pub(crate) fn new(socket_path: PathBuf, vm_id: String, proof: String) -> Option<Self> {
        if !socket_path.is_absolute()
            || !valid_id(&vm_id)
            || proof.len() < 32
            || proof.len() > 4096
            || proof.contains('\0')
        {
            return None;
        }
        Some(Self {
            socket_path,
            vm_id,
            proof: Zeroizing::new(proof),
        })
    }
}

#[async_trait]
impl ScheduledGuestDispatcher for LinuxScheduledGuestDispatcher {
    async fn dispatch(
        &self,
        request: ScheduledGuestRequest,
        cancellation: CancellationToken,
    ) -> Result<ScheduledGuestOutcome, ScheduledGuestDispatchError> {
        if cancellation.is_cancelled() {
            return Err(ScheduledGuestDispatchError::Unavailable);
        }
        let payload = encode_payload(request)?;
        let dispatched = Arc::new(AtomicBool::new(false));
        let active_stream = Arc::new(Mutex::new(None::<UnixStream>));
        let socket = self.socket_path.clone();
        let vm_id = self.vm_id.clone();
        let proof = self.proof.clone();
        let task_dispatched = dispatched.clone();
        let task_stream = active_stream.clone();
        let task_cancellation = cancellation.clone();
        let mut task = spawn_blocking(move || {
            exchange(
                socket,
                vm_id,
                proof,
                payload,
                task_cancellation,
                task_dispatched,
                task_stream,
            )
        });
        tokio::select! {
            result = &mut task => result.map_err(|_| ScheduledGuestDispatchError::Interrupted)?,
            () = cancellation.cancelled() => {
                if let Ok(guard) = active_stream.lock()
                    && let Some(stream) = guard.as_ref()
                {
                    let _ = stream.shutdown(Shutdown::Both);
                }
                let write_started = dispatched.load(Ordering::Acquire);
                let _ = task.await;
                if write_started || dispatched.load(Ordering::Acquire) {
                    Err(ScheduledGuestDispatchError::Interrupted)
                } else {
                    Err(ScheduledGuestDispatchError::Unavailable)
                }
            }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestEnvelope<'a> {
    version: &'static str,
    id: &'a str,
    operation: &'static str,
    deadline: String,
    payload: GuestControlPayload<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GuestControlPayload<'a> {
    vm_id: &'a str,
    method: &'static str,
    proof: &'a str,
    payload: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseEnvelope {
    version: String,
    id: String,
    ok: bool,
    #[serde(default)]
    result: Option<ScheduledResult>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduledResult {
    method: String,
    outcome: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseError {
    code: String,
    message: String,
    retryable: bool,
}

#[allow(clippy::needless_pass_by_value)]
fn exchange(
    socket: PathBuf,
    vm_id: String,
    proof: Zeroizing<String>,
    payload: Zeroizing<Vec<u8>>,
    cancellation: CancellationToken,
    dispatched: Arc<AtomicBool>,
    active_stream: Arc<Mutex<Option<UnixStream>>>,
) -> Result<ScheduledGuestOutcome, ScheduledGuestDispatchError> {
    if cancellation.is_cancelled() {
        return Err(ScheduledGuestDispatchError::Unavailable);
    }
    let mut stream =
        UnixStream::connect(socket).map_err(|_| ScheduledGuestDispatchError::Unavailable)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|_| ScheduledGuestDispatchError::Unavailable)?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|_| ScheduledGuestDispatchError::Unavailable)?;
    let cloned = stream
        .try_clone()
        .map_err(|_| ScheduledGuestDispatchError::Unavailable)?;
    *active_stream
        .lock()
        .map_err(|_| ScheduledGuestDispatchError::Unavailable)? = Some(cloned);
    if cancellation.is_cancelled() {
        return Err(ScheduledGuestDispatchError::Unavailable);
    }
    let id = format!("scheduled-{}", uuid::Uuid::new_v4());
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| value.as_millis().checked_add(IO_TIMEOUT.as_millis()))
        .ok_or(ScheduledGuestDispatchError::Unavailable)?;
    let encoded_payload = Zeroizing::new(B64.encode(payload.as_slice()));
    let mut frame = Zeroizing::new(
        serde_json::to_vec(&RequestEnvelope {
            version: WIRE_VERSION,
            id: &id,
            operation: "guest_control",
            deadline: deadline.to_string(),
            payload: GuestControlPayload {
                vm_id: &vm_id,
                method: METHOD,
                proof: proof.as_str(),
                payload: encoded_payload.as_str(),
            },
        })
        .map_err(|_| ScheduledGuestDispatchError::Unavailable)?,
    );
    if frame.len() >= MAX_FRAME_BYTES {
        return Err(ScheduledGuestDispatchError::Unavailable);
    }
    frame.push(b'\n');
    write_dispatched_frame(&mut stream, &frame, dispatched.as_ref())?;

    let mut reader = io::BufReader::new(stream);
    let mut response = Zeroizing::new(Vec::new());
    let mut bounded = io::Read::take(
        io::Read::by_ref(&mut reader),
        u64::try_from(MAX_FRAME_BYTES).unwrap_or(u64::MAX) + 1,
    );
    io::BufRead::read_until(&mut bounded, b'\n', &mut response)
        .map_err(|_| ScheduledGuestDispatchError::Interrupted)?;
    if response.len() > MAX_FRAME_BYTES || !response.ends_with(b"\n") {
        return Err(ScheduledGuestDispatchError::Interrupted);
    }
    decode_response(&response, &id)
}

fn encode_payload(
    request: ScheduledGuestRequest,
) -> Result<Zeroizing<Vec<u8>>, ScheduledGuestDispatchError> {
    let occurrence = request.occurrence_id.as_str().as_bytes();
    let run = request.run_id.as_str().as_bytes();
    let prompt = Zeroizing::new(request.prompt);
    let prompt = prompt.as_bytes();
    if !valid_id_bytes(occurrence)
        || !valid_id_bytes(run)
        || prompt.is_empty()
        || prompt.len() > MAX_PROMPT_BYTES
        || prompt.contains(&0)
    {
        return Err(ScheduledGuestDispatchError::Unavailable);
    }
    let mut payload = Vec::with_capacity(16 + occurrence.len() + run.len() + prompt.len());
    payload.extend_from_slice(PAYLOAD_MAGIC);
    payload.extend_from_slice(
        &u16::try_from(occurrence.len())
            .map_err(|_| ScheduledGuestDispatchError::Unavailable)?
            .to_be_bytes(),
    );
    payload.extend_from_slice(occurrence);
    payload.extend_from_slice(
        &u16::try_from(run.len())
            .map_err(|_| ScheduledGuestDispatchError::Unavailable)?
            .to_be_bytes(),
    );
    payload.extend_from_slice(run);
    payload.extend_from_slice(
        &u32::try_from(prompt.len())
            .map_err(|_| ScheduledGuestDispatchError::Unavailable)?
            .to_be_bytes(),
    );
    payload.extend_from_slice(prompt);
    Ok(Zeroizing::new(payload))
}

fn write_dispatched_frame(
    writer: &mut impl Write,
    frame: &[u8],
    write_started: &AtomicBool,
) -> Result<(), ScheduledGuestDispatchError> {
    write_started.store(true, Ordering::Release);
    writer
        .write_all(frame)
        .map_err(|_| ScheduledGuestDispatchError::Interrupted)
}

fn decode_response(
    frame: &[u8],
    request_id: &str,
) -> Result<ScheduledGuestOutcome, ScheduledGuestDispatchError> {
    let response: ResponseEnvelope =
        serde_json::from_slice(frame).map_err(|_| ScheduledGuestDispatchError::Interrupted)?;
    if response.version != WIRE_VERSION || response.id != request_id || !response.ok {
        if let Some(error) = response.error {
            let _ = (error.code, error.message, error.retryable);
        }
        return Err(ScheduledGuestDispatchError::Interrupted);
    }
    if response.error.is_some() {
        return Err(ScheduledGuestDispatchError::Interrupted);
    }
    let result = response
        .result
        .ok_or(ScheduledGuestDispatchError::Interrupted)?;
    if result.method != METHOD {
        return Err(ScheduledGuestDispatchError::Interrupted);
    }
    match result.outcome.as_str() {
        "succeeded" => Ok(ScheduledGuestOutcome::Succeeded),
        "failed" => Ok(ScheduledGuestOutcome::Failed),
        _ => Err(ScheduledGuestDispatchError::Interrupted),
    }
}

fn valid_id(value: &str) -> bool {
    valid_id_bytes(value.as_bytes())
}

fn valid_id_bytes(value: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value[0].is_ascii_alphanumeric()
        && value
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grok_domain::{AutomationOccurrenceId, RunId};
    use std::{io::Read as _, os::unix::net::UnixListener, sync::mpsc};

    fn request(prompt: String) -> ScheduledGuestRequest {
        ScheduledGuestRequest {
            occurrence_id: AutomationOccurrenceId::new("occurrence-1").expect("occurrence"),
            run_id: RunId::new("run-1").expect("run"),
            prompt,
        }
    }

    #[test]
    fn encodes_exact_prompt_maximum_without_json_escape_expansion() {
        #[cfg(not(all(debug_assertions, feature = "debug-linux-vm-service")))]
        assert!(LinuxScheduledGuestDispatcher::from_env().is_none());
        let payload = encode_payload(request("x".repeat(MAX_PROMPT_BYTES))).expect("maximum");
        assert_eq!(
            payload.len(),
            8 + 2 + "occurrence-1".len() + 2 + "run-1".len() + 4 + MAX_PROMPT_BYTES
        );
        let encoded_length = 4 * payload.len().div_ceil(3);
        assert!(encoded_length < MAX_FRAME_BYTES);
        assert_eq!(
            encode_payload(request("x".repeat(MAX_PROMPT_BYTES + 1))),
            Err(ScheduledGuestDispatchError::Unavailable)
        );
    }

    #[allow(clippy::items_after_statements)]
    #[test]
    fn partial_write_is_ambiguous_and_debug_redacts_secrets() {
        struct PartialWriter(bool);
        impl Write for PartialWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 {
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
                } else {
                    self.0 = true;
                    Ok(bytes.len().min(3))
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let phase = AtomicBool::new(false);
        assert_eq!(
            write_dispatched_frame(&mut PartialWriter(false), b"secret-frame", &phase),
            Err(ScheduledGuestDispatchError::Interrupted)
        );
        assert!(phase.load(Ordering::Acquire));

        const PROOF: &str = "sentinel-proof-credential-12345678901234567890";
        let dispatcher = LinuxScheduledGuestDispatcher::new(
            PathBuf::from("/run/test.sock"),
            "work-vm".into(),
            PROOF.into(),
        )
        .expect("dispatcher");
        let diagnostic = format!(
            "{dispatcher:?} {:?}",
            ScheduledGuestDispatchError::Interrupted
        );
        assert!(!diagnostic.contains(PROOF));
        assert!(diagnostic.contains("[REDACTED]"));
        const PROMPT: &str = "sentinel-prompt-must-not-enter-errors\0";
        let error = encode_payload(request(PROMPT.into())).expect_err("NUL prompt");
        assert!(!format!("{error:?}").contains(PROMPT));
    }

    #[test]
    fn malformed_or_unknown_terminal_response_is_interrupted() {
        let responses = [
            "{\"version\":\"1.0.0\",\"id\":\"request\",\"ok\":true,\"result\":{\"method\":\"scheduled.run\",\"outcome\":\"unknown\"}}\n",
            "{\"version\":\"1.0.0\",\"id\":\"wrong\",\"ok\":true,\"result\":{\"method\":\"scheduled.run\",\"outcome\":\"succeeded\"}}\n",
            "{\"version\":\"1.0.0\",\"id\":\"request\",\"ok\":false,\"error\":{\"code\":\"unavailable\",\"message\":\"no guest\",\"retryable\":true}}\n",
            "not-json\n",
        ];
        for response in responses {
            assert_eq!(
                decode_response(response.as_bytes(), "request"),
                Err(ScheduledGuestDispatchError::Interrupted)
            );
        }
    }

    #[tokio::test]
    async fn cancellation_after_write_joins_worker_and_returns_interrupted() {
        let directory = tempfile::tempdir().expect("socket directory");
        let socket = directory.path().join("scheduled.sock");
        let listener = UnixListener::bind(&socket).expect("listener");
        let (received_tx, received_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            io::BufRead::read_until(
                &mut io::BufReader::new(stream.try_clone().expect("clone")),
                b'\n',
                &mut request,
            )
            .expect("request");
            received_tx.send(()).expect("received");
            let mut byte = [0_u8; 1];
            let _ = stream.read(&mut byte);
            finished_tx.send(()).expect("finished");
        });
        let dispatcher = LinuxScheduledGuestDispatcher::new(
            socket,
            "work-vm".into(),
            "proof-of-possession-token-isolation-runtime!!".into(),
        )
        .expect("dispatcher");
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let dispatch = tokio::spawn(async move {
            dispatcher
                .dispatch(request("prompt-sentinel".into()), cancellation)
                .await
        });
        spawn_blocking(move || received_rx.recv_timeout(Duration::from_secs(2)))
            .await
            .expect("receive join")
            .expect("request crossed");
        cancel.cancel();
        assert_eq!(
            dispatch.await.expect("join"),
            Err(ScheduledGuestDispatchError::Interrupted)
        );
        spawn_blocking(move || finished_rx.recv_timeout(Duration::from_secs(2)))
            .await
            .expect("finish join")
            .expect("worker socket closed");
        server.join().expect("server");
    }
}
