#![deny(unsafe_code)]
#![warn(missing_docs)]

//! Strict, read-only client for probing the Grok Desktop Windows VM service.
//!
//! This crate deliberately exposes no VM lifecycle or guest-control method. A
//! successful probe is a static compatibility fact about the exact packaged
//! broker service, not execution authority.

use std::{collections::BTreeSet, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use chrono::{SecondsFormat, TimeZone, Utc};
use grok_application::{
    IsolationBackend, IsolationBrokerCapabilities, IsolationBrokerOperation,
    IsolationContractVersion, IsolationProbe, IsolationProbeError, IsolationWorkspaceMode,
};
use serde::{Deserialize, Serialize};

mod platform;
#[cfg(any(windows, test))]
mod service_policy;
mod transport;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_identity;

use transport::{MAX_FRAME_BYTES, ProbeTransport};

const ENVELOPE_VERSION: &str = "1.0.0";
const CONTRACT_VERSION: &str = "1.1.0";
const BACKEND: &str = "hcs-virtual-machine-platform";
const HCS_SCHEMA: &str = "2.1";
const WORKSPACE_MODE: &str = "read-only-plan9";
const PROBE_DEADLINE_MILLIS: u64 = 3_000;
const MAX_ERROR_MESSAGE_BYTES: usize = 1_024;

/// Read-only isolation-broker probe backed by the fixed platform transport.
pub struct VmServiceIsolationProbe {
    transport: Arc<dyn ProbeTransport>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    request_id: Arc<dyn Fn() -> String + Send + Sync>,
}

impl std::fmt::Debug for VmServiceIsolationProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VmServiceIsolationProbe")
            .finish_non_exhaustive()
    }
}

impl VmServiceIsolationProbe {
    /// Creates a probe for the fixed production VM-service endpoint.
    #[must_use]
    pub fn new() -> Self {
        Self {
            transport: platform::transport(),
            now: Arc::new(system_time_millis),
            request_id: Arc::new(|| format!("probe-{}", uuid::Uuid::new_v4())),
        }
    }

    async fn execute(&self) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
        let request_id = (self.request_id)();
        if !valid_request_id(&request_id) {
            return Err(IsolationProbeError::Protocol);
        }
        let deadline_millis = (self.now)()
            .checked_add(PROBE_DEADLINE_MILLIS)
            .ok_or(IsolationProbeError::Protocol)?;
        let deadline = Utc
            .timestamp_millis_opt(
                i64::try_from(deadline_millis).map_err(|_| IsolationProbeError::Protocol)?,
            )
            .single()
            .ok_or(IsolationProbeError::Protocol)?
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut encoded = serde_json::to_vec(&RequestEnvelope {
            version: ENVELOPE_VERSION,
            id: &request_id,
            operation: "get_capabilities",
            deadline: &deadline,
            payload: EmptyPayload {},
        })
        .map_err(|_| IsolationProbeError::Protocol)?;
        if encoded.len() >= MAX_FRAME_BYTES {
            return Err(IsolationProbeError::Protocol);
        }
        encoded.push(b'\n');
        let response = self.transport.exchange(&encoded, MAX_FRAME_BYTES).await?;
        decode_response(&response, &request_id)
    }
}

impl Default for VmServiceIsolationProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IsolationProbe for VmServiceIsolationProbe {
    async fn probe(&self) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
        self.execute().await
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestEnvelope<'a> {
    version: &'a str,
    id: &'a str,
    operation: &'a str,
    deadline: &'a str,
    payload: EmptyPayload,
}

#[derive(Serialize)]
struct EmptyPayload {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseEnvelope {
    version: String,
    #[serde(default)]
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
    #[serde(default)]
    hcs_schema: String,
    operations: Vec<String>,
    workspace_mode: String,
    socket_purposes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseError {
    code: String,
    message: String,
    retryable: bool,
}

fn decode_response(
    frame: &[u8],
    expected_id: &str,
) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
    if frame.len() < 2 || frame.len() > MAX_FRAME_BYTES || frame.last() != Some(&b'\n') {
        return Err(IsolationProbeError::Protocol);
    }
    let body = &frame[..frame.len() - 1];
    if body.contains(&b'\n') || body.contains(&b'\r') {
        return Err(IsolationProbeError::Protocol);
    }
    let response: ResponseEnvelope =
        serde_json::from_slice(body).map_err(|_| IsolationProbeError::Protocol)?;
    if response.version != ENVELOPE_VERSION || response.id != expected_id {
        return Err(IsolationProbeError::Protocol);
    }
    match (response.ok, response.result, response.error) {
        (true, Some(result), None) => qualify(&result),
        (false, None, Some(error)) => map_service_error(&error),
        _ => Err(IsolationProbeError::Protocol),
    }
}

fn qualify(
    response: &CapabilitiesResponse,
) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
    if response.contract_version != CONTRACT_VERSION {
        return Err(IsolationProbeError::Incompatible);
    }
    if response.backend != BACKEND
        || response.simulated
        || !response.available
        || response.hcs_schema != HCS_SCHEMA
        || response.workspace_mode != WORKSPACE_MODE
        || !response.socket_purposes.is_empty()
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let operations = response
        .operations
        .iter()
        .map(|operation| operation_from_wire(operation))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let expected = expected_operations();
    if operations != expected || response.operations.len() != expected.len() {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(IsolationBrokerCapabilities {
        contract_version: IsolationContractVersion {
            major: 1,
            minor: 1,
            patch: 0,
        },
        backend: IsolationBackend::HcsVirtualMachinePlatform,
        hcs_schema: HCS_SCHEMA.into(),
        workspace_mode: IsolationWorkspaceMode::ReadOnlyPlan9,
        operations: operations.into_iter().collect(),
    })
}

fn expected_operations() -> BTreeSet<IsolationBrokerOperation> {
    [
        IsolationBrokerOperation::GetCapabilities,
        IsolationBrokerOperation::EnsureImage,
        IsolationBrokerOperation::CreateVm,
        IsolationBrokerOperation::StartVm,
        IsolationBrokerOperation::StopVm,
        IsolationBrokerOperation::DeleteVm,
        IsolationBrokerOperation::AttachWorkspace,
    ]
    .into_iter()
    .collect()
}

fn operation_from_wire(value: &str) -> Result<IsolationBrokerOperation, IsolationProbeError> {
    match value {
        "get_capabilities" => Ok(IsolationBrokerOperation::GetCapabilities),
        "ensure_image" => Ok(IsolationBrokerOperation::EnsureImage),
        "create_vm" => Ok(IsolationBrokerOperation::CreateVm),
        "start_vm" => Ok(IsolationBrokerOperation::StartVm),
        "stop_vm" => Ok(IsolationBrokerOperation::StopVm),
        "delete_vm" => Ok(IsolationBrokerOperation::DeleteVm),
        "attach_workspace" => Ok(IsolationBrokerOperation::AttachWorkspace),
        _ => Err(IsolationProbeError::Unqualified),
    }
}

fn map_service_error(
    error: &ResponseError,
) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
    if error.code.is_empty()
        || error.code.len() > 64
        || !error
            .code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || error.message.is_empty()
        || error.message.len() > MAX_ERROR_MESSAGE_BYTES
    {
        return Err(IsolationProbeError::Protocol);
    }
    match error.code.as_str() {
        "unsupported_version" if !error.retryable => Err(IsolationProbeError::Incompatible),
        "permission_denied" if !error.retryable => Err(IsolationProbeError::Unqualified),
        "deadline_exceeded" | "server_busy" | "unavailable" if error.retryable => {
            Err(IsolationProbeError::Unavailable)
        }
        _ => Err(IsolationProbeError::Protocol),
    }
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn system_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const CAPABILITIES_FIXTURE: &str =
        include_str!("../../../native/windows-vm-service/testdata/capabilities-1.1.0.json");

    #[derive(Default)]
    struct RecordingTransport {
        response: Mutex<Vec<u8>>,
        request: Mutex<Vec<u8>>,
    }

    #[async_trait]
    impl ProbeTransport for RecordingTransport {
        async fn exchange(
            &self,
            request: &[u8],
            maximum_response_bytes: usize,
        ) -> Result<Vec<u8>, IsolationProbeError> {
            assert_eq!(maximum_response_bytes, MAX_FRAME_BYTES);
            *self.request.lock().expect("request lock") = request.to_vec();
            Ok(self.response.lock().expect("response lock").clone())
        }
    }

    fn test_probe(response: &str) -> (VmServiceIsolationProbe, Arc<RecordingTransport>) {
        let transport = Arc::new(RecordingTransport {
            response: Mutex::new(response.as_bytes().to_vec()),
            request: Mutex::new(Vec::new()),
        });
        (
            VmServiceIsolationProbe {
                transport: transport.clone(),
                now: Arc::new(|| 1_784_458_800_000),
                request_id: Arc::new(|| "probe-request-0001".into()),
            },
            transport,
        )
    }

    fn successful_response(overrides: &str) -> String {
        let fixture = CAPABILITIES_FIXTURE.trim();
        let result = if overrides.is_empty() {
            fixture.to_owned()
        } else {
            format!(
                "{}{overrides}}}",
                fixture
                    .strip_suffix('}')
                    .expect("capabilities fixture object")
            )
        };
        format!(
            "{{\"version\":\"1.0.0\",\"id\":\"probe-request-0001\",\"ok\":true,\"result\":{result}}}\n"
        )
    }

    #[tokio::test]
    async fn sends_only_the_read_only_capability_operation() {
        let (probe, transport) = test_probe(&successful_response(""));
        let capabilities = probe.probe().await.expect("qualified probe");
        assert_eq!(
            capabilities.backend,
            IsolationBackend::HcsVirtualMachinePlatform
        );
        let request = transport.request.lock().expect("request lock").clone();
        assert_eq!(request.last(), Some(&b'\n'));
        let value: serde_json::Value =
            serde_json::from_slice(&request[..request.len() - 1]).expect("request JSON");
        assert_eq!(value["operation"], "get_capabilities");
        assert_eq!(value["payload"], serde_json::json!({}));
        assert!(value.get("idempotencyKey").is_none());
        assert_eq!(value["deadline"], "2026-07-19T11:00:03.000Z");
    }

    #[tokio::test]
    async fn rejects_simulators_unknown_operations_and_duplicate_operations() {
        let simulator =
            successful_response("").replace("\"simulated\":false", "\"simulated\":true");
        let (probe, _) = test_probe(&simulator);
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Unqualified));

        let unknown_field = successful_response(",\"ignored\":true");
        let (probe, _) = test_probe(&unknown_field);
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Protocol));

        let unknown = successful_response("").replace(
            "\"attach_workspace\"",
            "\"attach_workspace\",\"guest_control\"",
        );
        let (probe, _) = test_probe(&unknown);
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Unqualified));

        let duplicate = successful_response("").replace(
            "\"attach_workspace\"",
            "\"attach_workspace\",\"attach_workspace\"",
        );
        let (probe, _) = test_probe(&duplicate);
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Unqualified));
    }

    #[tokio::test]
    async fn rejects_uncorrelated_ambiguous_and_unbounded_responses() {
        let wrong_id = successful_response("").replace("probe-request-0001", "other-request");
        let (probe, _) = test_probe(&wrong_id);
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Protocol));

        let both = successful_response("").replace(
            "\"result\":",
            "\"error\":{\"code\":\"unavailable\",\"message\":\"down\",\"retryable\":true},\"result\":",
        );
        let (probe, _) = test_probe(&both);
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Protocol));

        let (probe, _) = test_probe("{}\n");
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Protocol));

        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        let transport = Arc::new(RecordingTransport {
            response: Mutex::new(oversized),
            request: Mutex::new(Vec::new()),
        });
        let probe = VmServiceIsolationProbe {
            transport,
            now: Arc::new(|| 1_784_458_800_000),
            request_id: Arc::new(|| "probe-request-0001".into()),
        };
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Protocol));
    }

    #[tokio::test]
    async fn maps_service_failures_without_returning_diagnostics() {
        let response = "{\"version\":\"1.0.0\",\"id\":\"probe-request-0001\",\"ok\":false,\"error\":{\"code\":\"unavailable\",\"message\":\"VM service is temporarily unavailable\",\"retryable\":true}}\n";
        let (probe, _) = test_probe(response);
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Unavailable));
    }

    #[tokio::test]
    #[cfg(not(windows))]
    async fn platform_transport_fails_closed_off_windows() {
        let probe = VmServiceIsolationProbe::new();
        assert_eq!(probe.probe().await, Err(IsolationProbeError::Unavailable));
    }
}
