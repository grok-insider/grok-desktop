use grok_application::{
    AccountState, ArtifactOpenFailureCode, ArtifactOpenReceipt, ArtifactOpenReceiptStatus,
    CapabilityFacts, ChatModelCatalog, ChatModelCatalogEntry, ConversationForkDelivery,
    ConversationForkDeliveryState, ConversationForkMetadata, ConversationForkSnapshot,
    ConversationInheritedAssistantOutcome, ConversationTurnEventPage, ConversationTurnSnapshot,
    ImportArtifact, OpenArtifact, RemoveArtifact, SelectedSourcePath, UsageScope, UsageSummary,
    UsageWindow, WorkspaceSearchHit, WorkspaceSearchKind,
};
use grok_domain::{
    Approval, ApprovalDecision, ApprovalRisk, ApprovalScope, ApprovalStatus, Artifact,
    ArtifactState, AuthMethod, Automation, AutomationHistoryEntry, AutomationHistoryStatus,
    AutomationState, Capability, CapabilityAvailability, CapabilityStatus, CapabilitySurface,
    ChatModelPreference, ConversationCitation, ConversationFailureKind, ConversationForkKind,
    ConversationMessageDerivation, ConversationMessageDerivationKind, ConversationThreadLineage,
    ConversationThreadOrigin, ConversationTurnEvent, ConversationTurnEventKind,
    ConversationTurnLineage, ConversationTurnOrigin, ConversationTurnState, ConversationUsage,
    DesktopPreferences, Message, MessageRole, MessageState, MissedRunPolicy, OverlapPolicy,
    Project, ProjectState, RequestedAction, Run, RunEvent, RunEventKind, RunState, Thread,
    ThreadId, ThreadState,
};
use thiserror::Error;

use crate::{
    ArtifactRequestError, v1, validate_import_artifact_request, validate_open_artifact_request,
    validate_remove_artifact_request,
};

/// A wire enum or structured value could not be converted safely.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid {field}: {value}")]
pub struct ProtocolConversionError {
    /// Invalid field name.
    pub field: &'static str,
    /// Invalid numeric or textual value.
    pub value: String,
}

/// Daemon-derived structural reason a canonical turn can or cannot be retried.
///
/// This projection is advisory to renderers. The daemon revalidates the exact
/// source turn and current credential generation when handling a retry command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationRetryEligibility {
    /// The caller has not supplied an eligibility projection yet.
    Unspecified,
    /// The newest turn is a cancelled or retryable known failure and its local
    /// credential generation remains available.
    Allowed,
    /// A later turn exists in the same conversation.
    NotNewest,
    /// The source turn has not reached a terminal state.
    SourceInProgress,
    /// Completed output requires distinct regenerate semantics.
    SourceCompleted,
    /// Provider outcome uncertainty requires review and cannot be retried.
    SourceInterruptedNeedsReview,
    /// The provider returned a known failure which it did not classify retryable.
    FailureNotRetryable,
    /// The opaque local credential generation used by the source is unavailable.
    SourceAccountUnavailable,
    /// The bounded linear retry chain has reached its maximum depth.
    DepthExhausted,
    /// The owning conversation thread or project is archived.
    SourceReadOnly,
}

/// Converts an application capability snapshot from the wire.
#[must_use]
pub fn capability_facts_from_wire(value: Option<v1::CapabilityFacts>) -> CapabilityFacts {
    let value = value.unwrap_or_default();
    CapabilityFacts {
        subscription_authenticated: value.subscription_authenticated,
        xai_api_key_configured: value.xai_api_key_configured,
        // SuperGrok connection state is daemon-vault-owned and never caller supplied.
        supergrok_api_connected: false,
        xai_capabilities_resolved: false,
        online: value.online,
        isolation_broker_qualified: false,
        strong_isolation_ready: value.strong_isolation_ready,
        managed_browser_ready: value.managed_browser_ready,
        computer_use_ready: value.computer_use_ready,
        // The deprecated caller-facts message can never self-qualify a local
        // filesystem/platform adapter. Only daemon composition sets this.
        artifact_content_ready: false,
        // Scheduler readiness is daemon-owned lifecycle state, never client facts.
        automation_scheduler_ready: false,
    }
}

/// Converts a capability resolution into its version-one wire form.
#[must_use]
pub fn capability_to_wire(value: CapabilityStatus) -> v1::CapabilityStatus {
    v1::CapabilityStatus {
        capability: capability(value.capability) as i32,
        surface: surface(value.surface) as i32,
        authentication: authentication(value.authentication) as i32,
        availability: availability(value.availability) as i32,
        reason_code: value.reason_code,
        reason: value.reason,
    }
}

/// Converts non-secret daemon account state into its wire form.
#[must_use]
pub const fn account_state_to_wire(value: AccountState) -> v1::AccountState {
    v1::AccountState {
        xai_api_key_configured: value.xai_api_key_configured,
        xai_capabilities_resolved: value.xai_capabilities_resolved,
        grok_build_authenticated: value.grok_build_authenticated,
    }
}

/// Converts daemon-owned desktop behavior to its wire form.
#[must_use]
pub const fn desktop_preferences_to_wire(value: &DesktopPreferences) -> v1::DesktopPreferences {
    v1::DesktopPreferences {
        keep_running_in_notification_area: value.keep_running_in_notification_area,
        revision: value.revision,
        updated_at_unix_ms: value.updated_at,
    }
}

/// Converts a durable Chat model selection to its wire form.
#[must_use]
pub fn chat_model_preference_to_wire(value: ChatModelPreference) -> v1::ChatModelPreference {
    v1::ChatModelPreference {
        selected_model_id: value.selected_model_id,
        revision: value.revision,
        updated_at_unix_ms: value.updated_at,
    }
}

/// Converts a live bounded official xAI model catalog to its wire form.
#[must_use]
pub fn chat_model_catalog_to_wire(value: ChatModelCatalog) -> v1::ChatModelCatalog {
    v1::ChatModelCatalog {
        models: value
            .models
            .into_iter()
            .map(chat_model_descriptor_to_wire)
            .collect(),
        preference: Some(chat_model_preference_to_wire(value.preference)),
        default_model_id: value.default_model_id,
        selected_model_ready: value.selected_model_ready,
        default_model_ready: value.default_model_ready,
    }
}

fn chat_model_descriptor_to_wire(value: ChatModelCatalogEntry) -> v1::ChatModelDescriptor {
    v1::ChatModelDescriptor {
        id: value.id,
        aliases: value.aliases,
        input_modalities: value.input_modalities,
        output_modalities: value.output_modalities,
        text_conversation_ready: value.text_conversation_ready,
    }
}

/// Converts a local completed-turn usage aggregate to its wire form.
#[must_use]
pub fn usage_summary_to_wire(value: UsageSummary) -> v1::UsageSummary {
    let (scope_kind, scope_id) = match value.scope {
        UsageScope::Workspace => ("workspace", String::new()),
        UsageScope::Project(project_id) => ("project", project_id.as_str().to_owned()),
        UsageScope::Thread(thread_id) => ("thread", thread_id.as_str().to_owned()),
    };
    let window = match value.window {
        UsageWindow::Last7Days => "last_7_days",
        UsageWindow::Last30Days => "last_30_days",
        UsageWindow::AllTime => "all_time",
    };
    v1::UsageSummary {
        input_tokens: value.input_tokens,
        output_tokens: value.output_tokens,
        cost_in_usd_ticks: value.cost_in_usd_ticks,
        turn_count: value.turn_count,
        scope_kind: scope_kind.into(),
        scope_id,
        window: window.into(),
        as_of_unix_ms: value.as_of,
    }
}

/// Converts the canonical result of one explicit child-thread fork.
///
/// Copied child messages are loaded through the canonical conversation read and
/// fork-metadata operations. They are deliberately not duplicated in this
/// mutation result.
#[must_use]
pub fn conversation_fork_to_wire(value: ConversationForkSnapshot) -> v1::ConversationForkResult {
    let ConversationForkSnapshot {
        child_thread,
        messages: _,
        started_turn,
        delivery,
    } = value;
    v1::ConversationForkResult {
        child_thread: Some(thread_to_wire(child_thread)),
        started_turn: started_turn.map(conversation_turn_to_wire),
        delivery: Some(conversation_fork_delivery_to_wire(&delivery)),
    }
}

/// Converts one bounded daemon-owned fork delivery projection.
#[must_use]
pub fn conversation_fork_delivery_to_wire(
    value: &ConversationForkDelivery,
) -> v1::ConversationForkDelivery {
    v1::ConversationForkDelivery {
        child_thread_id: value.child_thread_id.to_string(),
        state: match value.state {
            ConversationForkDeliveryState::Pending => {
                v1::ConversationForkDeliveryState::Pending as i32
            }
            ConversationForkDeliveryState::Acknowledged => {
                v1::ConversationForkDeliveryState::Acknowledged as i32
            }
        },
        revision: value.revision,
    }
}

/// Converts bounded immutable fork ancestry and inherited assistant outcomes.
#[must_use]
pub fn conversation_fork_metadata_to_wire(
    value: ConversationForkMetadata,
) -> v1::ConversationForkMetadata {
    v1::ConversationForkMetadata {
        lineage: Some(conversation_thread_lineage_to_wire(value.lineage)),
        inherited_assistant_outcomes: value
            .inherited_assistant_outcomes
            .into_iter()
            .map(conversation_inherited_assistant_outcome_to_wire)
            .collect(),
        family_threads: value
            .family_threads
            .into_iter()
            .map(thread_to_wire)
            .collect(),
    }
}

fn conversation_inherited_assistant_outcome_to_wire(
    value: ConversationInheritedAssistantOutcome,
) -> v1::ConversationInheritedAssistantOutcome {
    v1::ConversationInheritedAssistantOutcome {
        child_assistant_message_id: value.child_assistant_message_id.into_inner(),
        source_turn_id: value.source_turn_id.into_inner(),
        model_id: value.model_id,
        citations: value
            .citations
            .into_iter()
            .map(conversation_citation_to_wire)
            .collect(),
        usage: Some(conversation_usage_to_wire(value.usage)),
        zero_data_retention: value.zero_data_retention,
    }
}

/// Converts a durable direct-provider turn and its linked outcome to wire form.
#[must_use]
pub fn conversation_turn_to_wire(value: ConversationTurnSnapshot) -> v1::ConversationTurnResult {
    conversation_turn_to_wire_with_retry_eligibility(
        value,
        ConversationRetryEligibility::Unspecified,
    )
}

/// Converts a durable direct-provider turn with daemon-projected retry eligibility.
#[must_use]
pub fn conversation_turn_to_wire_with_retry_eligibility(
    value: ConversationTurnSnapshot,
    retry_eligibility: ConversationRetryEligibility,
) -> v1::ConversationTurnResult {
    let lineage = conversation_turn_lineage_to_wire(value.lineage);
    let turn = value.turn;
    v1::ConversationTurnResult {
        turn_id: turn.id.into_inner(),
        state: conversation_turn_state_to_wire(turn.state) as i32,
        model_id: turn.model_id,
        user_message: Some(message_to_wire(value.user_message)),
        assistant_message: value.assistant_message.map(message_to_wire),
        run: Some(run_to_wire(value.run)),
        failure: turn.failure.map(|failure| v1::ConversationFailure {
            kind: match failure.kind {
                ConversationFailureKind::Authentication => {
                    v1::ConversationFailureKind::Authentication
                }
                ConversationFailureKind::Forbidden => v1::ConversationFailureKind::Forbidden,
                ConversationFailureKind::InvalidRequest => {
                    v1::ConversationFailureKind::InvalidRequest
                }
                ConversationFailureKind::RateLimited => v1::ConversationFailureKind::RateLimited,
                ConversationFailureKind::Unavailable => v1::ConversationFailureKind::Unavailable,
                ConversationFailureKind::Protocol => v1::ConversationFailureKind::Protocol,
            } as i32,
            message: failure.message,
            retryable: failure.retryable,
        }),
        citations: turn
            .citations
            .into_iter()
            .map(conversation_citation_to_wire)
            .collect(),
        usage: Some(conversation_usage_to_wire(turn.usage)),
        zero_data_retention: turn.zero_data_retention,
        revision: turn.revision,
        lineage: Some(lineage),
        retry_eligibility: conversation_retry_eligibility_to_wire(retry_eligibility) as i32,
    }
}

fn conversation_turn_lineage_to_wire(
    value: ConversationTurnLineage,
) -> v1::ConversationTurnLineage {
    let (origin, source_turn_id) = match value.origin {
        ConversationTurnOrigin::Original => (v1::ConversationTurnOrigin::Original, String::new()),
        ConversationTurnOrigin::Retry { source_turn_id } => (
            v1::ConversationTurnOrigin::Retry,
            source_turn_id.into_inner(),
        ),
        ConversationTurnOrigin::EditAndBranch { source_turn_id } => (
            v1::ConversationTurnOrigin::EditAndBranch,
            source_turn_id.into_inner(),
        ),
        ConversationTurnOrigin::Regenerate { source_turn_id } => (
            v1::ConversationTurnOrigin::Regenerate,
            source_turn_id.into_inner(),
        ),
    };
    v1::ConversationTurnLineage {
        origin: origin as i32,
        source_turn_id,
        retry_depth: u32::from(value.retry_depth),
    }
}

fn conversation_citation_to_wire(value: ConversationCitation) -> v1::ConversationCitation {
    v1::ConversationCitation {
        title: value.title.unwrap_or_default(),
        url: value.url,
    }
}

const fn conversation_usage_to_wire(value: ConversationUsage) -> v1::ConversationUsage {
    v1::ConversationUsage {
        input_tokens: value.input_tokens,
        output_tokens: value.output_tokens,
        cost_in_usd_ticks: value.cost_in_usd_ticks,
    }
}

const fn conversation_retry_eligibility_to_wire(
    value: ConversationRetryEligibility,
) -> v1::ConversationRetryEligibility {
    match value {
        ConversationRetryEligibility::Unspecified => v1::ConversationRetryEligibility::Unspecified,
        ConversationRetryEligibility::Allowed => v1::ConversationRetryEligibility::Allowed,
        ConversationRetryEligibility::NotNewest => v1::ConversationRetryEligibility::NotNewest,
        ConversationRetryEligibility::SourceInProgress => {
            v1::ConversationRetryEligibility::SourceInProgress
        }
        ConversationRetryEligibility::SourceCompleted => {
            v1::ConversationRetryEligibility::SourceCompleted
        }
        ConversationRetryEligibility::SourceInterruptedNeedsReview => {
            v1::ConversationRetryEligibility::SourceInterruptedNeedsReview
        }
        ConversationRetryEligibility::FailureNotRetryable => {
            v1::ConversationRetryEligibility::FailureNotRetryable
        }
        ConversationRetryEligibility::SourceAccountUnavailable => {
            v1::ConversationRetryEligibility::SourceAccountUnavailable
        }
        ConversationRetryEligibility::DepthExhausted => {
            v1::ConversationRetryEligibility::DepthExhausted
        }
        ConversationRetryEligibility::SourceReadOnly => {
            v1::ConversationRetryEligibility::SourceReadOnly
        }
    }
}

/// Converts one validated durable conversation event into its wire form.
#[must_use]
pub fn conversation_turn_event_to_wire(value: ConversationTurnEvent) -> v1::ConversationTurnEvent {
    let (kind, from_state, to_state, start_utf8_offset, text_appended) = match value.kind {
        ConversationTurnEventKind::Created => (
            v1::ConversationTurnEventKind::Created,
            v1::ConversationTurnState::Unspecified,
            v1::ConversationTurnState::Unspecified,
            0,
            String::new(),
        ),
        ConversationTurnEventKind::StateChanged { from, to } => (
            v1::ConversationTurnEventKind::StateChanged,
            conversation_turn_state_to_wire(from),
            conversation_turn_state_to_wire(to),
            0,
            String::new(),
        ),
        ConversationTurnEventKind::TextAppended {
            start_utf8_offset,
            text,
        } => (
            v1::ConversationTurnEventKind::TextAppended,
            v1::ConversationTurnState::Unspecified,
            v1::ConversationTurnState::Unspecified,
            start_utf8_offset,
            text,
        ),
    };
    v1::ConversationTurnEvent {
        sequence: value.sequence,
        turn_id: value.turn_id.into_inner(),
        kind: kind as i32,
        from_state: from_state as i32,
        to_state: to_state as i32,
        start_utf8_offset,
        text_appended,
    }
}

/// Converts one bounded reconnect page and preserves an empty page's cursor.
#[must_use]
pub fn conversation_turn_event_page_to_wire(
    value: ConversationTurnEventPage,
    after_sequence: u64,
) -> v1::ConversationTurnEventBatch {
    let next_sequence = value
        .events
        .last()
        .map_or(after_sequence, |event| event.sequence);
    v1::ConversationTurnEventBatch {
        events: value
            .events
            .into_iter()
            .map(conversation_turn_event_to_wire)
            .collect(),
        next_sequence,
        has_more: value.has_more,
    }
}

/// Converts a run into its version-one wire form.
#[must_use]
pub fn run_to_wire(value: Run) -> v1::Run {
    v1::Run {
        id: value.id.into_inner(),
        project_id: value.project_id.into_inner(),
        thread_id: value.thread_id.into_inner(),
        state: run_state_to_wire(value.state) as i32,
        revision: value.revision,
        created_at_unix_ms: value.created_at,
        updated_at_unix_ms: value.updated_at,
    }
}

/// Converts an audit event into its version-one wire form.
#[must_use]
pub fn event_to_wire(value: RunEvent) -> v1::RunEvent {
    let (kind, from_state, to_state, related_id) = match value.kind {
        RunEventKind::Created => (
            v1::RunEventKind::Created,
            v1::RunState::Unspecified,
            v1::RunState::Unspecified,
            String::new(),
        ),
        RunEventKind::StateChanged { from, to } => (
            v1::RunEventKind::StateChanged,
            run_state_to_wire(from),
            run_state_to_wire(to),
            String::new(),
        ),
        RunEventKind::ApprovalRequested { approval_id } => (
            v1::RunEventKind::ApprovalRequested,
            v1::RunState::Unspecified,
            v1::RunState::Unspecified,
            approval_id.into_inner(),
        ),
        RunEventKind::EffectPrepared { effect_id } => (
            v1::RunEventKind::EffectPrepared,
            v1::RunState::Unspecified,
            v1::RunState::Unspecified,
            effect_id.into_inner(),
        ),
        RunEventKind::EffectNeedsReview { effect_id } => (
            v1::RunEventKind::EffectNeedsReview,
            v1::RunState::Unspecified,
            v1::RunState::Unspecified,
            effect_id.into_inner(),
        ),
    };
    v1::RunEvent {
        sequence: value.sequence,
        run_id: value.run_id.into_inner(),
        occurred_at_unix_ms: value.occurred_at,
        kind: kind as i32,
        from_state: from_state as i32,
        to_state: to_state as i32,
        related_id,
    }
}

/// Converts an approval into its version-one wire form.
#[must_use]
pub fn approval_to_wire(value: Approval) -> v1::Approval {
    let (scope, resource_id) = match value.scope {
        ApprovalScope::Once => (v1::ApprovalScope::Once, String::new()),
        ApprovalScope::Run => (v1::ApprovalScope::Run, String::new()),
        ApprovalScope::Resource(id) => (v1::ApprovalScope::Resource, id),
    };
    v1::Approval {
        id: value.id.into_inner(),
        run_id: value.run_id.into_inner(),
        action: Some(requested_action_to_wire(value.request)),
        scope: scope as i32,
        resource_id,
        status: approval_status_to_wire(value.status) as i32,
        revision: value.revision,
        created_at_unix_ms: value.created_at,
        expires_at_unix_ms: value.expires_at,
        decided_at_unix_ms: value.decided_at,
    }
}

/// Converts a canonical project to the version-one wire form.
#[must_use]
pub fn project_to_wire(value: Project) -> v1::Project {
    v1::Project {
        id: value.id.into_inner(),
        name: value.name,
        description: value.description,
        state: match value.state {
            ProjectState::Active => v1::ProjectState::Active,
            ProjectState::Archived => v1::ProjectState::Archived,
        } as i32,
        revision: value.revision,
        created_at_unix_ms: value.created_at,
        updated_at_unix_ms: value.updated_at,
    }
}

/// Converts a canonical thread to the version-one wire form.
#[must_use]
pub fn thread_to_wire(value: Thread) -> v1::Thread {
    v1::Thread {
        id: value.id.into_inner(),
        project_id: value.project_id.into_inner(),
        title: value.title,
        state: match value.state {
            ThreadState::Open => v1::ThreadState::Open,
            ThreadState::Archived => v1::ThreadState::Archived,
        } as i32,
        revision: value.revision,
        created_at_unix_ms: value.created_at,
        updated_at_unix_ms: value.updated_at,
        lineage: Some(conversation_thread_lineage_to_wire(value.lineage)),
    }
}

fn conversation_thread_lineage_to_wire(
    value: ConversationThreadLineage,
) -> v1::ConversationThreadLineage {
    let origin = match value.origin {
        ConversationThreadOrigin::Original => v1::conversation_thread_lineage::Origin::Original(
            v1::ConversationOriginalThreadOrigin {},
        ),
        ConversationThreadOrigin::Fork {
            parent_thread_id,
            source_turn_id,
            source_message_id,
            kind,
        } => v1::conversation_thread_lineage::Origin::Fork(v1::ConversationForkedThreadOrigin {
            parent_thread_id: parent_thread_id.into_inner(),
            source_turn_id: source_turn_id.into_inner(),
            source_message_id: source_message_id.into_inner(),
            kind: conversation_fork_kind_to_wire(kind) as i32,
        }),
    };
    v1::ConversationThreadLineage {
        root_thread_id: value.root_thread_id.into_inner(),
        fork_depth: u32::from(value.fork_depth),
        origin: Some(origin),
    }
}

const fn conversation_fork_kind_to_wire(value: ConversationForkKind) -> v1::ConversationForkKind {
    match value {
        ConversationForkKind::Branch => v1::ConversationForkKind::Branch,
        ConversationForkKind::EditAndBranch => v1::ConversationForkKind::EditAndBranch,
        ConversationForkKind::Regenerate => v1::ConversationForkKind::Regenerate,
    }
}

/// Converts a canonical message to the version-one wire form.
#[must_use]
pub fn message_to_wire(value: Message) -> v1::Message {
    v1::Message {
        id: value.id.into_inner(),
        thread_id: value.thread_id.into_inner(),
        sequence: value.sequence,
        role: message_role_to_wire(value.role) as i32,
        content: value.content,
        state: match value.state {
            MessageState::Active => v1::MessageState::Active,
            MessageState::Deleted => v1::MessageState::Deleted,
        } as i32,
        revision: value.revision,
        created_at_unix_ms: value.created_at,
        updated_at_unix_ms: value.updated_at,
        derivation: Some(conversation_message_derivation_to_wire(value.derivation)),
    }
}

fn conversation_message_derivation_to_wire(
    value: ConversationMessageDerivation,
) -> v1::ConversationMessageDerivation {
    let origin = match value {
        ConversationMessageDerivation::Original => {
            v1::conversation_message_derivation::Origin::Original(
                v1::ConversationOriginalMessageDerivation {},
            )
        }
        ConversationMessageDerivation::Fork {
            kind,
            source_message_id,
            source_turn_id,
            source_context_sequence,
        } => v1::conversation_message_derivation::Origin::Fork(
            v1::ConversationForkedMessageDerivation {
                source_message_id: source_message_id.into_inner(),
                source_turn_id: source_turn_id.into_inner(),
                context_position: source_context_sequence,
                kind: conversation_message_derivation_kind_to_wire(kind) as i32,
            },
        ),
    };
    v1::ConversationMessageDerivation {
        origin: Some(origin),
    }
}

const fn conversation_message_derivation_kind_to_wire(
    value: ConversationMessageDerivationKind,
) -> v1::ConversationMessageDerivationKind {
    match value {
        ConversationMessageDerivationKind::ContextCopy => {
            v1::ConversationMessageDerivationKind::ContextCopy
        }
        ConversationMessageDerivationKind::SourceAssistantCopy => {
            v1::ConversationMessageDerivationKind::SourceAssistantCopy
        }
        ConversationMessageDerivationKind::EditedUser => {
            v1::ConversationMessageDerivationKind::EditedUser
        }
    }
}

/// Converts canonical artifact metadata to the version-one wire form.
///
/// Storage identity, content digests, and object locators remain daemon-private
/// and are intentionally excluded from this renderer-facing projection.
#[must_use]
pub fn artifact_to_wire(value: Artifact) -> v1::Artifact {
    let Artifact {
        id,
        project_id,
        thread_id,
        name,
        content,
        state,
        revision,
        created_at,
        updated_at,
    } = value;
    let (media_type, byte_size, content_version) = content.map_or_else(
        || (String::new(), 0, None),
        |content| {
            (
                content.media_type,
                content.byte_size,
                Some(content.content_version),
            )
        },
    );
    v1::Artifact {
        id: id.into_inner(),
        project_id: project_id.into_inner(),
        thread_id: thread_id.map_or_else(String::new, grok_domain::ThreadId::into_inner),
        name,
        media_type,
        byte_size,
        state: match state {
            ArtifactState::Unavailable => v1::ArtifactState::Unavailable,
            ArtifactState::Available => v1::ArtifactState::Available,
            ArtifactState::Deleted => v1::ArtifactState::Deleted,
        } as i32,
        revision,
        created_at_unix_ms: created_at,
        updated_at_unix_ms: updated_at,
        content_version,
    }
}

/// Validates and converts one renderer artifact-import request.
///
/// The selected source path is moved directly into the application's
/// redacting ephemeral wrapper. It is never copied into a durable or
/// renderer-facing value.
///
/// # Errors
///
/// Returns [`ArtifactRequestError`] for malformed or unbounded request fields.
pub fn import_artifact_from_wire(
    value: v1::ImportArtifactRequest,
) -> Result<ImportArtifact, ArtifactRequestError> {
    validate_import_artifact_request(&value)?;
    let v1::ImportArtifactRequest {
        project_id,
        thread_id,
        display_name,
        media_type,
        source_path,
    } = value;
    let source = SelectedSourcePath::new(std::path::PathBuf::from(source_path))
        .map_err(|_| ArtifactRequestError::InvalidSourcePath)?;
    Ok(ImportArtifact {
        project_id,
        thread_id,
        display_name,
        media_type,
        source,
    })
}

/// Validates and converts one exact-version artifact-open request.
///
/// # Errors
///
/// Returns [`ArtifactRequestError`] for an invalid artifact identity or
/// content version.
pub fn open_artifact_from_wire(
    value: v1::OpenArtifactRequest,
) -> Result<OpenArtifact, ArtifactRequestError> {
    validate_open_artifact_request(&value)?;
    Ok(OpenArtifact {
        artifact_id: value.artifact_id,
        content_version: value.content_version,
    })
}

/// Validates and converts one exact-current-version artifact removal request.
///
/// # Errors
///
/// Returns [`ArtifactRequestError`] for an invalid artifact identity, current
/// version, or optimistic revision binding.
pub fn remove_artifact_from_wire(
    value: v1::RemoveArtifactRequest,
) -> Result<RemoveArtifact, ArtifactRequestError> {
    validate_remove_artifact_request(&value)?;
    Ok(RemoveArtifact {
        artifact_id: value.artifact_id,
        expected_revision: value.expected_revision,
        expected_content_version: value.expected_content_version,
    })
}

/// Wraps one successful canonical artifact import for the response union.
#[must_use]
pub fn imported_artifact_to_wire(value: Artifact) -> v1::ArtifactOperationResult {
    v1::ArtifactOperationResult {
        result: Some(v1::artifact_operation_result::Result::ImportedArtifact(
            artifact_to_wire(value),
        )),
    }
}

/// Wraps one committed canonical artifact tombstone for the response union.
#[must_use]
pub fn removed_artifact_to_wire(value: Artifact) -> v1::ArtifactOperationResult {
    v1::ArtifactOperationResult {
        result: Some(v1::artifact_operation_result::Result::RemovedArtifact(
            artifact_to_wire(value),
        )),
    }
}

/// Wraps a durable exact-command tombstone whose private namespace cleanup is
/// still owned by daemon recovery.
#[must_use]
pub fn artifact_removal_pending_to_wire(
    value: Artifact,
    expected_revision: u64,
    expected_content_version: u32,
) -> v1::ArtifactOperationResult {
    let artifact_id = value.id.as_str().to_owned();
    v1::ArtifactOperationResult {
        result: Some(v1::artifact_operation_result::Result::RemovalPending(
            v1::ArtifactRemovalPendingReceipt {
                artifact_id,
                expected_revision,
                expected_content_version,
                tombstone: Some(artifact_to_wire(value)),
            },
        )),
    }
}

/// Projects one terminal exact-version open receipt without a path, digest, or
/// object locator.
#[must_use]
pub fn artifact_open_receipt_to_wire(value: ArtifactOpenReceipt) -> v1::ArtifactOperationResult {
    let ArtifactOpenReceipt {
        artifact_id,
        content_version,
        status,
        failure,
    } = value;
    let failure_code = match status {
        ArtifactOpenReceiptStatus::Failed => failure.map(|failure| {
            (match failure {
                ArtifactOpenFailureCode::ContentUnavailable => {
                    v1::ArtifactOpenFailureCode::ContentUnavailable
                }
                ArtifactOpenFailureCode::PlatformUnavailable => {
                    v1::ArtifactOpenFailureCode::PlatformUnavailable
                }
                ArtifactOpenFailureCode::DeadlineExceeded => {
                    v1::ArtifactOpenFailureCode::DeadlineExceeded
                }
                ArtifactOpenFailureCode::IntegrityFailure => {
                    v1::ArtifactOpenFailureCode::IntegrityFailure
                }
                ArtifactOpenFailureCode::InterruptedBeforeDispatch => {
                    v1::ArtifactOpenFailureCode::InterruptedBeforeDispatch
                }
            }) as i32
        }),
        ArtifactOpenReceiptStatus::Opened | ArtifactOpenReceiptStatus::InterruptedNeedsReview => {
            None
        }
    };
    v1::ArtifactOperationResult {
        result: Some(v1::artifact_operation_result::Result::OpenReceipt(
            v1::ArtifactOpenReceipt {
                artifact_id: artifact_id.into_inner(),
                content_version,
                status: match status {
                    ArtifactOpenReceiptStatus::Opened => v1::ArtifactOpenReceiptStatus::Opened,
                    ArtifactOpenReceiptStatus::Failed => v1::ArtifactOpenReceiptStatus::Failed,
                    ArtifactOpenReceiptStatus::InterruptedNeedsReview => {
                        v1::ArtifactOpenReceiptStatus::InterruptedNeedsReview
                    }
                } as i32,
                failure_code,
            },
        )),
    }
}

/// Converts an automation definition to the version-one wire form.
#[must_use]
pub fn automation_to_wire(value: Automation) -> v1::Automation {
    v1::Automation {
        id: value.id.into_inner(),
        project_id: value.project_id.into_inner(),
        title: value.title,
        prompt: value.prompt,
        schedule: value.schedule,
        timezone: value.timezone,
        missed_run_policy: missed_run_policy_to_wire(value.missed_run_policy) as i32,
        overlap_policy: overlap_policy_to_wire(value.overlap_policy) as i32,
        state: match value.state {
            AutomationState::Enabled => v1::AutomationState::Enabled,
            AutomationState::Disabled => v1::AutomationState::Disabled,
            AutomationState::Archived => v1::AutomationState::Archived,
        } as i32,
        revision: value.revision,
        created_at_unix_ms: value.created_at,
        updated_at_unix_ms: value.updated_at,
    }
}

/// Converts automation history to the version-one wire form.
#[must_use]
pub fn automation_history_to_wire(value: AutomationHistoryEntry) -> v1::AutomationHistoryEntry {
    v1::AutomationHistoryEntry {
        automation_id: value.automation_id.into_inner(),
        sequence: value.sequence,
        scheduled_for_unix_ms: value.scheduled_for,
        recorded_at_unix_ms: value.recorded_at,
        status: match value.status {
            AutomationHistoryStatus::Succeeded => v1::AutomationHistoryStatus::Succeeded,
            AutomationHistoryStatus::Failed => v1::AutomationHistoryStatus::Failed,
            AutomationHistoryStatus::SkippedMissed => v1::AutomationHistoryStatus::SkippedMissed,
            AutomationHistoryStatus::SkippedOverlap => v1::AutomationHistoryStatus::SkippedOverlap,
        } as i32,
        summary: value.summary,
    }
}

/// Converts a canonical workspace search hit to the version-one wire form.
#[must_use]
pub fn workspace_search_hit_to_wire(value: WorkspaceSearchHit) -> v1::WorkspaceSearchHit {
    v1::WorkspaceSearchHit {
        id: value.id,
        project_id: value.project_id.into_inner(),
        thread_id: value
            .thread_id
            .map(ThreadId::into_inner)
            .unwrap_or_default(),
        kind: match value.kind {
            WorkspaceSearchKind::Project => v1::WorkspaceSearchKind::Project,
            WorkspaceSearchKind::Thread => v1::WorkspaceSearchKind::Thread,
            WorkspaceSearchKind::Message => v1::WorkspaceSearchKind::Message,
            WorkspaceSearchKind::Artifact => v1::WorkspaceSearchKind::Artifact,
            WorkspaceSearchKind::Automation => v1::WorkspaceSearchKind::Automation,
        } as i32,
        title: value.title,
        snippet: value.snippet,
        updated_at_unix_ms: value.updated_at,
    }
}

/// Validates and converts automation missed-run policy.
///
/// # Errors
///
/// Returns [`ProtocolConversionError`] for unknown or unspecified values.
pub fn missed_run_policy_from_wire(value: i32) -> Result<MissedRunPolicy, ProtocolConversionError> {
    match v1::MissedRunPolicy::try_from(value) {
        Ok(v1::MissedRunPolicy::RunOnce) => Ok(MissedRunPolicy::RunOnce),
        Ok(v1::MissedRunPolicy::Skip) => Ok(MissedRunPolicy::Skip),
        Ok(v1::MissedRunPolicy::Unspecified) | Err(_) => {
            Err(invalid_enum("missed_run_policy", value))
        }
    }
}

/// Validates and converts automation overlap policy.
///
/// # Errors
///
/// Returns [`ProtocolConversionError`] for unknown or unspecified values.
pub fn overlap_policy_from_wire(value: i32) -> Result<OverlapPolicy, ProtocolConversionError> {
    match v1::OverlapPolicy::try_from(value) {
        Ok(v1::OverlapPolicy::QueueOne) => Ok(OverlapPolicy::QueueOne),
        Ok(v1::OverlapPolicy::Skip) => Ok(OverlapPolicy::Skip),
        Ok(v1::OverlapPolicy::Unspecified) | Err(_) => Err(invalid_enum("overlap_policy", value)),
    }
}

/// Validates and converts an approval decision.
///
/// # Errors
///
/// Returns [`ProtocolConversionError`] for unknown or unspecified values.
pub fn approval_decision_from_wire(
    value: i32,
) -> Result<ApprovalDecision, ProtocolConversionError> {
    match v1::ApprovalDecision::try_from(value) {
        Ok(v1::ApprovalDecision::Grant) => Ok(ApprovalDecision::Grant),
        Ok(v1::ApprovalDecision::Deny) => Ok(ApprovalDecision::Deny),
        Ok(v1::ApprovalDecision::Unspecified) | Err(_) => {
            Err(invalid_enum("approval_decision", value))
        }
    }
}

fn requested_action_to_wire(value: RequestedAction) -> v1::RequestedAction {
    v1::RequestedAction {
        action: value.action,
        target: value.target,
        data_summary: value.data_summary,
        risk: match value.risk {
            ApprovalRisk::Low => v1::ApprovalRisk::Low,
            ApprovalRisk::Elevated => v1::ApprovalRisk::Elevated,
            ApprovalRisk::High => v1::ApprovalRisk::High,
            ApprovalRisk::Critical => v1::ApprovalRisk::Critical,
        } as i32,
    }
}

const fn message_role_to_wire(value: MessageRole) -> v1::MessageRole {
    match value {
        MessageRole::System => v1::MessageRole::System,
        MessageRole::User => v1::MessageRole::User,
        MessageRole::Assistant => v1::MessageRole::Assistant,
    }
}

const fn missed_run_policy_to_wire(value: MissedRunPolicy) -> v1::MissedRunPolicy {
    match value {
        MissedRunPolicy::RunOnce => v1::MissedRunPolicy::RunOnce,
        MissedRunPolicy::Skip => v1::MissedRunPolicy::Skip,
    }
}

const fn overlap_policy_to_wire(value: OverlapPolicy) -> v1::OverlapPolicy {
    match value {
        OverlapPolicy::QueueOne => v1::OverlapPolicy::QueueOne,
        OverlapPolicy::Skip => v1::OverlapPolicy::Skip,
    }
}

const fn capability(value: Capability) -> v1::Capability {
    match value {
        Capability::Chat => v1::Capability::Chat,
        Capability::Work => v1::Capability::Work,
        Capability::Files => v1::Capability::Files,
        Capability::Shell => v1::Capability::Shell,
        Capability::Mcp => v1::Capability::Mcp,
        Capability::BrowserAutomation => v1::Capability::BrowserAutomation,
        Capability::ComputerUse => v1::Capability::ComputerUse,
        Capability::Search => v1::Capability::Search,
        Capability::Research => v1::Capability::Research,
        Capability::ImagineImage => v1::Capability::ImagineImage,
        Capability::ImagineVideo => v1::Capability::ImagineVideo,
        Capability::RealtimeVoice => v1::Capability::RealtimeVoice,
        Capability::Automations => v1::Capability::Automations,
    }
}

const fn surface(value: CapabilitySurface) -> v1::CapabilitySurface {
    match value {
        CapabilitySurface::SubscriptionAcp => v1::CapabilitySurface::SubscriptionAcp,
        CapabilitySurface::XaiApi => v1::CapabilitySurface::XaiApi,
        CapabilitySurface::Desktop => v1::CapabilitySurface::Desktop,
        CapabilitySurface::ManagedAddon => v1::CapabilitySurface::ManagedAddon,
        CapabilitySurface::WebHandoff => v1::CapabilitySurface::WebHandoff,
    }
}

const fn authentication(value: AuthMethod) -> v1::AuthMethod {
    match value {
        AuthMethod::None => v1::AuthMethod::None,
        AuthMethod::SubscriptionOAuth => v1::AuthMethod::SubscriptionOauth,
        AuthMethod::XaiApiKey => v1::AuthMethod::XaiApiKey,
        AuthMethod::Either => v1::AuthMethod::Either,
    }
}

const fn availability(value: CapabilityAvailability) -> v1::CapabilityAvailability {
    match value {
        CapabilityAvailability::Available => v1::CapabilityAvailability::Available,
        CapabilityAvailability::Limited => v1::CapabilityAvailability::Limited,
        CapabilityAvailability::Unavailable => v1::CapabilityAvailability::Unavailable,
    }
}

const fn run_state_to_wire(value: RunState) -> v1::RunState {
    match value {
        RunState::Queued => v1::RunState::Queued,
        RunState::Planning => v1::RunState::Planning,
        RunState::AwaitingApproval => v1::RunState::AwaitingApproval,
        RunState::Running => v1::RunState::Running,
        RunState::Paused => v1::RunState::Paused,
        RunState::Completed => v1::RunState::Completed,
        RunState::Failed => v1::RunState::Failed,
        RunState::Cancelled => v1::RunState::Cancelled,
        RunState::InterruptedNeedsReview => v1::RunState::InterruptedNeedsReview,
    }
}

const fn conversation_turn_state_to_wire(
    value: ConversationTurnState,
) -> v1::ConversationTurnState {
    match value {
        ConversationTurnState::Reserved => v1::ConversationTurnState::Reserved,
        ConversationTurnState::ProviderStarted => v1::ConversationTurnState::ProviderStarted,
        ConversationTurnState::Completed => v1::ConversationTurnState::Completed,
        ConversationTurnState::Failed => v1::ConversationTurnState::Failed,
        ConversationTurnState::Cancelled => v1::ConversationTurnState::Cancelled,
        ConversationTurnState::InterruptedNeedsReview => {
            v1::ConversationTurnState::InterruptedNeedsReview
        }
    }
}

const fn approval_status_to_wire(value: ApprovalStatus) -> v1::ApprovalStatus {
    match value {
        ApprovalStatus::Pending => v1::ApprovalStatus::Pending,
        ApprovalStatus::Granted => v1::ApprovalStatus::Granted,
        ApprovalStatus::Denied => v1::ApprovalStatus::Denied,
        ApprovalStatus::Expired => v1::ApprovalStatus::Expired,
        ApprovalStatus::Cancelled => v1::ApprovalStatus::Cancelled,
    }
}

fn invalid_enum(field: &'static str, value: i32) -> ProtocolConversionError {
    ProtocolConversionError {
        field,
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message as _;

    #[derive(Clone, PartialEq, prost::Message)]
    struct LegacyArtifactPathProjection {
        #[prost(string, tag = "5")]
        relative_path: String,
    }

    #[test]
    fn epoch_sixteen_automation_projection_remains_definition_only() {
        let automation = Automation::new(
            grok_domain::AutomationId::new("automation-1").expect("automation id"),
            grok_domain::ProjectId::new("project-1").expect("project id"),
            "Daily brief".into(),
            "Summarize the project".into(),
            "v1;daily;09:00".into(),
            "UTC".into(),
            MissedRunPolicy::Skip,
            OverlapPolicy::QueueOne,
            false,
            10,
        )
        .expect("automation");
        let wire = automation_to_wire(automation);
        assert_eq!(wire.schedule, "v1;daily;09:00");
        assert_eq!(wire.state, v1::AutomationState::Disabled as i32);
        assert_eq!(wire.missed_run_policy, v1::MissedRunPolicy::Skip as i32);
        assert_eq!(wire.overlap_policy, v1::OverlapPolicy::QueueOne as i32);
    }

    #[test]
    fn artifact_projection_does_not_emit_the_legacy_relative_path_tag() {
        let mut artifact = Artifact::new_unavailable(
            grok_domain::ArtifactId::new("artifact-1").expect("artifact id"),
            grok_domain::ProjectId::new("project-1").expect("project id"),
            Some(ThreadId::new("thread-1").expect("thread id")),
            "notes.txt".into(),
            1_000,
        )
        .expect("artifact");
        artifact
            .record_content(
                grok_domain::ArtifactContentSummary::new(1, "text/plain".into(), 42)
                    .expect("content summary"),
                1_001,
            )
            .expect("available content");

        let encoded = artifact_to_wire(artifact).encode_to_vec();
        let legacy = LegacyArtifactPathProjection::decode(encoded.as_slice())
            .expect("current artifact decodes through legacy projection");

        assert!(legacy.relative_path.is_empty());
    }

    #[test]
    fn artifact_projection_exposes_only_bounded_current_content_metadata() {
        let unavailable = Artifact::new_unavailable(
            grok_domain::ArtifactId::new("artifact-unavailable").expect("artifact id"),
            grok_domain::ProjectId::new("project-1").expect("project id"),
            None,
            "report.pdf".into(),
            1_000,
        )
        .expect("unavailable artifact");
        let unavailable = artifact_to_wire(unavailable);
        assert_eq!(unavailable.state, v1::ArtifactState::Unavailable as i32);
        assert!(unavailable.media_type.is_empty());
        assert_eq!(unavailable.byte_size, 0);
        assert_eq!(unavailable.content_version, None);

        let mut available = Artifact::new_unavailable(
            grok_domain::ArtifactId::new("artifact-available").expect("artifact id"),
            grok_domain::ProjectId::new("project-1").expect("project id"),
            None,
            "report.pdf".into(),
            1_000,
        )
        .expect("available artifact");
        available
            .record_content(
                grok_domain::ArtifactContentSummary::new(1, "application/pdf".into(), 4096)
                    .expect("content summary"),
                1_001,
            )
            .expect("record content");
        let available = artifact_to_wire(available);
        assert_eq!(available.state, v1::ArtifactState::Available as i32);
        assert_eq!(available.media_type, "application/pdf");
        assert_eq!(available.byte_size, 4096);
        assert_eq!(available.content_version, Some(1));
    }

    #[test]
    fn artifact_request_conversion_moves_source_into_a_redacted_ephemeral_value() {
        let source_path = if cfg!(windows) {
            r"C:\Users\tester\source-canary.txt".to_owned()
        } else {
            "/home/tester/source-canary.txt".to_owned()
        };
        let imported = import_artifact_from_wire(v1::ImportArtifactRequest {
            project_id: "project-1".into(),
            thread_id: Some("thread-1".into()),
            display_name: "notes.txt".into(),
            media_type: "text/plain".into(),
            source_path: source_path.clone(),
        })
        .expect("valid import request");

        assert_eq!(imported.project_id, "project-1");
        assert_eq!(imported.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(
            imported.source.as_path(),
            std::path::Path::new(&source_path)
        );
        let debug = format!("{imported:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("source-canary"));

        assert_eq!(
            open_artifact_from_wire(v1::OpenArtifactRequest {
                artifact_id: "artifact-1".into(),
                content_version: 7,
            }),
            Ok(OpenArtifact {
                artifact_id: "artifact-1".into(),
                content_version: 7,
            })
        );
        assert_eq!(
            remove_artifact_from_wire(v1::RemoveArtifactRequest {
                artifact_id: "artifact-1".into(),
                expected_revision: 7,
                expected_content_version: 7,
            }),
            Ok(RemoveArtifact {
                artifact_id: "artifact-1".into(),
                expected_revision: 7,
                expected_content_version: 7,
            })
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn artifact_operation_results_are_closed_path_free_wrappers() {
        let imported = Artifact::new_unavailable(
            grok_domain::ArtifactId::new("artifact-imported").expect("artifact id"),
            grok_domain::ProjectId::new("project-1").expect("project id"),
            None,
            "notes.txt".into(),
            1_000,
        )
        .expect("artifact");
        assert!(matches!(
            imported_artifact_to_wire(imported).result,
            Some(v1::artifact_operation_result::Result::ImportedArtifact(_))
        ));

        let mut removed = Artifact::new_unavailable(
            grok_domain::ArtifactId::new("artifact-removed").expect("artifact id"),
            grok_domain::ProjectId::new("project-1").expect("project id"),
            None,
            "removed.txt".into(),
            1_000,
        )
        .expect("artifact");
        removed
            .record_content(
                grok_domain::ArtifactContentSummary::new(1, "text/plain".into(), 7)
                    .expect("content"),
                1_001,
            )
            .expect("available");
        removed.remove(1_002).expect("remove");
        assert!(matches!(
            removed_artifact_to_wire(removed.clone()).result,
            Some(v1::artifact_operation_result::Result::RemovedArtifact(artifact))
                if artifact.state == v1::ArtifactState::Deleted as i32
                    && artifact.content_version.is_none()
        ));
        let pending = artifact_removal_pending_to_wire(removed, 1, 1);
        assert!(matches!(
            pending.result,
            Some(v1::artifact_operation_result::Result::RemovalPending(receipt))
                if receipt.artifact_id == "artifact-removed"
                    && receipt.expected_revision == 1
                    && receipt.expected_content_version == 1
                    && receipt.tombstone.as_ref().is_some_and(|artifact|
                        artifact.state == v1::ArtifactState::Deleted as i32
                            && artifact.content_version.is_none())
        ));

        let cases = [
            (
                ArtifactOpenReceiptStatus::Opened,
                None,
                v1::ArtifactOpenReceiptStatus::Opened,
                None,
            ),
            (
                ArtifactOpenReceiptStatus::Failed,
                Some(ArtifactOpenFailureCode::ContentUnavailable),
                v1::ArtifactOpenReceiptStatus::Failed,
                Some(v1::ArtifactOpenFailureCode::ContentUnavailable as i32),
            ),
            (
                ArtifactOpenReceiptStatus::InterruptedNeedsReview,
                None,
                v1::ArtifactOpenReceiptStatus::InterruptedNeedsReview,
                None,
            ),
        ];
        for (status, failure, expected_status, expected_failure) in cases {
            let result = artifact_open_receipt_to_wire(ArtifactOpenReceipt {
                artifact_id: grok_domain::ArtifactId::new("artifact-1").expect("artifact id"),
                content_version: 7,
                status,
                failure,
            });
            let Some(v1::artifact_operation_result::Result::OpenReceipt(receipt)) = result.result
            else {
                panic!("open result must contain a receipt");
            };
            assert_eq!(receipt.artifact_id, "artifact-1");
            assert_eq!(receipt.content_version, 7);
            assert_eq!(receipt.status, expected_status as i32);
            assert_eq!(receipt.failure_code, expected_failure);
        }

        for (failure, expected) in [
            (
                ArtifactOpenFailureCode::PlatformUnavailable,
                v1::ArtifactOpenFailureCode::PlatformUnavailable,
            ),
            (
                ArtifactOpenFailureCode::DeadlineExceeded,
                v1::ArtifactOpenFailureCode::DeadlineExceeded,
            ),
            (
                ArtifactOpenFailureCode::IntegrityFailure,
                v1::ArtifactOpenFailureCode::IntegrityFailure,
            ),
            (
                ArtifactOpenFailureCode::InterruptedBeforeDispatch,
                v1::ArtifactOpenFailureCode::InterruptedBeforeDispatch,
            ),
        ] {
            let result = artifact_open_receipt_to_wire(ArtifactOpenReceipt {
                artifact_id: grok_domain::ArtifactId::new("artifact-1").expect("artifact id"),
                content_version: 7,
                status: ArtifactOpenReceiptStatus::Failed,
                failure: Some(failure),
            });
            let Some(v1::artifact_operation_result::Result::OpenReceipt(receipt)) = result.result
            else {
                panic!("open result must contain a receipt");
            };
            assert_eq!(receipt.failure_code, Some(expected as i32));
        }
    }

    #[test]
    fn unspecified_enums_never_cross_into_domain() {
        assert!(approval_decision_from_wire(99).is_err());
        assert!(missed_run_policy_from_wire(99).is_err());
        assert!(overlap_policy_from_wire(v1::OverlapPolicy::Unspecified as i32).is_err());
    }

    #[test]
    fn workspace_search_retains_canonical_conversation_routing() {
        let wire = workspace_search_hit_to_wire(WorkspaceSearchHit {
            id: "message-1".into(),
            project_id: grok_domain::ProjectId::new("project-1").expect("project"),
            thread_id: Some(ThreadId::new("thread-1").expect("thread")),
            kind: WorkspaceSearchKind::Message,
            title: "Release review".into(),
            snippet: "Evidence".into(),
            updated_at: 10,
        });

        assert_eq!(wire.thread_id, "thread-1");
        assert_eq!(wire.kind, v1::WorkspaceSearchKind::Message as i32);
    }

    #[test]
    fn conversation_turn_lineage_exposes_only_origin_source_and_depth() {
        let original = conversation_turn_lineage_to_wire(
            ConversationTurnLineage::original("local-binding-not-for-wire".into())
                .expect("original lineage"),
        );
        assert_eq!(original.origin, v1::ConversationTurnOrigin::Original as i32);
        assert!(original.source_turn_id.is_empty());
        assert_eq!(original.retry_depth, 0);

        let retry = conversation_turn_lineage_to_wire(
            ConversationTurnLineage::retry(
                grok_domain::ConversationTurnId::new("turn-source").expect("source turn"),
                "second-local-binding-not-for-wire".into(),
                3,
            )
            .expect("retry lineage"),
        );
        assert_eq!(retry.origin, v1::ConversationTurnOrigin::Retry as i32);
        assert_eq!(retry.source_turn_id, "turn-source");
        assert_eq!(retry.retry_depth, 4);

        let edited = conversation_turn_lineage_to_wire(
            ConversationTurnLineage::edit_and_branch(
                grok_domain::ConversationTurnId::new("turn-edited-source").expect("source turn"),
                "edited-local-binding-not-for-wire".into(),
            )
            .expect("edit-and-branch lineage"),
        );
        assert_eq!(
            edited.origin,
            v1::ConversationTurnOrigin::EditAndBranch as i32
        );
        assert_eq!(edited.source_turn_id, "turn-edited-source");
        assert_eq!(edited.retry_depth, 0);

        let regenerated = conversation_turn_lineage_to_wire(
            ConversationTurnLineage::regenerate(
                grok_domain::ConversationTurnId::new("turn-regenerate-source")
                    .expect("source turn"),
                "regenerated-local-binding-not-for-wire".into(),
            )
            .expect("regenerate lineage"),
        );
        assert_eq!(
            regenerated.origin,
            v1::ConversationTurnOrigin::Regenerate as i32
        );
        assert_eq!(regenerated.source_turn_id, "turn-regenerate-source");
        assert_eq!(regenerated.retry_depth, 0);
    }

    fn forked_threads() -> (Thread, Thread) {
        let project_id = grok_domain::ProjectId::new("project-1").expect("project");
        let root = Thread::new(
            ThreadId::new("thread-root").expect("root"),
            project_id.clone(),
            "Root".into(),
            1,
        )
        .expect("root thread");
        let child = Thread::new_fork(
            ThreadId::new("thread-child").expect("child"),
            project_id,
            "Root".into(),
            root.id.clone(),
            &root.lineage,
            grok_domain::ConversationTurnId::new("turn-source").expect("turn"),
            grok_domain::MessageId::new("message-source-assistant").expect("message"),
            MessageRole::Assistant,
            ConversationForkKind::Regenerate,
            2,
        )
        .expect("forked thread");
        (root, child)
    }

    #[test]
    fn forked_threads_and_messages_use_closed_wire_lineage() {
        let (root, child) = forked_threads();

        let root_wire = thread_to_wire(root.clone());
        let root_lineage = root_wire.lineage.expect("root lineage");
        assert_eq!(root_lineage.root_thread_id, "thread-root");
        assert_eq!(root_lineage.fork_depth, 0);
        assert!(matches!(
            root_lineage.origin,
            Some(v1::conversation_thread_lineage::Origin::Original(_))
        ));

        let child_wire = thread_to_wire(child.clone());
        let child_lineage = child_wire.lineage.expect("child lineage");
        assert_eq!(child_lineage.root_thread_id, "thread-root");
        assert_eq!(child_lineage.fork_depth, 1);
        let Some(v1::conversation_thread_lineage::Origin::Fork(fork)) = child_lineage.origin else {
            panic!("expected fork lineage");
        };
        assert_eq!(fork.parent_thread_id, "thread-root");
        assert_eq!(fork.source_turn_id, "turn-source");
        assert_eq!(fork.source_message_id, "message-source-assistant");
        assert_eq!(fork.kind, v1::ConversationForkKind::Regenerate as i32);

        let derived = Message::new_derived(
            grok_domain::MessageId::new("message-child-user").expect("child message"),
            child.id.clone(),
            1,
            MessageRole::User,
            "Edited prompt".into(),
            grok_domain::MessageId::new("message-source-user").expect("source message"),
            grok_domain::ConversationTurnId::new("turn-source").expect("source turn"),
            Some(1_000),
            ConversationMessageDerivationKind::EditedUser,
            2,
        )
        .expect("derived message");
        let derivation = message_to_wire(derived).derivation.expect("derivation");
        let Some(v1::conversation_message_derivation::Origin::Fork(fork)) = derivation.origin
        else {
            panic!("expected fork derivation");
        };
        assert_eq!(fork.source_message_id, "message-source-user");
        assert_eq!(fork.source_turn_id, "turn-source");
        assert_eq!(fork.context_position, Some(1_000));
        assert_eq!(
            fork.kind,
            v1::ConversationMessageDerivationKind::EditedUser as i32
        );
    }

    #[test]
    fn fork_results_and_metadata_preserve_the_bounded_projection() {
        let (root, child) = forked_threads();
        let fork_result = conversation_fork_to_wire(ConversationForkSnapshot {
            child_thread: child.clone(),
            messages: Vec::new(),
            started_turn: None,
            delivery: ConversationForkDelivery {
                child_thread_id: child.id.clone(),
                state: ConversationForkDeliveryState::Pending,
                revision: 0,
            },
        });
        assert_eq!(
            fork_result
                .child_thread
                .as_ref()
                .map(|thread| thread.id.as_str()),
            Some("thread-child")
        );
        assert!(fork_result.started_turn.is_none());
        assert_eq!(
            fork_result.delivery,
            Some(v1::ConversationForkDelivery {
                child_thread_id: "thread-child".into(),
                state: v1::ConversationForkDeliveryState::Pending as i32,
                revision: 0,
            })
        );

        let metadata = conversation_fork_metadata_to_wire(ConversationForkMetadata {
            lineage: child.lineage.clone(),
            inherited_assistant_outcomes: vec![ConversationInheritedAssistantOutcome {
                child_assistant_message_id: grok_domain::MessageId::new("message-child-assistant")
                    .expect("child assistant"),
                source_turn_id: grok_domain::ConversationTurnId::new("turn-source")
                    .expect("source turn"),
                model_id: "grok-test".into(),
                citations: vec![ConversationCitation {
                    title: Some("Source".into()),
                    url: "https://example.test/source".into(),
                }],
                usage: ConversationUsage {
                    input_tokens: 7,
                    output_tokens: 11,
                    cost_in_usd_ticks: 13,
                },
                zero_data_retention: Some(true),
            }],
            family_threads: vec![root, child],
        });
        assert_eq!(metadata.family_threads.len(), 2);
        let outcome = &metadata.inherited_assistant_outcomes[0];
        assert_eq!(
            outcome.child_assistant_message_id,
            "message-child-assistant"
        );
        assert_eq!(outcome.source_turn_id, "turn-source");
        assert_eq!(outcome.model_id, "grok-test");
        assert_eq!(outcome.zero_data_retention, Some(true));
        assert_eq!(
            outcome.usage.as_ref().map(|usage| usage.output_tokens),
            Some(11)
        );
    }

    #[test]
    fn retry_eligibility_mapping_is_closed_and_exhaustive() {
        let cases = [
            (
                ConversationRetryEligibility::Unspecified,
                v1::ConversationRetryEligibility::Unspecified,
            ),
            (
                ConversationRetryEligibility::Allowed,
                v1::ConversationRetryEligibility::Allowed,
            ),
            (
                ConversationRetryEligibility::NotNewest,
                v1::ConversationRetryEligibility::NotNewest,
            ),
            (
                ConversationRetryEligibility::SourceInProgress,
                v1::ConversationRetryEligibility::SourceInProgress,
            ),
            (
                ConversationRetryEligibility::SourceCompleted,
                v1::ConversationRetryEligibility::SourceCompleted,
            ),
            (
                ConversationRetryEligibility::SourceInterruptedNeedsReview,
                v1::ConversationRetryEligibility::SourceInterruptedNeedsReview,
            ),
            (
                ConversationRetryEligibility::FailureNotRetryable,
                v1::ConversationRetryEligibility::FailureNotRetryable,
            ),
            (
                ConversationRetryEligibility::SourceAccountUnavailable,
                v1::ConversationRetryEligibility::SourceAccountUnavailable,
            ),
            (
                ConversationRetryEligibility::DepthExhausted,
                v1::ConversationRetryEligibility::DepthExhausted,
            ),
            (
                ConversationRetryEligibility::SourceReadOnly,
                v1::ConversationRetryEligibility::SourceReadOnly,
            ),
        ];
        for (domain, wire) in cases {
            assert_eq!(conversation_retry_eligibility_to_wire(domain), wire);
        }
    }

    #[test]
    fn conversation_events_use_closed_kind_specific_wire_fields() {
        let turn_id = grok_domain::ConversationTurnId::new("turn-1").expect("turn");
        let mut log = grok_domain::ConversationTurnEventLog::new(turn_id);
        let created = log
            .append_kind(ConversationTurnEventKind::Created)
            .expect("created");
        let started = log
            .append_kind(ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::Reserved,
                to: ConversationTurnState::ProviderStarted,
            })
            .expect("started");
        let text = log
            .append_kind(ConversationTurnEventKind::TextAppended {
                start_utf8_offset: 0,
                text: "Grok".into(),
            })
            .expect("text");

        let batch = conversation_turn_event_page_to_wire(
            ConversationTurnEventPage {
                events: vec![created.clone(), started.clone(), text.clone()],
                has_more: true,
            },
            0,
        );
        assert_eq!(batch.events.len(), 3);
        assert_eq!(batch.next_sequence, 3);
        assert!(batch.has_more);
        let empty = conversation_turn_event_page_to_wire(
            ConversationTurnEventPage {
                events: Vec::new(),
                has_more: false,
            },
            7,
        );
        assert!(empty.events.is_empty());
        assert_eq!(empty.next_sequence, 7);
        assert!(!empty.has_more);

        let created = conversation_turn_event_to_wire(created);
        assert_eq!(created.kind, v1::ConversationTurnEventKind::Created as i32);
        assert_eq!(
            created.from_state,
            v1::ConversationTurnState::Unspecified as i32
        );
        assert_eq!(
            created.to_state,
            v1::ConversationTurnState::Unspecified as i32
        );
        assert_eq!(created.start_utf8_offset, 0);
        assert!(created.text_appended.is_empty());

        let started = conversation_turn_event_to_wire(started);
        assert_eq!(
            started.kind,
            v1::ConversationTurnEventKind::StateChanged as i32
        );
        assert_eq!(
            started.from_state,
            v1::ConversationTurnState::Reserved as i32
        );
        assert_eq!(
            started.to_state,
            v1::ConversationTurnState::ProviderStarted as i32
        );
        assert!(started.text_appended.is_empty());

        let text = conversation_turn_event_to_wire(text);
        assert_eq!(
            text.kind,
            v1::ConversationTurnEventKind::TextAppended as i32
        );
        assert_eq!(text.start_utf8_offset, 0);
        assert_eq!(text.text_appended, "Grok");
        assert_eq!(
            text.from_state,
            v1::ConversationTurnState::Unspecified as i32
        );
        assert_eq!(text.to_state, v1::ConversationTurnState::Unspecified as i32);
    }
}
