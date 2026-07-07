//! Generated local IPC contract and explicit domain conversions.

mod convert;
mod validation;

/// Current compatibility epoch accepted by the daemon.
///
/// Epoch sixteen exposes only the daemon scheduler kernel lifecycle while
/// scheduled execution remains disabled, and removes renderer authority to
/// request an enabled automation definition. Epoch fifteen added daemon-owned
/// artifact removal with durable private-namespace retention recovery. It does
/// not resurrect the permanently reserved generic
/// artifact delete producer. Epoch fourteen added daemon-owned artifact import
/// and exact-version open receipts without exposing a source path, storage
/// path, digest, or object locator. Epoch thirteen removed daemon-private artifact storage paths from
/// renderer wire projections. Epoch twelve removed renderer-accessible generic message
/// mutations, while retaining daemon-owned message reads and inward-facing
/// production APIs.
/// Epoch eleven removed renderer-accessible artifact metadata mutations, while
/// retaining daemon-owned artifact reads and inward-facing production APIs.
/// Epoch ten added acknowledged fork-result delivery so an ambiguous renderer
/// response cannot create another billable child after a process restart. The
/// Protobuf package remains the canonical v1 schema family, while the envelope
/// version prevents either side from silently accepting a different operation
/// set.
pub const PROTOCOL_VERSION: u32 = 16;

/// Generated messages for the canonical daemon schema family.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/grok.desktop.daemon.v1.rs"));
}

pub use convert::{
    ConversationRetryEligibility, ProtocolConversionError, account_state_to_wire,
    approval_decision_from_wire, approval_to_wire, artifact_open_receipt_to_wire,
    artifact_removal_pending_to_wire, artifact_to_wire, automation_history_to_wire,
    automation_to_wire, capability_facts_from_wire, capability_to_wire, chat_model_catalog_to_wire,
    chat_model_preference_to_wire, conversation_fork_delivery_to_wire,
    conversation_fork_metadata_to_wire, conversation_fork_to_wire,
    conversation_turn_event_page_to_wire, conversation_turn_event_to_wire,
    conversation_turn_to_wire, conversation_turn_to_wire_with_retry_eligibility,
    desktop_preferences_to_wire, event_to_wire, import_artifact_from_wire,
    imported_artifact_to_wire, message_to_wire, missed_run_policy_from_wire,
    open_artifact_from_wire, overlap_policy_from_wire, project_to_wire, remove_artifact_from_wire,
    removed_artifact_to_wire, run_to_wire, thread_to_wire, workspace_search_hit_to_wire,
};
pub use validation::{
    ArtifactRequestError, EnvelopeError, ValidatedMetadata, validate_envelope,
    validate_import_artifact_request, validate_open_artifact_request,
    validate_remove_artifact_request,
};
