use thiserror::Error;

use crate::{PROTOCOL_VERSION, v1};

const MAX_ARTIFACT_SOURCE_PATH_BYTES: usize = 32 * 1024;
const MAX_ARTIFACT_MEDIA_TYPE_BYTES: usize = 255;

/// Validated metadata safe for request dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMetadata {
    /// Correlation identifier returned verbatim in the response.
    pub request_id: String,
    /// Per-daemon nonce preventing stale renderer instances from reconnecting.
    pub startup_nonce: Vec<u8>,
    /// Absolute request deadline.
    pub deadline_unix_ms: u64,
    /// Optional operation key used by durable mutation handlers.
    pub idempotency_key: Option<String>,
}

/// Envelope rejection before request dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EnvelopeError {
    /// Client and daemon have incompatible protocol versions.
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u32),
    /// Correlation ID is missing or too large.
    #[error("invalid request id")]
    InvalidRequestId,
    /// Startup nonce does not identify this daemon process.
    #[error("invalid startup nonce")]
    InvalidStartupNonce,
    /// The request deadline has passed.
    #[error("request deadline exceeded")]
    DeadlineExceeded,
    /// Envelope does not contain a request.
    #[error("request payload is missing")]
    MissingRequest,
    /// Idempotency key is too large.
    #[error("invalid idempotency key")]
    InvalidIdempotencyKey,
}

/// Stable artifact-operation rejection which never contains an ephemeral path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactRequestError {
    /// Project identity is missing or not transport-safe.
    #[error("invalid artifact project id")]
    InvalidProjectId,
    /// Optional thread identity is present but invalid.
    #[error("invalid artifact thread id")]
    InvalidThreadId,
    /// Display name is not a bounded portable file name.
    #[error("invalid artifact display name")]
    InvalidDisplayName,
    /// Media type is missing, oversized, or contains controls.
    #[error("invalid artifact media type")]
    InvalidMediaType,
    /// Ephemeral source path is missing, relative, oversized, or contains controls.
    #[error("invalid artifact source path")]
    InvalidSourcePath,
    /// Artifact identity is missing or not transport-safe.
    #[error("invalid artifact id")]
    InvalidArtifactId,
    /// Content version is not a valid canonical artifact version.
    #[error("invalid artifact content version")]
    InvalidContentVersion,
    /// Expected metadata revision is not the exact current content revision.
    #[error("invalid artifact revision")]
    InvalidArtifactRevision,
}

/// Validates one ephemeral artifact import request without retaining its path.
///
/// Filesystem identity, reparse behavior, size, and content are revalidated by
/// the daemon at the moment of use. This boundary check is deliberately pure
/// and its errors never format the source path.
///
/// # Errors
///
/// Returns [`ArtifactRequestError`] for malformed or unbounded request fields.
pub fn validate_import_artifact_request(
    value: &v1::ImportArtifactRequest,
) -> Result<(), ArtifactRequestError> {
    grok_domain::ProjectId::new(value.project_id.clone())
        .map_err(|_| ArtifactRequestError::InvalidProjectId)?;
    if let Some(thread_id) = &value.thread_id {
        grok_domain::ThreadId::new(thread_id.clone())
            .map_err(|_| ArtifactRequestError::InvalidThreadId)?;
    }
    grok_domain::validate_imported_file_name(&value.display_name)
        .map_err(|_| ArtifactRequestError::InvalidDisplayName)?;
    if !bounded_single_line(&value.media_type, MAX_ARTIFACT_MEDIA_TYPE_BYTES) {
        return Err(ArtifactRequestError::InvalidMediaType);
    }
    if value.source_path.is_empty()
        || value.source_path.len() > MAX_ARTIFACT_SOURCE_PATH_BYTES
        || value.source_path.chars().any(char::is_control)
        || !std::path::Path::new(&value.source_path).is_absolute()
    {
        return Err(ArtifactRequestError::InvalidSourcePath);
    }
    Ok(())
}

/// Validates an exact-version artifact open request.
///
/// # Errors
///
/// Returns [`ArtifactRequestError`] for an invalid artifact identity or
/// content version.
pub fn validate_open_artifact_request(
    value: &v1::OpenArtifactRequest,
) -> Result<(), ArtifactRequestError> {
    grok_domain::ArtifactId::new(value.artifact_id.clone())
        .map_err(|_| ArtifactRequestError::InvalidArtifactId)?;
    if !(1..=grok_domain::MAX_ARTIFACT_CONTENT_VERSION).contains(&value.content_version) {
        return Err(ArtifactRequestError::InvalidContentVersion);
    }
    Ok(())
}

/// Validates an exact-current-version artifact removal request.
///
/// The daemon still reloads the canonical artifact and every retained version;
/// this boundary only rejects malformed or internally inconsistent optimistic
/// identity supplied by the renderer.
///
/// # Errors
///
/// Returns [`ArtifactRequestError`] for an invalid artifact identity, content
/// version, or revision/version mismatch.
pub fn validate_remove_artifact_request(
    value: &v1::RemoveArtifactRequest,
) -> Result<(), ArtifactRequestError> {
    grok_domain::ArtifactId::new(value.artifact_id.clone())
        .map_err(|_| ArtifactRequestError::InvalidArtifactId)?;
    if !(1..=grok_domain::MAX_ARTIFACT_CONTENT_VERSION).contains(&value.expected_content_version) {
        return Err(ArtifactRequestError::InvalidContentVersion);
    }
    if value.expected_revision != u64::from(value.expected_content_version) {
        return Err(ArtifactRequestError::InvalidArtifactRevision);
    }
    Ok(())
}

fn bounded_single_line(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

/// Validates protocol, freshness, nonce, and bounded metadata before dispatch.
///
/// # Errors
///
/// Returns [`EnvelopeError`] when pairing, version, deadline, shape, or metadata
/// bounds validation fails.
pub fn validate_envelope(
    envelope: &v1::Envelope,
    expected_nonce: &[u8],
    now_unix_ms: u64,
) -> Result<ValidatedMetadata, EnvelopeError> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(EnvelopeError::UnsupportedVersion(envelope.protocol_version));
    }
    if envelope.request_id.is_empty() || envelope.request_id.len() > 128 {
        return Err(EnvelopeError::InvalidRequestId);
    }
    if envelope.startup_nonce.len() != 32 || envelope.startup_nonce != expected_nonce {
        return Err(EnvelopeError::InvalidStartupNonce);
    }
    if envelope.deadline_unix_ms < now_unix_ms {
        return Err(EnvelopeError::DeadlineExceeded);
    }
    if !matches!(envelope.payload, Some(v1::envelope::Payload::Request(_))) {
        return Err(EnvelopeError::MissingRequest);
    }
    if envelope.idempotency_key.len() > 128 {
        return Err(EnvelopeError::InvalidIdempotencyKey);
    }
    Ok(ValidatedMetadata {
        request_id: envelope.request_id.clone(),
        startup_nonce: envelope.startup_nonce.clone(),
        deadline_unix_ms: envelope.deadline_unix_ms,
        idempotency_key: (!envelope.idempotency_key.is_empty())
            .then(|| envelope.idempotency_key.clone()),
    })
}

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::*;

    fn request() -> v1::Envelope {
        v1::Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            startup_nonce: vec![7; 32],
            deadline_unix_ms: 100,
            idempotency_key: String::new(),
            payload: Some(v1::envelope::Payload::Request(v1::Request {
                operation: Some(v1::request::Operation::Health(v1::HealthRequest {})),
            })),
        }
    }

    #[test]
    fn validates_nonce_version_and_deadline() {
        assert_eq!(PROTOCOL_VERSION, 28);
        assert!(validate_envelope(&request(), &[7; 32], 99).is_ok());
        for version in 0..PROTOCOL_VERSION {
            let mut previous_epoch = request();
            previous_epoch.protocol_version = version;
            assert_eq!(
                validate_envelope(&previous_epoch, &[7; 32], 99),
                Err(EnvelopeError::UnsupportedVersion(version))
            );
        }
        assert_eq!(
            validate_envelope(&request(), &[8; 32], 99),
            Err(EnvelopeError::InvalidStartupNonce)
        );
        assert_eq!(
            validate_envelope(&request(), &[7; 32], 101),
            Err(EnvelopeError::DeadlineExceeded)
        );
        let mut unsolicited_event = request();
        unsolicited_event.payload =
            Some(v1::envelope::Payload::Event(v1::Event { run_event: None }));
        assert_eq!(
            validate_envelope(&unsolicited_event, &[7; 32], 99),
            Err(EnvelopeError::MissingRequest)
        );
    }

    #[test]
    fn epoch_sixteen_health_has_a_closed_required_scheduler_lifecycle() {
        assert_eq!(v1::AutomationSchedulerHealth::Unspecified as i32, 0);
        assert_eq!(
            v1::AutomationSchedulerHealth::KernelInitializedExecutionDisabled as i32,
            1
        );
        assert_eq!(
            v1::AutomationSchedulerHealth::RecoveryPendingExecutionDisabled as i32,
            2
        );
        assert_eq!(
            v1::AutomationSchedulerHealth::DegradedExecutionDisabled as i32,
            3
        );
        assert_eq!(
            v1::AutomationSchedulerHealth::KernelInitializedExecutionEnabled as i32,
            4
        );

        let health = v1::HealthResponse {
            service_version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            instance_id: "daemon-1".into(),
            agent_runtime: None,
            automation_scheduler: v1::AutomationSchedulerHealth::KernelInitializedExecutionDisabled
                as i32,
        };
        let encoded = health.encode_to_vec();
        // HealthResponse field 5, enum value 1.
        assert!(encoded.windows(2).any(|window| window == [0x28, 0x01]));
        assert_eq!(
            v1::HealthResponse::decode(encoded.as_slice()).expect("decode health"),
            health
        );
    }

    #[test]
    fn epoch_sixteen_removed_automation_enable_fields_are_unknown() {
        let create = v1::CreateAutomationRequest {
            project_id: "project-1".into(),
            title: "Daily brief".into(),
            prompt: "Summarize the project".into(),
            schedule: "v1;daily;09:00".into(),
            timezone: "UTC".into(),
            missed_run_policy: v1::MissedRunPolicy::Skip as i32,
            overlap_policy: v1::OverlapPolicy::Skip as i32,
            schedule_active: false,
        };
        let mut legacy_create = create.encode_to_vec();
        // Removed CreateAutomationRequest.enabled field 8, varint true.
        legacy_create.extend_from_slice(&[0x40, 0x01]);
        assert_eq!(
            v1::CreateAutomationRequest::decode(legacy_create.as_slice())
                .expect("decode legacy create"),
            create
        );

        let update = v1::UpdateAutomationRequest {
            automation_id: "automation-1".into(),
            expected_revision: 0,
            title: "Daily brief".into(),
            prompt: "Summarize the project".into(),
            schedule: "v1;daily;09:00".into(),
            timezone: "UTC".into(),
            missed_run_policy: v1::MissedRunPolicy::RunOnce as i32,
            overlap_policy: v1::OverlapPolicy::QueueOne as i32,
            schedule_active: false,
        };
        let mut legacy_update = update.encode_to_vec();
        // Removed UpdateAutomationRequest.enabled field 9, varint true.
        legacy_update.extend_from_slice(&[0x48, 0x01]);
        assert_eq!(
            v1::UpdateAutomationRequest::decode(legacy_update.as_slice())
                .expect("decode legacy update"),
            update
        );
    }

    #[test]
    fn removed_unary_conversation_field_decodes_to_no_operation() {
        // Request field 38, wire type 2, followed by an empty legacy payload.
        let decoded = v1::Request::decode([0xb2, 0x02, 0x00].as_slice())
            .expect("legacy request remains wire-decodable as an unknown field");
        assert!(decoded.operation.is_none());
        assert!(decoded.encode_to_vec().is_empty());
    }

    #[test]
    fn epoch_eleven_artifact_mutation_tags_are_unknown_while_reads_keep_their_tags() {
        // Epoch 10 exposed Create/Update/DeleteArtifact on Request fields
        // 23-25. Epoch 11 reserves those tags, so even non-empty legacy
        // payloads are discarded instead of becoming dispatchable operations.
        for encoded in [
            vec![0xba, 0x01, 0x02, 0x0a, 0x00],
            vec![0xc2, 0x01, 0x02, 0x0a, 0x00],
            vec![0xca, 0x01, 0x02, 0x0a, 0x00],
        ] {
            let decoded = v1::Request::decode(encoded.as_slice())
                .expect("legacy artifact mutation remains an unknown field");
            assert!(decoded.operation.is_none());
            assert!(decoded.encode_to_vec().is_empty());
        }

        let read_operations = [
            (
                v1::request::Operation::GetArtifact(v1::GetArtifactRequest {
                    artifact_id: "artifact-1".into(),
                }),
                [0xd2, 0x01],
            ),
            (
                v1::request::Operation::ListArtifacts(v1::ListArtifactsRequest {
                    project_id: "project-1".into(),
                    cursor: String::new(),
                    limit: 20,
                }),
                [0xda, 0x01],
            ),
        ];
        for (operation, tag) in read_operations {
            let request = v1::Request {
                operation: Some(operation),
            };
            let encoded = request.encode_to_vec();
            assert!(encoded.starts_with(&tag));
            assert_eq!(
                v1::Request::decode(encoded.as_slice()).expect("decode artifact read"),
                request
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn epoch_fifteen_artifact_operations_keep_exact_tags_and_fields() {
        let import = v1::Request {
            operation: Some(v1::request::Operation::ImportArtifact(
                v1::ImportArtifactRequest {
                    project_id: "project-1".into(),
                    thread_id: Some("thread-1".into()),
                    display_name: "report.pdf".into(),
                    media_type: "application/pdf".into(),
                    source_path: absolute_test_source_path(),
                },
            )),
        };
        let encoded = import.encode_to_vec();
        // Field 55, wire type 2: (55 << 3) | 2 = 442 = [0xba, 0x03].
        assert!(encoded.starts_with(&[0xba, 0x03]));
        assert_eq!(
            v1::Request::decode(encoded.as_slice()).expect("decode import request"),
            import
        );

        let open = v1::Request {
            operation: Some(v1::request::Operation::OpenArtifact(
                v1::OpenArtifactRequest {
                    artifact_id: "artifact-1".into(),
                    content_version: 7,
                },
            )),
        };
        let encoded = open.encode_to_vec();
        // Field 56, wire type 2: (56 << 3) | 2 = 450 = [0xc2, 0x03].
        assert!(encoded.starts_with(&[0xc2, 0x03]));
        assert_eq!(
            v1::Request::decode(encoded.as_slice()).expect("decode open request"),
            open
        );

        let removal = v1::Request {
            operation: Some(v1::request::Operation::RemoveArtifact(
                v1::RemoveArtifactRequest {
                    artifact_id: "artifact-1".into(),
                    expected_revision: 7,
                    expected_content_version: 7,
                },
            )),
        };
        let encoded = removal.encode_to_vec();
        // Field 57, wire type 2: (57 << 3) | 2 = 458 = [0xca, 0x03].
        assert!(encoded.starts_with(&[0xca, 0x03]));
        assert_eq!(
            v1::Request::decode(encoded.as_slice()).expect("decode removal request"),
            removal
        );

        let response = v1::Response {
            result: Some(v1::response::Result::ArtifactOperation(
                v1::ArtifactOperationResult {
                    result: Some(v1::artifact_operation_result::Result::OpenReceipt(
                        v1::ArtifactOpenReceipt {
                            artifact_id: "artifact-1".into(),
                            content_version: 7,
                            status: v1::ArtifactOpenReceiptStatus::InterruptedNeedsReview as i32,
                            failure_code: None,
                        },
                    )),
                },
            )),
        };
        let encoded = response.encode_to_vec();
        // Field 30, wire type 2: (30 << 3) | 2 = 242 = [0xf2, 0x01].
        assert!(encoded.starts_with(&[0xf2, 0x01]));
        assert_eq!(
            v1::Response::decode(encoded.as_slice()).expect("decode artifact response"),
            response
        );

        let removed = v1::ArtifactOperationResult {
            result: Some(v1::artifact_operation_result::Result::RemovedArtifact(
                v1::Artifact {
                    id: "artifact-1".into(),
                    project_id: "project-1".into(),
                    thread_id: String::new(),
                    name: "report.pdf".into(),
                    media_type: String::new(),
                    byte_size: 0,
                    state: v1::ArtifactState::Deleted as i32,
                    revision: 8,
                    created_at_unix_ms: 1,
                    updated_at_unix_ms: 2,
                    content_version: None,
                },
            )),
        };
        let encoded = removed.encode_to_vec();
        // ArtifactOperationResult field 3 is the closed removal result.
        assert!(encoded.starts_with(&[0x1a]));
        assert_eq!(
            v1::ArtifactOperationResult::decode(encoded.as_slice())
                .expect("decode removed artifact result"),
            removed
        );

        let pending = v1::ArtifactOperationResult {
            result: Some(v1::artifact_operation_result::Result::RemovalPending(
                v1::ArtifactRemovalPendingReceipt {
                    artifact_id: "artifact-1".into(),
                    expected_revision: 7,
                    expected_content_version: 7,
                    tombstone: Some(v1::Artifact {
                        id: "artifact-1".into(),
                        project_id: "project-1".into(),
                        state: v1::ArtifactState::Deleted as i32,
                        revision: 8,
                        ..v1::Artifact::default()
                    }),
                },
            )),
        };
        let encoded = pending.encode_to_vec();
        // ArtifactOperationResult field 4 is the path-free pending receipt.
        assert!(encoded.starts_with(&[0x22]));
        assert_eq!(
            v1::ArtifactOperationResult::decode(encoded.as_slice())
                .expect("decode pending artifact removal result"),
            pending
        );

        assert_eq!(v1::ArtifactOpenFailureCode::ContentUnavailable as i32, 1);
        assert_eq!(v1::ArtifactOpenFailureCode::PlatformUnavailable as i32, 2);
        assert_eq!(v1::ArtifactOpenFailureCode::DeadlineExceeded as i32, 3);
        assert_eq!(v1::ArtifactOpenFailureCode::IntegrityFailure as i32, 4);
        assert_eq!(
            v1::ArtifactOpenFailureCode::InterruptedBeforeDispatch as i32,
            5
        );
        let failure_code_only = v1::ArtifactOpenReceipt {
            failure_code: Some(v1::ArtifactOpenFailureCode::IntegrityFailure as i32),
            ..v1::ArtifactOpenReceipt::default()
        }
        .encode_to_vec();
        // Optional enum field 4, wire type 0: (4 << 3) | 0 = 32.
        assert_eq!(failure_code_only, [0x20, 0x04]);
    }

    #[test]
    fn artifact_operation_validation_is_bounded_and_path_errors_are_redacted() {
        let valid_import = v1::ImportArtifactRequest {
            project_id: "project-1".into(),
            thread_id: None,
            display_name: "notes.txt".into(),
            media_type: "text/plain".into(),
            source_path: absolute_test_source_path(),
        };
        assert_eq!(validate_import_artifact_request(&valid_import), Ok(()));

        let mut invalid = valid_import.clone();
        invalid.source_path = "relative/secret-source".into();
        let error = validate_import_artifact_request(&invalid).expect_err("relative path rejected");
        assert_eq!(error, ArtifactRequestError::InvalidSourcePath);
        assert!(!error.to_string().contains("secret-source"));

        let mut invalid = valid_import.clone();
        invalid.display_name = "../escape".into();
        assert_eq!(
            validate_import_artifact_request(&invalid),
            Err(ArtifactRequestError::InvalidDisplayName)
        );

        let mut invalid = valid_import;
        invalid.media_type = "x".repeat(MAX_ARTIFACT_MEDIA_TYPE_BYTES + 1);
        assert_eq!(
            validate_import_artifact_request(&invalid),
            Err(ArtifactRequestError::InvalidMediaType)
        );

        assert_eq!(
            validate_open_artifact_request(&v1::OpenArtifactRequest {
                artifact_id: "artifact-1".into(),
                content_version: 1,
            }),
            Ok(())
        );
        assert_eq!(
            validate_open_artifact_request(&v1::OpenArtifactRequest {
                artifact_id: "artifact-1".into(),
                content_version: 0,
            }),
            Err(ArtifactRequestError::InvalidContentVersion)
        );
        assert_eq!(
            validate_remove_artifact_request(&v1::RemoveArtifactRequest {
                artifact_id: "artifact-1".into(),
                expected_revision: 1,
                expected_content_version: 1,
            }),
            Ok(())
        );
        assert_eq!(
            validate_remove_artifact_request(&v1::RemoveArtifactRequest {
                artifact_id: "artifact-1".into(),
                expected_revision: 2,
                expected_content_version: 1,
            }),
            Err(ArtifactRequestError::InvalidArtifactRevision)
        );
    }

    #[test]
    fn ephemeral_import_source_path_has_no_response_projection() {
        const SOURCE_CANARY: &str = "/private/import/source-canary-never-return";
        let request = v1::Request {
            operation: Some(v1::request::Operation::ImportArtifact(
                v1::ImportArtifactRequest {
                    project_id: "project-1".into(),
                    thread_id: None,
                    display_name: "notes.txt".into(),
                    media_type: "text/plain".into(),
                    source_path: SOURCE_CANARY.into(),
                },
            )),
        }
        .encode_to_vec();
        assert!(contains_bytes(&request, SOURCE_CANARY.as_bytes()));

        let responses = [
            v1::ArtifactOperationResult {
                result: Some(v1::artifact_operation_result::Result::ImportedArtifact(
                    v1::Artifact {
                        id: "artifact-1".into(),
                        project_id: "project-1".into(),
                        name: "notes.txt".into(),
                        media_type: "text/plain".into(),
                        byte_size: 12,
                        state: v1::ArtifactState::Available as i32,
                        revision: 1,
                        content_version: Some(1),
                        ..v1::Artifact::default()
                    },
                )),
            },
            v1::ArtifactOperationResult {
                result: Some(v1::artifact_operation_result::Result::OpenReceipt(
                    v1::ArtifactOpenReceipt {
                        artifact_id: "artifact-1".into(),
                        content_version: 1,
                        status: v1::ArtifactOpenReceiptStatus::Opened as i32,
                        failure_code: None,
                    },
                )),
            },
            v1::ArtifactOperationResult {
                result: Some(v1::artifact_operation_result::Result::RemovedArtifact(
                    v1::Artifact {
                        id: "artifact-1".into(),
                        project_id: "project-1".into(),
                        state: v1::ArtifactState::Deleted as i32,
                        revision: 2,
                        ..v1::Artifact::default()
                    },
                )),
            },
            v1::ArtifactOperationResult {
                result: Some(v1::artifact_operation_result::Result::RemovalPending(
                    v1::ArtifactRemovalPendingReceipt {
                        artifact_id: "artifact-1".into(),
                        expected_revision: 1,
                        expected_content_version: 1,
                        tombstone: Some(v1::Artifact {
                            id: "artifact-1".into(),
                            project_id: "project-1".into(),
                            state: v1::ArtifactState::Deleted as i32,
                            revision: 2,
                            ..v1::Artifact::default()
                        }),
                    },
                )),
            },
        ];
        for response in responses {
            let encoded = response.encode_to_vec();
            assert!(!contains_bytes(&encoded, SOURCE_CANARY.as_bytes()));
        }
    }

    #[test]
    fn artifact_projection_keeps_state_numbers_and_content_version_tag_stable() {
        assert_eq!(v1::ArtifactState::Available as i32, 1);
        assert_eq!(v1::ArtifactState::Deleted as i32, 2);
        assert_eq!(v1::ArtifactState::Unavailable as i32, 3);

        let content_version_only = v1::Artifact {
            content_version: Some(grok_domain::MAX_ARTIFACT_CONTENT_VERSION),
            ..v1::Artifact::default()
        }
        .encode_to_vec();
        // Artifact field 12, wire type 0: (12 << 3) | 0 = 96.
        assert_eq!(content_version_only.first(), Some(&0x60));

        let absent = v1::Artifact::default().encode_to_vec();
        assert!(absent.is_empty());
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn absolute_test_source_path() -> String {
        if cfg!(windows) {
            r"C:\Users\tester\report.pdf".into()
        } else {
            "/home/tester/report.pdf".into()
        }
    }

    #[test]
    fn epoch_twelve_message_mutation_tags_are_unknown_while_reads_keep_their_tags() {
        // Epoch 11 exposed Create/Update/DeleteMessage on Request fields
        // 18-20. Epoch 12 reserves those tags, so legacy payloads cannot be
        // interpreted as generic message mutations.
        for encoded in [
            vec![0x92, 0x01, 0x02, 0x0a, 0x00],
            vec![0x9a, 0x01, 0x02, 0x0a, 0x00],
            vec![0xa2, 0x01, 0x02, 0x0a, 0x00],
        ] {
            let decoded = v1::Request::decode(encoded.as_slice())
                .expect("legacy message mutation remains an unknown field");
            assert!(decoded.operation.is_none());
            assert!(decoded.encode_to_vec().is_empty());
        }

        let read_operations = [
            (
                v1::request::Operation::GetMessage(v1::GetMessageRequest {
                    message_id: "message-1".into(),
                }),
                [0xaa, 0x01],
            ),
            (
                v1::request::Operation::ListMessages(v1::ListMessagesRequest {
                    thread_id: "thread-1".into(),
                    cursor: String::new(),
                    limit: 20,
                }),
                [0xb2, 0x01],
            ),
        ];
        for (operation, tag) in read_operations {
            let request = v1::Request {
                operation: Some(operation),
            };
            let encoded = request.encode_to_vec();
            assert!(encoded.starts_with(&tag));
            assert_eq!(
                v1::Request::decode(encoded.as_slice()).expect("decode message read"),
                request
            );
        }
    }

    #[test]
    fn epoch_eight_conversation_operations_and_batch_round_trip() {
        let operations = [
            v1::request::Operation::StartConversationTurn(v1::StartConversationTurnRequest {
                thread_id: "thread-1".into(),
                content: "Hello".into(),
                model_id: Some("grok-4".into()),
                search_enabled: true,
            }),
            v1::request::Operation::CancelConversationTurn(v1::CancelConversationTurnRequest {
                turn_id: "turn-1".into(),
                expected_revision: 2,
            }),
            v1::request::Operation::PollConversationTurnEvents(
                v1::PollConversationTurnEventsRequest {
                    turn_id: "turn-1".into(),
                    after_sequence: 7,
                    limit: 32,
                    wait_timeout_ms: 20_000,
                },
            ),
            v1::request::Operation::RetryConversationTurn(v1::RetryConversationTurnRequest {
                source_turn_id: "turn-source".into(),
                expected_revision: u64::MAX,
            }),
        ];
        for operation in operations {
            let request = v1::Request {
                operation: Some(operation),
            };
            assert_eq!(
                v1::Request::decode(request.encode_to_vec().as_slice()).expect("decode request"),
                request
            );
        }

        let response = v1::Response {
            result: Some(v1::response::Result::ConversationTurnEventBatch(
                v1::ConversationTurnEventBatch {
                    events: vec![v1::ConversationTurnEvent {
                        sequence: 8,
                        turn_id: "turn-1".into(),
                        kind: v1::ConversationTurnEventKind::TextAppended as i32,
                        from_state: v1::ConversationTurnState::Unspecified as i32,
                        to_state: v1::ConversationTurnState::Unspecified as i32,
                        start_utf8_offset: 0,
                        text_appended: "durable text".into(),
                    }],
                    next_sequence: 8,
                    has_more: false,
                },
            )),
        };
        assert_eq!(
            v1::Response::decode(response.encode_to_vec().as_slice()).expect("decode response"),
            response
        );

        let turn_response = v1::Response {
            result: Some(v1::response::Result::ConversationTurn(
                v1::ConversationTurnResult {
                    turn_id: "turn-1".into(),
                    revision: u64::MAX,
                    lineage: Some(v1::ConversationTurnLineage {
                        origin: v1::ConversationTurnOrigin::Retry as i32,
                        source_turn_id: "turn-source".into(),
                        retry_depth: 64,
                    }),
                    retry_eligibility: v1::ConversationRetryEligibility::Allowed as i32,
                    ..v1::ConversationTurnResult::default()
                },
            )),
        };
        assert_eq!(
            v1::Response::decode(turn_response.encode_to_vec().as_slice())
                .expect("decode maximum revision"),
            turn_response
        );
    }

    #[test]
    fn epoch_twenty_one_start_payload_decodes_without_model_override() {
        // field 1 = "t", field 2 = "hi"; epoch 21 had no field 3.
        let decoded = v1::StartConversationTurnRequest::decode(
            [0x0a, 0x01, b't', 0x12, 0x02, b'h', b'i'].as_slice(),
        )
        .expect("decode epoch 21 start request");
        assert_eq!(decoded.thread_id, "t");
        assert_eq!(decoded.content, "hi");
        assert_eq!(decoded.model_id, None);
    }

    #[test]
    fn retry_operation_keeps_request_tag_49_and_exact_fields() {
        let request = v1::Request {
            operation: Some(v1::request::Operation::RetryConversationTurn(
                v1::RetryConversationTurnRequest {
                    source_turn_id: "turn-source".into(),
                    expected_revision: 2,
                },
            )),
        };
        let encoded = request.encode_to_vec();
        // Field 49, wire type 2: (49 << 3) | 2 = 394 = [0x8a, 0x03].
        assert!(encoded.starts_with(&[0x8a, 0x03]));
        assert_eq!(
            v1::Request::decode(encoded.as_slice()).expect("decode retry request"),
            request
        );
    }

    #[test]
    fn epoch_ten_fork_operations_keep_tags_and_renderer_authority_narrow() {
        let cases = [
            (
                v1::request::Operation::BranchConversationThread(
                    v1::BranchConversationThreadRequest {
                        source_turn_id: "turn-source".into(),
                        expected_revision: u64::MAX,
                    },
                ),
                [0x92, 0x03],
            ),
            (
                v1::request::Operation::EditAndBranchConversationTurn(
                    v1::EditAndBranchConversationTurnRequest {
                        source_turn_id: "turn-source".into(),
                        expected_revision: u64::MAX,
                        content: "Edited prompt".into(),
                    },
                ),
                [0x9a, 0x03],
            ),
            (
                v1::request::Operation::RegenerateConversationTurn(
                    v1::RegenerateConversationTurnRequest {
                        source_turn_id: "turn-source".into(),
                        expected_revision: u64::MAX,
                    },
                ),
                [0xa2, 0x03],
            ),
            (
                v1::request::Operation::GetConversationForkMetadata(
                    v1::GetConversationForkMetadataRequest {
                        thread_id: "thread-child".into(),
                    },
                ),
                [0xaa, 0x03],
            ),
            (
                v1::request::Operation::AcknowledgeConversationForkDelivery(
                    v1::AcknowledgeConversationForkDeliveryRequest {
                        child_thread_id: "thread-child".into(),
                        expected_revision: u64::MAX,
                    },
                ),
                [0xb2, 0x03],
            ),
        ];
        for (operation, tag) in cases {
            let request = v1::Request {
                operation: Some(operation),
            };
            let encoded = request.encode_to_vec();
            assert!(encoded.starts_with(&tag));
            assert_eq!(
                v1::Request::decode(encoded.as_slice()).expect("decode fork request"),
                request
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn epoch_ten_fork_results_metadata_and_delivery_round_trip() {
        let original_lineage = v1::ConversationThreadLineage {
            root_thread_id: "thread-root".into(),
            fork_depth: 0,
            origin: Some(v1::conversation_thread_lineage::Origin::Original(
                v1::ConversationOriginalThreadOrigin {},
            )),
        };
        let fork_lineage = v1::ConversationThreadLineage {
            root_thread_id: "thread-root".into(),
            fork_depth: 1,
            origin: Some(v1::conversation_thread_lineage::Origin::Fork(
                v1::ConversationForkedThreadOrigin {
                    parent_thread_id: "thread-root".into(),
                    source_turn_id: "turn-source".into(),
                    source_message_id: "message-source".into(),
                    kind: v1::ConversationForkKind::Regenerate as i32,
                },
            )),
        };
        let child_thread = v1::Thread {
            id: "thread-child".into(),
            project_id: "project-1".into(),
            lineage: Some(fork_lineage.clone()),
            ..v1::Thread::default()
        };
        let lineage_field = v1::Thread {
            lineage: Some(original_lineage.clone()),
            ..v1::Thread::default()
        }
        .encode_to_vec();
        // Thread field 8, wire type 2: (8 << 3) | 2 = 66.
        assert_eq!(lineage_field.first(), Some(&0x42));
        let result = v1::Response {
            result: Some(v1::response::Result::ConversationFork(
                v1::ConversationForkResult {
                    child_thread: Some(child_thread.clone()),
                    started_turn: None,
                    delivery: Some(v1::ConversationForkDelivery {
                        child_thread_id: "thread-child".into(),
                        state: v1::ConversationForkDeliveryState::Pending as i32,
                        revision: u64::MAX,
                    }),
                },
            )),
        };
        let result_bytes = result.encode_to_vec();
        // Response field 27, wire type 2: (27 << 3) | 2 = 218 = [0xda, 0x01].
        assert!(result_bytes.starts_with(&[0xda, 0x01]));
        assert_eq!(
            v1::Response::decode(result_bytes.as_slice()).expect("decode fork result"),
            result
        );

        let metadata = v1::Response {
            result: Some(v1::response::Result::ConversationForkMetadata(
                v1::ConversationForkMetadata {
                    lineage: Some(fork_lineage),
                    inherited_assistant_outcomes: vec![v1::ConversationInheritedAssistantOutcome {
                        child_assistant_message_id: "message-child-assistant".into(),
                        source_turn_id: "turn-source".into(),
                        model_id: "grok-4.3".into(),
                        citations: vec![v1::ConversationCitation {
                            title: "Source".into(),
                            url: "https://example.com/source".into(),
                        }],
                        usage: Some(v1::ConversationUsage {
                            input_tokens: u64::MAX,
                            output_tokens: u64::MAX,
                            cost_in_usd_ticks: u64::MAX,
                        }),
                        zero_data_retention: Some(true),
                    }],
                    family_threads: vec![
                        v1::Thread {
                            id: "thread-root".into(),
                            project_id: "project-1".into(),
                            lineage: Some(original_lineage),
                            ..v1::Thread::default()
                        },
                        child_thread,
                    ],
                },
            )),
        };
        let metadata_bytes = metadata.encode_to_vec();
        // Response field 28, wire type 2: (28 << 3) | 2 = 226 = [0xe2, 0x01].
        assert!(metadata_bytes.starts_with(&[0xe2, 0x01]));
        assert_eq!(
            v1::Response::decode(metadata_bytes.as_slice()).expect("decode fork metadata"),
            metadata
        );

        let delivery = v1::Response {
            result: Some(v1::response::Result::ConversationForkDelivery(
                v1::ConversationForkDelivery {
                    child_thread_id: "thread-child".into(),
                    state: v1::ConversationForkDeliveryState::Acknowledged as i32,
                    revision: 1,
                },
            )),
        };
        let delivery_bytes = delivery.encode_to_vec();
        // Response field 29, wire type 2: (29 << 3) | 2 = 234 = [0xea, 0x01].
        assert!(delivery_bytes.starts_with(&[0xea, 0x01]));
        assert_eq!(
            v1::Response::decode(delivery_bytes.as_slice()).expect("decode fork delivery"),
            delivery
        );
    }

    #[test]
    fn epoch_nine_message_derivation_keeps_field_ten_and_closed_lineage() {
        let message = v1::Message {
            id: "message-child".into(),
            thread_id: "thread-child".into(),
            derivation: Some(v1::ConversationMessageDerivation {
                origin: Some(v1::conversation_message_derivation::Origin::Fork(
                    v1::ConversationForkedMessageDerivation {
                        source_message_id: "message-source".into(),
                        source_turn_id: "turn-source".into(),
                        context_position: Some(u32::MAX),
                        kind: v1::ConversationMessageDerivationKind::EditedUser as i32,
                    },
                )),
            }),
            ..v1::Message::default()
        };
        let derivation_field = v1::Message {
            derivation: message.derivation.clone(),
            ..v1::Message::default()
        }
        .encode_to_vec();
        // Message field 10, wire type 2: (10 << 3) | 2 = 82.
        assert_eq!(derivation_field.first(), Some(&0x52));
        assert_eq!(
            v1::Message::decode(message.encode_to_vec().as_slice())
                .expect("decode message derivation"),
            message
        );
    }

    #[test]
    fn epoch_twenty_three_usage_summary_request_and_result_round_trip() {
        let usage_request = v1::GetUsageSummaryRequest {
            scope_kind: "project".into(),
            scope_id: "project-1".into(),
            window: "last_30_days".into(),
        };
        let decoded_request =
            v1::GetUsageSummaryRequest::decode(usage_request.encode_to_vec().as_slice())
                .expect("decode usage summary request");
        assert_eq!(decoded_request, usage_request);

        let mut envelope = request();
        envelope.payload = Some(v1::envelope::Payload::Request(v1::Request {
            operation: Some(v1::request::Operation::GetUsageSummary(
                usage_request.clone(),
            )),
        }));
        let round_tripped = v1::Envelope::decode(envelope.encode_to_vec().as_slice())
            .expect("decode envelope with usage summary request");
        let Some(v1::envelope::Payload::Request(inner)) = round_tripped.payload else {
            panic!("request payload");
        };
        assert!(matches!(
            inner.operation,
            Some(v1::request::Operation::GetUsageSummary(value))
                if value == usage_request
        ));

        let summary = v1::UsageSummary {
            input_tokens: 12,
            output_tokens: 4,
            cost_in_usd_ticks: 9,
            turn_count: 1,
            scope_kind: "workspace".into(),
            scope_id: String::new(),
            window: "last_7_days".into(),
            as_of_unix_ms: 1_234,
        };
        let response = v1::Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            startup_nonce: vec![7; 32],
            deadline_unix_ms: 100,
            idempotency_key: String::new(),
            payload: Some(v1::envelope::Payload::Response(v1::Response {
                result: Some(v1::response::Result::UsageSummary(summary.clone())),
            })),
        };
        let decoded_response = v1::Envelope::decode(response.encode_to_vec().as_slice())
            .expect("decode usage summary response");
        let Some(v1::envelope::Payload::Response(inner)) = decoded_response.payload else {
            panic!("response payload");
        };
        assert!(matches!(
            inner.result,
            Some(v1::response::Result::UsageSummary(value)) if value == summary
        ));
    }

    #[test]
    fn epoch_twenty_five_host_enrollment_and_backend_projection_round_trip() {
        let enrollment = v1::EnrollHostExecutionRequest {
            expected_revision: 4,
            acknowledgment_version: 1,
            typed_acknowledgment: "I UNDERSTAND HOST TOOLS CAN CONTROL THIS COMPUTER".into(),
            filesystem_read: true,
            filesystem_write: true,
            process_execute: true,
            path_roots: vec!["/workspace".into()],
            broad_scope_acknowledged: false,
        };
        let mut envelope = request();
        envelope.payload = Some(v1::envelope::Payload::Request(v1::Request {
            operation: Some(v1::request::Operation::EnrollHostExecution(
                enrollment.clone(),
            )),
        }));
        let decoded = v1::Envelope::decode(envelope.encode_to_vec().as_slice())
            .expect("decode Host Tools enrollment");
        let Some(v1::envelope::Payload::Request(request)) = decoded.payload else {
            panic!("request payload");
        };
        assert!(matches!(
            request.operation,
            Some(v1::request::Operation::EnrollHostExecution(value)) if value == enrollment
        ));

        let run = v1::Run {
            kind: v1::RunKind::Work as i32,
            work_backend: v1::WorkExecutionBackend::HostDirect as i32,
            ..v1::Run::default()
        };
        assert_eq!(
            v1::Run::decode(run.encode_to_vec().as_slice()).expect("decode bound Work run"),
            run
        );
    }

    #[test]
    fn epoch_twenty_eight_host_work_requests_and_snapshots_round_trip() {
        let start = v1::StartHostWorkRequest {
            project_id: "project-1".into(),
            thread_id: "thread-1".into(),
            prompt: "Inspect the workspace".into(),
        };
        let mut envelope = request();
        envelope.payload = Some(v1::envelope::Payload::Request(v1::Request {
            operation: Some(v1::request::Operation::StartHostWork(start.clone())),
        }));
        let decoded = v1::Envelope::decode(envelope.encode_to_vec().as_slice())
            .expect("decode Host Work request");
        let Some(v1::envelope::Payload::Request(request)) = decoded.payload else {
            panic!("request payload");
        };
        assert!(matches!(
            request.operation,
            Some(v1::request::Operation::StartHostWork(value)) if value == start
        ));

        let cancel = v1::Request {
            operation: Some(v1::request::Operation::CancelHostWork(
                v1::CancelHostWorkRequest {
                    run_id: "run-1".into(),
                },
            )),
        };
        let decoded = v1::Request::decode(cancel.encode_to_vec().as_slice())
            .expect("decode Host Work cancellation");
        assert!(matches!(
            decoded.operation,
            Some(v1::request::Operation::CancelHostWork(value)) if value.run_id == "run-1"
        ));

        let result = v1::HostWorkResult {
            run: Some(v1::Run {
                kind: v1::RunKind::Work as i32,
                work_backend: v1::WorkExecutionBackend::HostDirect as i32,
                ..v1::Run::default()
            }),
            assistant_text: "Workspace inspected".into(),
        };
        let response = v1::Response {
            result: Some(v1::response::Result::HostWorkResult(result.clone())),
        };
        let decoded = v1::Response::decode(response.encode_to_vec().as_slice())
            .expect("decode Host Work result");
        assert!(matches!(
            decoded.result,
            Some(v1::response::Result::HostWorkResult(value)) if value == result
        ));

        let list = v1::Request {
            operation: Some(v1::request::Operation::ListHostWorkRuns(
                v1::ListHostWorkRunsRequest {
                    limit: 25,
                    thread_id: "thread-work".into(),
                },
            )),
        };
        let decoded = v1::Request::decode(list.encode_to_vec().as_slice())
            .expect("decode bounded Host Work list request");
        assert!(matches!(
            decoded.operation,
            Some(v1::request::Operation::ListHostWorkRuns(value))
                if value.limit == 25 && value.thread_id == "thread-work"
        ));

        let snapshots = v1::HostWorkList {
            items: vec![v1::HostWorkSnapshot {
                run: result.run,
                pending_approval: Some(v1::Approval {
                    id: "approval-1".into(),
                    run_id: "run-1".into(),
                    ..v1::Approval::default()
                }),
            }],
        };
        assert_eq!(
            v1::HostWorkList::decode(snapshots.encode_to_vec().as_slice())
                .expect("decode durable Host Work snapshots"),
            snapshots
        );
    }
}
