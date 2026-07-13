use std::error::Error;

use grok_domain::{
    Approval, ApprovalId, ApprovalRisk, ApprovalScope, ApprovalStatus, Artifact,
    ArtifactContentSummary, ArtifactId, ArtifactState, ArtifactVersion, Automation,
    AutomationHistoryEntry, AutomationHistoryStatus, AutomationId, AutomationState,
    ConversationForkKind, ConversationMessageDerivation, ConversationMessageDerivationKind,
    ConversationThreadLineage, ConversationThreadOrigin, ConversationTurnId, EffectId, EffectKind,
    EffectState, Idempotency, Message, MessageId, MessageRole, MessageState, MissedRunPolicy,
    OverlapPolicy, Project, ProjectId, ProjectState, RequestedAction, Run, RunEvent, RunEventKind,
    RunId, RunKind, RunState, SideEffect, Thread, ThreadId, ThreadState, WorkExecutionBackend,
};
use rusqlite::{Row, types::Type};

pub(crate) fn run_from_row(row: &Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: entity_id(row, 0, RunId::new)?,
        project_id: entity_id(row, 1, ProjectId::new)?,
        thread_id: entity_id(row, 2, ThreadId::new)?,
        kind: run_kind_from_i64(row.get(3)?)?,
        work_backend: optional_work_backend(row.get(4)?)?,
        state: run_state_from_i64(row.get(5)?)?,
        revision: unsigned(row, 6)?,
        created_at: unsigned(row, 7)?,
        updated_at: unsigned(row, 8)?,
    })
}

pub(crate) const fn run_kind_to_i64(kind: RunKind) -> i64 {
    match kind {
        RunKind::Unspecified => 0,
        RunKind::Chat => 1,
        RunKind::Work => 2,
        RunKind::Scheduled => 3,
    }
}

fn run_kind_from_i64(value: i64) -> rusqlite::Result<RunKind> {
    match value {
        0 => Ok(RunKind::Unspecified),
        1 => Ok(RunKind::Chat),
        2 => Ok(RunKind::Work),
        3 => Ok(RunKind::Scheduled),
        _ => invalid(3, "run kind"),
    }
}

pub(crate) const fn work_backend_to_i64(backend: WorkExecutionBackend) -> i64 {
    match backend {
        WorkExecutionBackend::HostDirect => 1,
        WorkExecutionBackend::IsolatedGuest => 2,
    }
}

fn optional_work_backend(value: Option<i64>) -> rusqlite::Result<Option<WorkExecutionBackend>> {
    match value {
        None => Ok(None),
        Some(1) => Ok(Some(WorkExecutionBackend::HostDirect)),
        Some(2) => Ok(Some(WorkExecutionBackend::IsolatedGuest)),
        Some(_) => invalid(4, "work execution backend"),
    }
}

pub(crate) fn approval_from_row(row: &Row<'_>) -> rusqlite::Result<Approval> {
    let scope_value: i64 = row.get(6)?;
    let resource_id: Option<String> = row.get(7)?;
    let scope = match (scope_value, resource_id) {
        (0, _) => ApprovalScope::Once,
        (1, _) => ApprovalScope::Run,
        (2, Some(id)) if !id.is_empty() => ApprovalScope::Resource(id),
        _ => return invalid(6, "approval scope"),
    };
    Ok(Approval {
        id: entity_id(row, 0, ApprovalId::new)?,
        run_id: entity_id(row, 1, RunId::new)?,
        request: RequestedAction {
            action: row.get(2)?,
            target: row.get(3)?,
            data_summary: row.get(4)?,
            risk: approval_risk(row.get(5)?)?,
        },
        scope,
        status: approval_status(row.get(8)?)?,
        revision: unsigned(row, 9)?,
        created_at: unsigned(row, 10)?,
        expires_at: unsigned(row, 11)?,
        decided_at: optional_unsigned(row, 12)?,
    })
}

pub(crate) fn effect_from_row(row: &Row<'_>) -> rusqlite::Result<SideEffect> {
    Ok(SideEffect {
        id: entity_id(row, 0, EffectId::new)?,
        run_id: entity_id(row, 1, RunId::new)?,
        kind: effect_kind(row.get(2)?)?,
        target: row.get(3)?,
        idempotency: idempotency(row.get(4)?)?,
        state: effect_state(row.get(5)?)?,
        revision: unsigned(row, 6)?,
        created_at: unsigned(row, 7)?,
        updated_at: unsigned(row, 8)?,
    })
}

pub(crate) fn event_from_row(row: &Row<'_>) -> rusqlite::Result<RunEvent> {
    let kind: i64 = row.get(3)?;
    let from_state: Option<i64> = row.get(4)?;
    let to_state: Option<i64> = row.get(5)?;
    let related_id: Option<String> = row.get(6)?;
    let kind = match (kind, from_state, to_state, related_id) {
        (0, _, _, _) => RunEventKind::Created,
        (1, Some(from), Some(to), _) => RunEventKind::StateChanged {
            from: run_state_from_i64(from)?,
            to: run_state_from_i64(to)?,
        },
        (2, _, _, Some(id)) => RunEventKind::ApprovalRequested {
            approval_id: ApprovalId::new(id).map_err(|error| conversion(6, error))?,
        },
        (3, _, _, Some(id)) => RunEventKind::EffectPrepared {
            effect_id: EffectId::new(id).map_err(|error| conversion(6, error))?,
        },
        (4, _, _, Some(id)) => RunEventKind::EffectNeedsReview {
            effect_id: EffectId::new(id).map_err(|error| conversion(6, error))?,
        },
        _ => return invalid(3, "run event kind"),
    };
    Ok(RunEvent {
        sequence: unsigned(row, 0)?,
        run_id: entity_id(row, 1, RunId::new)?,
        occurred_at: unsigned(row, 2)?,
        kind,
    })
}

pub(crate) fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: entity_id(row, 0, ProjectId::new)?,
        name: row.get(1)?,
        description: row.get(2)?,
        state: project_state(row.get(3)?)?,
        revision: unsigned(row, 4)?,
        created_at: unsigned(row, 5)?,
        updated_at: unsigned(row, 6)?,
    })
}

pub(crate) fn thread_from_row(row: &Row<'_>) -> rusqlite::Result<Thread> {
    let id = entity_id(row, 0, ThreadId::new)?;
    let parent_thread_id = row.get::<_, Option<String>>(7)?;
    let source_turn_id = row.get::<_, Option<String>>(8)?;
    let source_message_id = row.get::<_, Option<String>>(9)?;
    let fork_kind = row.get::<_, Option<i64>>(10)?;
    let root_thread_id = row.get::<_, Option<String>>(11)?;
    let fork_depth = row.get::<_, Option<i64>>(12)?;
    let lineage = match (
        parent_thread_id,
        source_turn_id,
        source_message_id,
        fork_kind,
        root_thread_id,
        fork_depth,
    ) {
        (None, None, None, None, None, None) => ConversationThreadLineage::original(id.clone()),
        (Some(parent), Some(turn), Some(message), Some(kind), Some(root), Some(depth)) => {
            let kind = match kind {
                0 => ConversationForkKind::Branch,
                1 => ConversationForkKind::EditAndBranch,
                2 => ConversationForkKind::Regenerate,
                _ => return invalid(10, "conversation fork kind"),
            };
            ConversationThreadLineage {
                root_thread_id: ThreadId::new(root).map_err(|error| conversion(11, error))?,
                origin: ConversationThreadOrigin::Fork {
                    parent_thread_id: ThreadId::new(parent)
                        .map_err(|error| conversion(7, error))?,
                    source_turn_id: ConversationTurnId::new(turn)
                        .map_err(|error| conversion(8, error))?,
                    source_message_id: MessageId::new(message)
                        .map_err(|error| conversion(9, error))?,
                    kind,
                },
                fork_depth: u8::try_from(depth).map_err(|error| conversion(12, error))?,
            }
        }
        _ => return invalid(7, "conversation thread lineage"),
    };
    Thread::restore(Thread {
        id,
        project_id: entity_id(row, 1, ProjectId::new)?,
        title: row.get(2)?,
        state: thread_state(row.get(3)?)?,
        lineage,
        revision: unsigned(row, 4)?,
        created_at: unsigned(row, 5)?,
        updated_at: unsigned(row, 6)?,
    })
    .map_err(|error| conversion(7, error))
}

pub(crate) fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let id = entity_id(row, 0, MessageId::new)?;
    let role = message_role(row.get(3)?)?;
    let derivation_kind = row.get::<_, Option<i64>>(9)?;
    let source_message_id = row.get::<_, Option<String>>(10)?;
    let source_turn_id = row.get::<_, Option<String>>(11)?;
    let source_context_sequence = row.get::<_, Option<i64>>(12)?;
    let derivation = match (derivation_kind, source_message_id, source_turn_id) {
        (None, None, None) if source_context_sequence.is_none() => {
            ConversationMessageDerivation::Original
        }
        (Some(kind), Some(message), Some(turn)) => {
            let kind = match kind {
                0 => ConversationMessageDerivationKind::ContextCopy,
                1 => ConversationMessageDerivationKind::SourceAssistantCopy,
                2 => ConversationMessageDerivationKind::EditedUser,
                _ => return invalid(9, "conversation message derivation kind"),
            };
            ConversationMessageDerivation::Fork {
                kind,
                source_message_id: MessageId::new(message)
                    .map_err(|error| conversion(10, error))?,
                source_turn_id: ConversationTurnId::new(turn)
                    .map_err(|error| conversion(11, error))?,
                source_context_sequence: source_context_sequence
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|error| conversion(12, error))?,
            }
        }
        _ => return invalid(9, "conversation message derivation"),
    };
    Message::restore(Message {
        id,
        thread_id: entity_id(row, 1, ThreadId::new)?,
        sequence: unsigned(row, 2)?,
        role,
        content: row.get(4)?,
        state: message_state(row.get(5)?)?,
        derivation,
        revision: unsigned(row, 6)?,
        created_at: unsigned(row, 7)?,
        updated_at: unsigned(row, 8)?,
    })
    .map_err(|error| conversion(9, error))
}

pub(crate) fn artifact_from_row(row: &Row<'_>) -> rusqlite::Result<Artifact> {
    let content_version = optional_unsigned(row, 4)?;
    let media_type = row.get::<_, Option<String>>(5)?;
    let byte_size = optional_unsigned(row, 6)?;
    let content = match (content_version, media_type, byte_size) {
        (None, None, None) => None,
        (Some(version), Some(media_type), Some(byte_size)) => Some(
            ArtifactContentSummary::new(
                u32::try_from(version).map_err(|error| conversion(4, error))?,
                media_type,
                byte_size,
            )
            .map_err(|error| conversion(4, error))?,
        ),
        _ => return invalid(4, "artifact current content"),
    };
    Artifact::restore(Artifact {
        id: entity_id(row, 0, ArtifactId::new)?,
        project_id: entity_id(row, 1, ProjectId::new)?,
        thread_id: row
            .get::<_, Option<String>>(2)?
            .map(ThreadId::new)
            .transpose()
            .map_err(|error| conversion(2, error))?,
        name: row.get(3)?,
        content,
        state: artifact_state(row.get(7)?)?,
        revision: unsigned(row, 8)?,
        created_at: unsigned(row, 9)?,
        updated_at: unsigned(row, 10)?,
    })
    .map_err(|error| conversion(4, error))
}

pub(crate) fn artifact_version_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactVersion> {
    let digest = row.get::<_, Vec<u8>>(2)?;
    ArtifactVersion::restore(ArtifactVersion {
        artifact_id: entity_id(row, 0, ArtifactId::new)?,
        version: u32::try_from(unsigned(row, 1)?).map_err(|error| conversion(1, error))?,
        sha256: digest
            .try_into()
            .map_err(|_| conversion(2, std::io::Error::other("invalid artifact digest length")))?,
        media_type: row.get(3)?,
        byte_size: unsigned(row, 4)?,
        created_at: unsigned(row, 5)?,
    })
    .map_err(|error| conversion(1, error))
}

pub(crate) fn automation_from_row(row: &Row<'_>) -> rusqlite::Result<Automation> {
    Ok(Automation {
        id: entity_id(row, 0, AutomationId::new)?,
        project_id: entity_id(row, 1, ProjectId::new)?,
        title: row.get(2)?,
        prompt: row.get(3)?,
        schedule: row.get(4)?,
        timezone: row.get(5)?,
        missed_run_policy: missed_run_policy(row.get(6)?)?,
        overlap_policy: overlap_policy(row.get(7)?)?,
        state: automation_state(row.get(8)?)?,
        revision: unsigned(row, 9)?,
        created_at: unsigned(row, 10)?,
        updated_at: unsigned(row, 11)?,
    })
}

pub(crate) fn automation_history_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<AutomationHistoryEntry> {
    Ok(AutomationHistoryEntry {
        automation_id: entity_id(row, 0, AutomationId::new)?,
        sequence: unsigned(row, 1)?,
        scheduled_for: unsigned(row, 2)?,
        recorded_at: unsigned(row, 3)?,
        status: automation_history_status(row.get(4)?)?,
        summary: row.get(5)?,
    })
}

pub(crate) const fn run_state_to_i64(value: RunState) -> i64 {
    match value {
        RunState::Queued => 0,
        RunState::Planning => 1,
        RunState::AwaitingApproval => 2,
        RunState::Running => 3,
        RunState::Paused => 4,
        RunState::Completed => 5,
        RunState::Failed => 6,
        RunState::Cancelled => 7,
        RunState::InterruptedNeedsReview => 8,
    }
}

pub(crate) fn event_parts(event: &RunEventKind) -> (i64, Option<i64>, Option<i64>, Option<&str>) {
    match event {
        RunEventKind::Created => (0, None, None, None),
        RunEventKind::StateChanged { from, to } => (
            1,
            Some(run_state_to_i64(*from)),
            Some(run_state_to_i64(*to)),
            None,
        ),
        RunEventKind::ApprovalRequested { approval_id } => {
            (2, None, None, Some(approval_id.as_str()))
        }
        RunEventKind::EffectPrepared { effect_id } => (3, None, None, Some(effect_id.as_str())),
        RunEventKind::EffectNeedsReview { effect_id } => (4, None, None, Some(effect_id.as_str())),
    }
}

pub(crate) const fn approval_risk_to_i64(value: ApprovalRisk) -> i64 {
    match value {
        ApprovalRisk::Low => 0,
        ApprovalRisk::Elevated => 1,
        ApprovalRisk::High => 2,
        ApprovalRisk::Critical => 3,
    }
}

pub(crate) fn approval_scope_parts(value: &ApprovalScope) -> (i64, Option<&str>) {
    match value {
        ApprovalScope::Once => (0, None),
        ApprovalScope::Run => (1, None),
        ApprovalScope::Resource(id) => (2, Some(id)),
    }
}

pub(crate) const fn approval_status_to_i64(value: ApprovalStatus) -> i64 {
    match value {
        ApprovalStatus::Pending => 0,
        ApprovalStatus::Granted => 1,
        ApprovalStatus::Denied => 2,
        ApprovalStatus::Expired => 3,
        ApprovalStatus::Cancelled => 4,
    }
}

pub(crate) const fn effect_kind_to_i64(value: EffectKind) -> i64 {
    match value {
        EffectKind::FileWrite => 0,
        EffectKind::ProcessExecution => 1,
        EffectKind::ExternalMutation => 2,
        EffectKind::ComputerInput => 3,
    }
}

pub(crate) const fn idempotency_to_i64(value: Idempotency) -> i64 {
    match value {
        Idempotency::Idempotent => 0,
        Idempotency::NonIdempotent => 1,
    }
}

pub(crate) const fn effect_state_to_i64(value: EffectState) -> i64 {
    match value {
        EffectState::Prepared => 0,
        EffectState::Executing => 1,
        EffectState::Succeeded => 2,
        EffectState::Failed => 3,
        EffectState::NeedsReview => 4,
    }
}

pub(crate) const fn project_state_to_i64(value: ProjectState) -> i64 {
    match value {
        ProjectState::Active => 0,
        ProjectState::Archived => 1,
    }
}

pub(crate) const fn thread_state_to_i64(value: ThreadState) -> i64 {
    match value {
        ThreadState::Open => 0,
        ThreadState::Archived => 1,
    }
}

pub(crate) const fn message_role_to_i64(value: MessageRole) -> i64 {
    match value {
        MessageRole::System => 0,
        MessageRole::User => 1,
        MessageRole::Assistant => 2,
    }
}

pub(crate) const fn message_state_to_i64(value: MessageState) -> i64 {
    match value {
        MessageState::Active => 0,
        MessageState::Deleted => 1,
    }
}

pub(crate) const fn missed_run_policy_to_i64(value: MissedRunPolicy) -> i64 {
    match value {
        MissedRunPolicy::RunOnce => 0,
        MissedRunPolicy::Skip => 1,
    }
}

pub(crate) const fn overlap_policy_to_i64(value: OverlapPolicy) -> i64 {
    match value {
        OverlapPolicy::QueueOne => 0,
        OverlapPolicy::Skip => 1,
    }
}

pub(crate) const fn automation_state_to_i64(value: AutomationState) -> i64 {
    match value {
        AutomationState::Enabled => 0,
        AutomationState::Disabled => 1,
        AutomationState::Archived => 2,
    }
}

pub(crate) const fn automation_history_status_to_i64(value: AutomationHistoryStatus) -> i64 {
    match value {
        AutomationHistoryStatus::Succeeded => 0,
        AutomationHistoryStatus::Failed => 1,
        AutomationHistoryStatus::SkippedMissed => 2,
        AutomationHistoryStatus::SkippedOverlap => 3,
    }
}

fn run_state_from_i64(value: i64) -> rusqlite::Result<RunState> {
    match value {
        0 => Ok(RunState::Queued),
        1 => Ok(RunState::Planning),
        2 => Ok(RunState::AwaitingApproval),
        3 => Ok(RunState::Running),
        4 => Ok(RunState::Paused),
        5 => Ok(RunState::Completed),
        6 => Ok(RunState::Failed),
        7 => Ok(RunState::Cancelled),
        8 => Ok(RunState::InterruptedNeedsReview),
        _ => invalid(3, "run state"),
    }
}

fn approval_risk(value: i64) -> rusqlite::Result<ApprovalRisk> {
    match value {
        0 => Ok(ApprovalRisk::Low),
        1 => Ok(ApprovalRisk::Elevated),
        2 => Ok(ApprovalRisk::High),
        3 => Ok(ApprovalRisk::Critical),
        _ => invalid(5, "approval risk"),
    }
}

fn approval_status(value: i64) -> rusqlite::Result<ApprovalStatus> {
    match value {
        0 => Ok(ApprovalStatus::Pending),
        1 => Ok(ApprovalStatus::Granted),
        2 => Ok(ApprovalStatus::Denied),
        3 => Ok(ApprovalStatus::Expired),
        4 => Ok(ApprovalStatus::Cancelled),
        _ => invalid(8, "approval status"),
    }
}

fn effect_kind(value: i64) -> rusqlite::Result<EffectKind> {
    match value {
        0 => Ok(EffectKind::FileWrite),
        1 => Ok(EffectKind::ProcessExecution),
        2 => Ok(EffectKind::ExternalMutation),
        3 => Ok(EffectKind::ComputerInput),
        _ => invalid(2, "effect kind"),
    }
}

fn idempotency(value: i64) -> rusqlite::Result<Idempotency> {
    match value {
        0 => Ok(Idempotency::Idempotent),
        1 => Ok(Idempotency::NonIdempotent),
        _ => invalid(4, "idempotency"),
    }
}

fn effect_state(value: i64) -> rusqlite::Result<EffectState> {
    match value {
        0 => Ok(EffectState::Prepared),
        1 => Ok(EffectState::Executing),
        2 => Ok(EffectState::Succeeded),
        3 => Ok(EffectState::Failed),
        4 => Ok(EffectState::NeedsReview),
        _ => invalid(5, "effect state"),
    }
}

fn project_state(value: i64) -> rusqlite::Result<ProjectState> {
    match value {
        0 => Ok(ProjectState::Active),
        1 => Ok(ProjectState::Archived),
        _ => invalid(3, "project state"),
    }
}

fn thread_state(value: i64) -> rusqlite::Result<ThreadState> {
    match value {
        0 => Ok(ThreadState::Open),
        1 => Ok(ThreadState::Archived),
        _ => invalid(3, "thread state"),
    }
}

fn message_role(value: i64) -> rusqlite::Result<MessageRole> {
    match value {
        0 => Ok(MessageRole::System),
        1 => Ok(MessageRole::User),
        2 => Ok(MessageRole::Assistant),
        _ => invalid(3, "message role"),
    }
}

fn message_state(value: i64) -> rusqlite::Result<MessageState> {
    match value {
        0 => Ok(MessageState::Active),
        1 => Ok(MessageState::Deleted),
        _ => invalid(5, "message state"),
    }
}

fn artifact_state(value: i64) -> rusqlite::Result<ArtifactState> {
    match value {
        0 => Ok(ArtifactState::Unavailable),
        1 => Ok(ArtifactState::Available),
        2 => Ok(ArtifactState::Deleted),
        _ => invalid(7, "artifact state"),
    }
}

fn missed_run_policy(value: i64) -> rusqlite::Result<MissedRunPolicy> {
    match value {
        0 => Ok(MissedRunPolicy::RunOnce),
        1 => Ok(MissedRunPolicy::Skip),
        _ => invalid(6, "missed run policy"),
    }
}

fn overlap_policy(value: i64) -> rusqlite::Result<OverlapPolicy> {
    match value {
        0 => Ok(OverlapPolicy::QueueOne),
        1 => Ok(OverlapPolicy::Skip),
        _ => invalid(7, "overlap policy"),
    }
}

fn automation_state(value: i64) -> rusqlite::Result<AutomationState> {
    match value {
        0 => Ok(AutomationState::Enabled),
        1 => Ok(AutomationState::Disabled),
        2 => Ok(AutomationState::Archived),
        _ => invalid(8, "automation state"),
    }
}

fn automation_history_status(value: i64) -> rusqlite::Result<AutomationHistoryStatus> {
    match value {
        0 => Ok(AutomationHistoryStatus::Succeeded),
        1 => Ok(AutomationHistoryStatus::Failed),
        2 => Ok(AutomationHistoryStatus::SkippedMissed),
        3 => Ok(AutomationHistoryStatus::SkippedOverlap),
        _ => invalid(4, "automation history status"),
    }
}

fn entity_id<T, E>(
    row: &Row<'_>,
    index: usize,
    constructor: impl FnOnce(String) -> Result<T, E>,
) -> rusqlite::Result<T>
where
    E: Error + Send + Sync + 'static,
{
    constructor(row.get(index)?).map_err(|error| conversion(index, error))
}

pub(crate) fn unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| conversion(index, error))
}

fn optional_unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| u64::try_from(value).map_err(|error| conversion(index, error)))
        .transpose()
}

fn invalid<T>(index: usize, name: &str) -> rusqlite::Result<T> {
    Err(conversion(
        index,
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("invalid {name}")),
    ))
}

fn conversion(index: usize, error: impl Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Integer, Box::new(error))
}
