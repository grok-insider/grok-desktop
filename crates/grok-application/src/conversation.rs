use std::{fmt::Write as _, future::Future, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures_util::{StreamExt, future::Either};
use grok_domain::{
    ConversationCitation, ConversationFailure, ConversationFailureKind, ConversationForkKind,
    ConversationMessageDerivationKind, ConversationThreadLineage, ConversationTurn,
    ConversationTurnEvent, ConversationTurnEventKind, ConversationTurnId, ConversationTurnLineage,
    ConversationTurnOrigin, ConversationTurnState, ConversationUsage, EffectId, EffectKind,
    EffectState, Idempotency, MAX_CONVERSATION_TEXT_CHUNK_BYTES, MAX_MESSAGE_BYTES, Message,
    MessageId, MessageRole, MessageState, ProjectState, Run, RunEventKind, RunState, SideEffect,
    Thread, ThreadId, ThreadState, UnixMillis,
};
use sha2::{Digest, Sha256};

use crate::{
    ApplicationError, ChatModelPreferenceStore, Citation, Clock, ContentPart, ConversationEvent,
    ConversationMessage, ConversationModelFactory, ConversationRequest, ConversationRole,
    CredentialService, GetUsageSummary, IdGenerator, ModelError, ModelErrorKind,
    ModelFailureCertainty, MutationCommand, NewRunEvent, Page, StoreError,
    SuperGrokEnrollmentService, Usage, UsageScope, UsageSummary, UsageWindow, WorkspaceService,
    mutations::mutation_command,
};

/// Maximum canonical messages copied into one immutable provider request.
pub const MAX_CONVERSATION_CONTEXT_MESSAGES: usize = 1_000;
/// Maximum aggregate UTF-8 content copied into one provider request.
pub const MAX_CONVERSATION_CONTEXT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum durable turn records a caller may request in one history page.
pub const MAX_CONVERSATION_TURN_PAGE_SIZE: usize = 200;
// One turn can contain two maximum-size messages plus one megabyte of
// citations. Materialize one result plus one look-ahead record so a nominal
// page request cannot allocate hundreds of megabytes before the IPC encoder
// applies its exact frame budget.
const MAX_CONVERSATION_TURN_MATERIALIZED_PAGE_SIZE: usize = 1;
/// Maximum incomplete turns resolved in one daemon startup pass.
pub const MAX_CONVERSATION_RECOVERY_BATCH: usize = 100;
/// Maximum durable turn events returned in one reconnect page.
pub const MAX_CONVERSATION_EVENT_BATCH: usize = 100;
/// Maximum immediate children one conversation thread may own.
pub const MAX_CONVERSATION_FORK_DIRECT_CHILDREN: usize = 64;
/// Maximum threads, including the root, in one conversation fork family.
pub const MAX_CONVERSATION_FORK_FAMILY_THREADS: usize = 256;
/// Maximum alternate request keys which may reconcile to one pending child.
///
/// The canonical creation command is separate and does not consume this bound.
pub const MAX_CONVERSATION_FORK_DELIVERY_ALIASES: usize = 64;
/// Maximum copied assistant outcomes materialized in one fork metadata response.
pub const MAX_CONVERSATION_FORK_INHERITED_OUTCOMES: usize = 256;
/// Conservative pre-encoding byte budget for one fork metadata response.
///
/// The daemon's framed protocol has a 4 MiB hard limit. Keeping the canonical
/// projection below 3 MiB leaves room for protobuf field tags, lengths, and the
/// enclosing response without relying on the encoder as the first bound.
pub const MAX_CONVERSATION_FORK_METADATA_BYTES: usize = 3 * 1024 * 1024;
const CONVERSATION_COMMAND_SCOPE: &str = "execute_conversation_turn";
const CONVERSATION_RETRY_COMMAND_SCOPE: &str = "retry_conversation_turn";
const CONVERSATION_BRANCH_COMMAND_SCOPE: &str = "branch_conversation_thread";
const CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE: &str = "edit_and_branch_conversation_turn";
const CONVERSATION_REGENERATE_COMMAND_SCOPE: &str = "regenerate_conversation_turn";
const CONVERSATION_FORK_DELIVERY_ACK_COMMAND_SCOPE: &str = "acknowledge_conversation_fork_delivery";
const CONVERSATION_CANCEL_COMMAND_SCOPE: &str = "cancel_conversation_turn";
const CONVERSATION_RECONCILIATION_COMMAND_SCOPE: &str = "reconcile_conversation_dispatch_exit";
// Use the domain's 16 KiB event bound as the progressive flush target. A
// maximum-size assistant message therefore needs at most 65 persistence
// batches across UTF-8 boundaries instead of thousands of full-log revalidations.
const CONVERSATION_TEXT_COALESCE_BYTES: usize = MAX_CONVERSATION_TEXT_CHUNK_BYTES;

/// Deadline/cancellation future supplied by the daemon runtime boundary.
pub type ConversationCancellationSignal = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Input for starting one official xAI BYOK conversation turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartConversationTurn {
    /// Existing durable conversation thread.
    pub thread_id: String,
    /// New canonical user-authored text.
    pub content: String,
    /// Optional composer override. The daemon resolves it against the live
    /// official catalog and binds the canonical ID to an empty thread.
    pub model_id: Option<String>,
}

/// Input for one explicit daemon-owned retry of a known-safe terminal turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryConversationTurn {
    /// Existing source turn loaded canonically by the daemon.
    pub source_turn_id: String,
    /// Source revision observed by the caller.
    pub expected_revision: u64,
}

/// Input for a side-effect-free child thread at one completed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchConversationThread {
    /// Canonical source turn selected by the user.
    pub source_turn_id: String,
    /// Exact source revision observed by the caller.
    pub expected_revision: u64,
}

/// Input for a child thread whose final source prompt is replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditAndBranchConversationTurn {
    /// Canonical source turn selected by the user.
    pub source_turn_id: String,
    /// Exact source revision observed by the caller.
    pub expected_revision: u64,
    /// New user content. Model and preceding context remain daemon-owned.
    pub content: String,
}

/// Input for another explicit billable attempt from a completed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegenerateConversationTurn {
    /// Canonical source turn selected by the user.
    pub source_turn_id: String,
    /// Exact source revision observed by the caller.
    pub expected_revision: u64,
}

/// Exact optimistic acknowledgement of one daemon-owned fork result delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgeConversationForkDelivery {
    /// Canonical child thread returned by the fork mutation.
    pub child_thread_id: String,
    /// Pending delivery revision observed at the presentation boundary.
    pub expected_revision: u64,
}

/// Compatibility name retained for inward-facing tests while epoch 7 migrates.
pub type ExecuteConversationTurn = StartConversationTurn;

/// Provider work which may begin only in the daemon's bounded task registry.
pub struct ConversationTurnDispatch {
    snapshot: ConversationTurnSnapshot,
    request: ConversationRequest,
    model: Arc<dyn crate::ConversationModel>,
}

impl ConversationTurnDispatch {
    /// Durable identity used to deduplicate and cancel daemon-owned tasks.
    #[must_use]
    pub fn turn_id(&self) -> &ConversationTurnId {
        &self.snapshot.turn.id
    }
}

/// Immediate durable result of an epoch-7 start command.
pub struct StartedConversationTurn {
    /// Current durable snapshot returned to the caller.
    pub snapshot: ConversationTurnSnapshot,
    /// Provider work only for a new or replayed reservation.
    pub dispatch: Option<ConversationTurnDispatch>,
}

/// Provider-free or provider-dispatching result of one explicit thread fork.
pub struct StartedConversationFork {
    /// Canonical child thread and copied-message projection.
    pub snapshot: ConversationForkSnapshot,
    /// Provider work only for a newly reserved Edit-and-branch or Regenerate turn.
    pub dispatch: Option<ConversationTurnDispatch>,
    /// True when this request key was coalesced onto an existing pending child.
    pub reconciled_pending_delivery: bool,
}

/// Durable delivery lifecycle for one canonical fork mutation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationForkDeliveryState {
    /// The result may not yet have crossed the presentation handoff boundary.
    Pending,
    /// The presentation boundary acknowledged the exact pending revision.
    Acknowledged,
}

/// Daemon-owned delivery journal projected without mutation keys or digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationForkDelivery {
    /// Canonical fork child correlated to this journal.
    pub child_thread_id: ThreadId,
    /// Current acknowledgement state.
    pub state: ConversationForkDeliveryState,
    /// Optimistic lifecycle revision; newly pending deliveries start at zero.
    pub revision: u64,
}

/// Immutable presentation outcome inherited by a copied assistant message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationInheritedAssistantOutcome {
    /// Child-owned assistant message which displays the outcome.
    pub child_assistant_message_id: MessageId,
    /// Canonical completed turn that produced the outcome.
    pub source_turn_id: ConversationTurnId,
    /// Exact recorded canonical model.
    pub model_id: String,
    /// Canonical validated citations.
    pub citations: Vec<ConversationCitation>,
    /// Canonical validated provider usage.
    pub usage: ConversationUsage,
    /// Provider-observed zero-data-retention value when present.
    pub zero_data_retention: Option<bool>,
}

/// Bounded reload projection for one thread's immutable fork ancestry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationForkMetadata {
    /// Validated lineage of the requested thread.
    pub lineage: ConversationThreadLineage,
    /// Outcomes for assistant messages copied into the requested thread.
    pub inherited_assistant_outcomes: Vec<ConversationInheritedAssistantOutcome>,
    /// Root family, including the requested thread, in deterministic order.
    pub family_threads: Vec<Thread>,
}

/// Returns a conservative size estimate for a canonical fork metadata response.
///
/// The estimate counts every UTF-8 payload plus fixed space for protobuf tags,
/// lengths, numeric fields, and enum discriminants. `None` denotes arithmetic
/// overflow and must be treated as over budget.
#[must_use]
pub fn conversation_fork_metadata_estimated_bytes(
    metadata: &ConversationForkMetadata,
) -> Option<usize> {
    const METADATA_OVERHEAD: usize = 256;
    const LINEAGE_OVERHEAD: usize = 96;
    const THREAD_OVERHEAD: usize = 192;
    const OUTCOME_OVERHEAD: usize = 192;
    const CITATION_OVERHEAD: usize = 32;

    fn add(total: &mut usize, value: usize) -> Option<()> {
        *total = total.checked_add(value)?;
        Some(())
    }

    fn add_lineage(
        total: &mut usize,
        lineage: &grok_domain::ConversationThreadLineage,
    ) -> Option<()> {
        add(total, LINEAGE_OVERHEAD)?;
        add(total, lineage.root_thread_id.as_str().len())?;
        if let grok_domain::ConversationThreadOrigin::Fork {
            parent_thread_id,
            source_turn_id,
            source_message_id,
            ..
        } = &lineage.origin
        {
            add(total, parent_thread_id.as_str().len())?;
            add(total, source_turn_id.as_str().len())?;
            add(total, source_message_id.as_str().len())?;
        }
        Some(())
    }

    let mut total = METADATA_OVERHEAD;
    add_lineage(&mut total, &metadata.lineage)?;
    for thread in &metadata.family_threads {
        add(&mut total, THREAD_OVERHEAD)?;
        add(&mut total, thread.id.as_str().len())?;
        add(&mut total, thread.project_id.as_str().len())?;
        add(&mut total, thread.title.len())?;
        add_lineage(&mut total, &thread.lineage)?;
    }
    for outcome in &metadata.inherited_assistant_outcomes {
        add(&mut total, OUTCOME_OVERHEAD)?;
        add(
            &mut total,
            outcome.child_assistant_message_id.as_str().len(),
        )?;
        add(&mut total, outcome.source_turn_id.as_str().len())?;
        add(&mut total, outcome.model_id.len())?;
        for citation in &outcome.citations {
            add(&mut total, CITATION_OVERHEAD)?;
            add(&mut total, citation.url.len())?;
            add(&mut total, citation.title.as_ref().map_or(0, String::len))?;
        }
    }
    Some(total)
}

/// Whether fork metadata is safe to materialize into one framed response.
#[must_use]
pub fn conversation_fork_metadata_is_within_bounds(metadata: &ConversationForkMetadata) -> bool {
    !metadata.family_threads.is_empty()
        && metadata.family_threads.len() <= MAX_CONVERSATION_FORK_FAMILY_THREADS
        && metadata.inherited_assistant_outcomes.len() <= MAX_CONVERSATION_FORK_INHERITED_OUTCOMES
        && conversation_fork_metadata_estimated_bytes(metadata)
            .is_some_and(|bytes| bytes <= MAX_CONVERSATION_FORK_METADATA_BYTES)
}

#[cfg(test)]
mod fork_metadata_bound_tests {
    use grok_domain::{
        ConversationCitation, ConversationThreadLineage, ConversationTurnId, ConversationUsage,
        MessageId, ProjectId, Thread, ThreadId,
    };

    use super::{
        ConversationForkMetadata, ConversationInheritedAssistantOutcome,
        MAX_CONVERSATION_FORK_INHERITED_OUTCOMES, conversation_fork_metadata_is_within_bounds,
    };

    fn metadata(outcomes: usize, citation_url_bytes: usize) -> ConversationForkMetadata {
        let root_id = ThreadId::new("metadata-root").expect("root ID");
        let root = Thread::new(
            root_id.clone(),
            ProjectId::new("metadata-project").expect("project ID"),
            "Metadata".into(),
            1,
        )
        .expect("root thread");
        let citations = if citation_url_bytes == 0 {
            Vec::new()
        } else {
            (0..128)
                .map(|index| {
                    let prefix = format!("https://example.test/{index}/");
                    ConversationCitation {
                        title: None,
                        url: format!(
                            "{prefix}{}",
                            "a".repeat(citation_url_bytes.saturating_sub(prefix.len()))
                        ),
                    }
                })
                .collect()
        };
        let outcome = ConversationInheritedAssistantOutcome {
            child_assistant_message_id: MessageId::new("metadata-assistant").expect("message ID"),
            source_turn_id: ConversationTurnId::new("metadata-turn").expect("turn ID"),
            model_id: "grok-4.3".into(),
            citations,
            usage: ConversationUsage::default(),
            zero_data_retention: Some(true),
        };
        ConversationForkMetadata {
            lineage: ConversationThreadLineage::original(root_id),
            inherited_assistant_outcomes: vec![outcome; outcomes],
            family_threads: vec![root],
        }
    }
    #[test]
    fn inherited_outcome_count_bound_is_inclusive() {
        assert!(conversation_fork_metadata_is_within_bounds(&metadata(
            MAX_CONVERSATION_FORK_INHERITED_OUTCOMES,
            0,
        )));
        assert!(!conversation_fork_metadata_is_within_bounds(&metadata(
            MAX_CONVERSATION_FORK_INHERITED_OUTCOMES + 1,
            0,
        )));
    }

    #[test]
    fn cumulative_citation_bytes_are_bounded_before_encoding() {
        assert!(conversation_fork_metadata_is_within_bounds(&metadata(
            3, 7_000,
        )));
        assert!(!conversation_fork_metadata_is_within_bounds(&metadata(
            4, 7_000,
        )));
    }
}

/// Canonical child aggregate returned for first execution and exact replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationForkSnapshot {
    /// Newly created child thread.
    pub child_thread: Thread,
    /// Child-owned immutable messages created by the fork transaction.
    pub messages: Vec<Message>,
    /// New turn for Edit-and-branch or Regenerate; absent for pure Branch.
    pub started_turn: Option<ConversationTurnSnapshot>,
    /// Durable at-least-once result delivery state for this child.
    pub delivery: ConversationForkDelivery,
}

/// Exact-key replay or pending-delivery reconciliation resolved atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationForkCommandResolution {
    /// Canonical child aggregate selected by the command.
    pub snapshot: ConversationForkSnapshot,
    /// True only when a new key was bound to an existing matching pending result.
    pub reconciled_pending_delivery: bool,
}

/// Atomic persistence result including provider input for a dispatching fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationForkReservation {
    /// Canonical child aggregate.
    pub snapshot: ConversationForkSnapshot,
    /// Exact child-owned provider context; absent for pure Branch.
    pub context: Option<Vec<Message>>,
    /// True only for the transaction that created the child.
    pub created: bool,
    /// True only when this key was atomically coalesced onto a pending child.
    pub reconciled_pending_delivery: bool,
}

/// Optional turn portion of one atomic child-thread reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationForkTurnPlan {
    /// Newly reserved child turn.
    pub turn: ConversationTurn,
    /// Fork-aware turn lineage and local generation binding.
    pub lineage: ConversationTurnLineage,
    /// New child-owned run.
    pub run: Run,
    /// Initial run audit event.
    pub run_event: NewRunEvent,
    /// Initial turn-local event.
    pub turn_event: ConversationTurnEventKind,
}

/// Complete caller-built identity plan revalidated and committed by a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationForkPlan {
    /// Scoped exact idempotency command whose result is the child thread.
    pub command: MutationCommand,
    /// Exact source turn loaded before planning and again inside the transaction.
    pub source_turn_id: ConversationTurnId,
    /// Exact source revision authorized by the caller.
    pub expected_source_revision: u64,
    /// New child with daemon-derived immutable lineage.
    pub child_thread: Thread,
    /// Child-owned context copies, edited prompt, and optional Branch assistant.
    pub messages: Vec<Message>,
    /// Present only for Edit-and-branch or Regenerate.
    pub started_turn: Option<ConversationForkTurnPlan>,
}

#[derive(Debug, Clone)]
enum ConversationForkRequest {
    Branch(BranchConversationThread),
    EditAndBranch(EditAndBranchConversationTurn),
    Regenerate(RegenerateConversationTurn),
}

impl ConversationForkRequest {
    const fn kind(&self) -> ConversationForkKind {
        match self {
            Self::Branch(_) => ConversationForkKind::Branch,
            Self::EditAndBranch(_) => ConversationForkKind::EditAndBranch,
            Self::Regenerate(_) => ConversationForkKind::Regenerate,
        }
    }

    fn source_turn_id(&self) -> &str {
        match self {
            Self::Branch(input) => &input.source_turn_id,
            Self::EditAndBranch(input) => &input.source_turn_id,
            Self::Regenerate(input) => &input.source_turn_id,
        }
    }

    const fn expected_revision(&self) -> u64 {
        match self {
            Self::Branch(input) => input.expected_revision,
            Self::EditAndBranch(input) => input.expected_revision,
            Self::Regenerate(input) => input.expected_revision,
        }
    }

    fn edited_content(&self) -> Option<&str> {
        match self {
            Self::EditAndBranch(input) => Some(&input.content),
            Self::Branch(_) | Self::Regenerate(_) => None,
        }
    }
}

/// Canonical aggregate and linked entities returned for first execution and replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurnSnapshot {
    /// Durable turn journal.
    pub turn: ConversationTurn,
    /// Canonical user message.
    pub user_message: Message,
    /// Canonical assistant message when completed.
    pub assistant_message: Option<Message>,
    /// Durable run snapshot.
    pub run: Run,
    /// Provider side-effect after dispatch was reserved.
    pub effect: Option<SideEffect>,
    /// Immutable origin and local credential-generation binding.
    pub lineage: ConversationTurnLineage,
}

/// Atomic reservation outcome including the exact immutable provider input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurnReservation {
    /// Linked canonical entities.
    pub snapshot: ConversationTurnSnapshot,
    /// Immutable active-message snapshot captured in the reservation transaction.
    pub context: Vec<Message>,
    /// True only for the transaction that created the reservation.
    pub created: bool,
}

/// Store-owned context selection for an atomic turn reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationTurnReservationSource {
    /// Capture the current canonical completed history plus the new prompt.
    CurrentThread,
    /// Reuse the exact immutable source context after replacing only its final
    /// user-message identity with the newly appended canonical retry message.
    Retry {
        /// Durable source attempt.
        source_turn_id: ConversationTurnId,
        /// Exact terminal revision authorized by the caller.
        expected_source_revision: u64,
    },
}

/// Internal thread binding state for the single supported direct xAI source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationThreadCredentialBinding {
    /// No turn has executed yet; the first reservation may bind this thread.
    UnboundEmpty,
    /// Thread is immutably bound to this local credential generation.
    Bound(String),
    /// Historical turns predate trustworthy binding and remain read-only.
    LegacyUnbound,
}

/// Durable model identity for one conversation thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationThreadModelBinding {
    /// An empty thread has not reserved its first turn.
    UnboundEmpty,
    /// Every turn in the thread uses this canonical model ID.
    Bound(String),
    /// Historical state could not be bound safely and remains read-only.
    LegacyUnbound,
}

/// Bounded reconnect page of durable turn-local events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurnEventPage {
    /// Ordered events strictly after the requested sequence.
    pub events: Vec<ConversationTurnEvent>,
    /// True when one look-ahead event proved that another page exists.
    pub has_more: bool,
}

/// Bounded startup recovery result without exposing conversation details over IPC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConversationRecoverySummary {
    /// Reservations cancelled before any provider dispatch could begin.
    pub cancelled_reserved: usize,
    /// In-flight provider calls moved to explicit human review.
    pub interrupted_needs_review: usize,
    /// True when the stable query observed additional incomplete turns.
    pub truncated: bool,
}

impl ConversationRecoverySummary {
    /// Total number of incomplete turns made terminal by this startup pass.
    #[must_use]
    pub const fn recovered(self) -> usize {
        self.cancelled_reserved + self.interrupted_needs_review
    }
}

/// Compound provider-start transition committed before network dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStartCommit {
    /// Turn moved from reserved to provider-started.
    pub turn: ConversationTurn,
    /// Expected prior turn revision.
    pub expected_turn_revision: u64,
    /// Run moved from queued through planning to running.
    pub run: Run,
    /// Expected prior run revision.
    pub expected_run_revision: u64,
    /// Executing non-idempotent provider effect.
    pub effect: SideEffect,
    /// Ordered audit events for the run changes and effect intent.
    pub events: Vec<NewRunEvent>,
    /// Exact turn-local lifecycle edge committed in the same transaction.
    pub turn_event: ConversationTurnEventKind,
}

/// Compound terminal transition committed with any assistant outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTurnCommit {
    /// Terminal turn snapshot.
    pub turn: ConversationTurn,
    /// Expected prior turn revision.
    pub expected_turn_revision: u64,
    /// Terminal run snapshot.
    pub run: Run,
    /// Expected prior run revision.
    pub expected_run_revision: u64,
    /// Terminal effect snapshot for a provider-started turn.
    pub effect: Option<SideEffect>,
    /// Expected prior effect revision when an effect is present.
    pub expected_effect_revision: Option<u64>,
    /// Assistant message inserted only for successful completion.
    pub assistant_message: Option<Message>,
    /// Ordered run audit events.
    pub events: Vec<NewRunEvent>,
    /// Exact turn-local lifecycle edge committed in the same transaction.
    pub turn_event: ConversationTurnEventKind,
}

/// Atomic exact-cancellation command and optional caller-built state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelConversationTurnCommit {
    /// Durable command identity and fingerprint for exact replay/conflict.
    pub command: MutationCommand,
    /// Turn selected by the typed cancellation intent.
    pub turn_id: ConversationTurnId,
    /// Revision observed by the caller before requesting cancellation.
    pub expected_turn_revision: u64,
    /// Exact cancellation/review transition built from the observed snapshot.
    /// This is absent only when the initial load already observed a terminal winner.
    pub terminal: Option<TerminalTurnCommit>,
}

/// Atomic durable boundary for a direct-provider conversation aggregate.
#[async_trait]
pub trait ConversationTurnStore: Send + Sync {
    /// Creates user message, run, turn, and immutable context in one transaction,
    /// or replays the exact existing command after fingerprint validation.
    #[allow(clippy::too_many_arguments)]
    async fn reserve_turn(
        &self,
        turn: ConversationTurn,
        lineage: ConversationTurnLineage,
        source: ConversationTurnReservationSource,
        user_message: Message,
        run: Run,
        event: NewRunEvent,
        turn_event: ConversationTurnEventKind,
    ) -> Result<ConversationTurnReservation, StoreError>;

    /// Loads a turn by the scoped command key and verifies its fingerprint.
    async fn load_turn_by_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError>;

    /// Loads one validated canonical turn snapshot by its durable identity.
    async fn load_turn(
        &self,
        id: &ConversationTurnId,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError>;

    /// Loads the exact immutable provider context for a turn.
    async fn load_turn_context(&self, id: &ConversationTurnId) -> Result<Vec<Message>, StoreError>;

    /// Atomically records the provider boundary, executing effect, and running run.
    async fn commit_provider_start(
        &self,
        commit: ProviderStartCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError>;

    /// Atomically records a completed, failed, cancelled, or uncertain outcome.
    async fn commit_terminal(
        &self,
        commit: TerminalTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError>;

    /// Atomically resolves or commits an exact turn-cancellation command.
    ///
    /// Implementations replay a prior matching command, conflict on key reuse,
    /// commit the supplied transition only when the current nonterminal revision
    /// matches, or bind and return a terminal race winner exactly one revision
    /// ahead of the caller. Command outcome and terminal classification are one
    /// transaction and therefore precede any provider-task abort signal.
    async fn commit_cancellation(
        &self,
        commit: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError>;

    /// Atomically resolves the daemon-internal task-exit classification.
    ///
    /// This is a distinct durable command namespace from renderer-authorized
    /// cancellation so an untrusted caller cannot pre-bind the daemon's key.
    async fn commit_dispatch_exit_reconciliation(
        &self,
        commit: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError>;

    /// Atomically appends normalized text at an exact UTF-8 byte offset.
    ///
    /// Implementations split only at UTF-8 boundaries, persist chunks no larger
    /// than 16 KiB, and replay an already committed exact append without
    /// duplicating events.
    async fn append_turn_text(
        &self,
        _turn_id: &ConversationTurnId,
        _expected_turn_revision: u64,
        _start_utf8_offset: u64,
        _text: String,
    ) -> Result<Vec<ConversationTurnEvent>, StoreError> {
        Err(StoreError::Internal(
            "conversation event storage is not implemented".into(),
        ))
    }

    /// Lists at most `limit` turn events with one-extra `has_more` detection.
    async fn list_turn_events_since(
        &self,
        _turn_id: &ConversationTurnId,
        _after_sequence: u64,
        _limit: usize,
    ) -> Result<ConversationTurnEventPage, StoreError> {
        Err(StoreError::Internal(
            "conversation event storage is not implemented".into(),
        ))
    }

    /// Lists a stable bounded prefix of nonterminal turns for startup recovery.
    async fn list_incomplete_turns_for_recovery(
        &self,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError>;

    /// Lists one stable chronological page of turns for a conversation thread.
    async fn list_thread_turns(
        &self,
        thread_id: &ThreadId,
        after: Option<&ConversationTurnId>,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError>;

    /// Aggregates official provider usage for completed turns in one scope/window.
    ///
    /// Implementations include only [`ConversationTurnState::Completed`] rows and
    /// apply the rolling lower bound derived from `as_of` for non-all-time windows.
    async fn summarize_usage(
        &self,
        scope: UsageScope,
        window: UsageWindow,
        as_of: UnixMillis,
    ) -> Result<UsageSummary, StoreError> {
        let _ = (scope, window, as_of);
        Err(StoreError::Internal(
            "conversation usage summary is not implemented".into(),
        ))
    }

    /// Checks the dynamic structural precondition for offering Retry.
    ///
    /// Implementations require the source user message to be the latest
    /// canonical message in its thread and no existing retry child.
    async fn retry_source_is_latest(&self, id: &ConversationTurnId) -> Result<bool, StoreError>;

    /// Atomically creates or exactly replays one daemon-planned child thread.
    ///
    /// Implementations re-read the source aggregate and immutable context,
    /// validate every message derivation and family bound, and commit the child,
    /// copied messages, optional turn, context, events, lineage, inherited
    /// outcomes, and command result in one transaction.
    async fn reserve_conversation_fork(
        &self,
        _plan: ConversationForkPlan,
    ) -> Result<ConversationForkReservation, StoreError> {
        Err(StoreError::Internal(
            "conversation fork storage is not implemented".into(),
        ))
    }

    /// Loads an exact fork replay after validating scoped key reuse.
    async fn load_conversation_fork_by_command(
        &self,
        _command: &MutationCommand,
    ) -> Result<Option<ConversationForkSnapshot>, StoreError> {
        Ok(None)
    }

    /// Resolves an exact key first, or atomically binds a new key to the one
    /// matching pending delivery for the same operation scope and fingerprint.
    ///
    /// An acknowledged delivery is never coalesced. Implementations must bind
    /// a reconciled key before returning so its later exact replay remains tied
    /// to the same child even after acknowledgement.
    async fn resolve_conversation_fork_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationForkCommandResolution>, StoreError> {
        Ok(self
            .load_conversation_fork_by_command(command)
            .await?
            .map(|snapshot| ConversationForkCommandResolution {
                snapshot,
                reconciled_pending_delivery: false,
            }))
    }

    /// Atomically acknowledges one exact pending delivery revision.
    ///
    /// The acknowledgement command is itself exact and replayable. Once a
    /// delivery is acknowledged, only that exact command may replay it; a new
    /// key, any other state, or any other revision conflicts.
    async fn acknowledge_conversation_fork_delivery(
        &self,
        _command: MutationCommand,
        _child_thread_id: ThreadId,
        _expected_revision: u64,
    ) -> Result<ConversationForkDelivery, StoreError> {
        Err(StoreError::Internal(
            "conversation fork delivery storage is not implemented".into(),
        ))
    }

    /// Loads bounded immutable lineage, copied outcomes, and root-family threads.
    async fn load_conversation_fork_metadata(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<ConversationForkMetadata, StoreError> {
        Err(StoreError::Internal(
            "conversation fork metadata is not implemented".into(),
        ))
    }

    /// Loads the fail-closed local credential-generation state for one thread.
    async fn thread_credential_binding(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationThreadCredentialBinding, StoreError>;

    /// Loads the fail-closed canonical model binding for one thread.
    async fn thread_model_binding(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationThreadModelBinding, StoreError>;
}

/// Coordinates official xAI calls without exposing credentials or provider DTOs.
pub struct ConversationService {
    store: Arc<dyn ConversationTurnStore>,
    workspace: Arc<WorkspaceService>,
    credentials: Arc<CredentialService>,
    factory: Arc<dyn ConversationModelFactory>,
    supergrok: Option<Arc<SuperGrokEnrollmentService>>,
    supergrok_factory: Option<Arc<dyn ConversationModelFactory>>,
    default_rail: Arc<crate::ChatRailSelection>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    model_preferences: Arc<dyn ChatModelPreferenceStore>,
}

enum ConversationCredentialUseGuard<'a> {
    Xai {
        _guard: crate::credentials::XaiCredentialUseGuard<'a>,
    },
    SuperGrok {
        _guard: tokio::sync::OwnedRwLockReadGuard<()>,
    },
}

impl ConversationService {
    /// Creates the daemon-owned conversation coordinator with a durable model policy.
    #[must_use]
    pub fn new(
        store: Arc<dyn ConversationTurnStore>,
        workspace: Arc<WorkspaceService>,
        credentials: Arc<CredentialService>,
        factory: Arc<dyn ConversationModelFactory>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        model_preferences: Arc<dyn ChatModelPreferenceStore>,
    ) -> Self {
        Self {
            store,
            workspace,
            credentials,
            factory,
            supergrok: None,
            supergrok_factory: None,
            default_rail: Arc::new(crate::ChatRailSelection::new(
                grok_domain::ChatRail::XaiApiKey,
            )),
            clock,
            ids,
            model_preferences,
        }
    }

    /// Creates a coordinator with both official credential rails.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_supergrok(
        store: Arc<dyn ConversationTurnStore>,
        workspace: Arc<WorkspaceService>,
        credentials: Arc<CredentialService>,
        api_key_factory: Arc<dyn ConversationModelFactory>,
        supergrok: Arc<SuperGrokEnrollmentService>,
        supergrok_factory: Arc<dyn ConversationModelFactory>,
        default_rail: Arc<crate::ChatRailSelection>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        model_preferences: Arc<dyn ChatModelPreferenceStore>,
    ) -> Self {
        Self {
            store,
            workspace,
            credentials,
            factory: api_key_factory,
            supergrok: Some(supergrok),
            supergrok_factory: Some(supergrok_factory),
            default_rail,
            clock,
            ids,
            model_preferences,
        }
    }

    /// Aggregates official completed-turn usage for one scope and rolling window.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError::NotFound`] when a project/thread scope does not
    /// exist, or a storage failure when persistence cannot answer.
    pub async fn usage_summary(
        &self,
        input: GetUsageSummary,
    ) -> Result<UsageSummary, ApplicationError> {
        let as_of = self.clock.now();
        self.store
            .summarize_usage(input.scope, input.window, as_of)
            .await
            .map_err(|error| match error {
                StoreError::NotFound => ApplicationError::NotFound,
                StoreError::Conflict => ApplicationError::Conflict,
                StoreError::Unavailable(message) => ApplicationError::Unavailable(message),
                StoreError::Internal(message) => ApplicationError::Storage(message),
            })
    }

    /// Durably starts or exactly replays one direct xAI conversation command.
    ///
    /// The returned snapshot is safe to return immediately. Any provider work
    /// must be placed in the daemon's bounded task registry using [`Self::dispatch`].
    ///
    /// # Errors
    ///
    /// Returns an error before reservation when credentials, model policy, or
    /// canonical input cannot be validated.
    pub async fn start(
        &self,
        input: StartConversationTurn,
        idempotency_key: &str,
        cancellation: ConversationCancellationSignal,
    ) -> Result<StartedConversationTurn, ApplicationError> {
        self.prepare_start(input, idempotency_key, cancellation)
            .await
            .map(|(started, _)| started)
    }

    /// Loads an exact durable Start replay without provider discovery or dispatch.
    ///
    /// Daemon admission uses this before reserving bounded task capacity so an
    /// already-active or terminal command remains replayable while unrelated
    /// provider tasks occupy every slot.
    ///
    /// # Errors
    ///
    /// Returns conflict when the scoped idempotency key exists with different
    /// canonical input, and propagates preference or storage failures.
    pub async fn replay_start(
        &self,
        input: &StartConversationTurn,
        idempotency_key: &str,
    ) -> Result<Option<ConversationTurnSnapshot>, ApplicationError> {
        let thread_id = ThreadId::new(input.thread_id.clone())?;
        let selected_model = match self.store.thread_model_binding(&thread_id).await? {
            ConversationThreadModelBinding::Bound(model) => {
                if input
                    .model_id
                    .as_ref()
                    .is_some_and(|requested| requested != &model)
                {
                    return Err(ApplicationError::Conflict);
                }
                model
            }
            // A successfully reserved first turn binds the model in the same
            // transaction. Therefore an unbound empty thread cannot contain an
            // exact replay and preference/provider access is unnecessary.
            ConversationThreadModelBinding::UnboundEmpty => return Ok(None),
            ConversationThreadModelBinding::LegacyUnbound => {
                return Err(ApplicationError::InvalidState(
                    "this legacy conversation thread has no trustworthy model binding".into(),
                ));
            }
        };
        let command = self.start_command(input, idempotency_key, &selected_model)?;
        Ok(self.store.load_turn_by_command(&command).await?)
    }

    /// Durably starts or exactly replays one explicit safe retry.
    ///
    /// The daemon, not the caller, loads the source prompt, immutable context,
    /// model, and credential-generation binding. Provider work is returned as a
    /// bounded dispatch plan exactly like an ordinary start.
    ///
    /// # Errors
    ///
    /// Returns conflict for a stale source revision or non-latest source, and
    /// invalid-state for a source which is not cancelled or retryable-failed.
    pub async fn retry(
        &self,
        input: RetryConversationTurn,
        idempotency_key: &str,
        cancellation: ConversationCancellationSignal,
    ) -> Result<StartedConversationTurn, ApplicationError> {
        self.prepare_retry(input, idempotency_key, cancellation)
            .await
    }

    /// Loads an exact durable Retry replay without provider discovery.
    ///
    /// This lets daemon admission return an existing active or terminal command
    /// even while unrelated dispatch slots are occupied.
    ///
    /// # Errors
    ///
    /// Returns conflict for scoped key reuse and propagates storage failures.
    pub async fn replay_retry(
        &self,
        input: &RetryConversationTurn,
        idempotency_key: &str,
    ) -> Result<Option<ConversationTurnSnapshot>, ApplicationError> {
        let source_turn_id = ConversationTurnId::new(input.source_turn_id.clone())?;
        let source = self
            .store
            .load_turn(&source_turn_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let context = self.store.load_turn_context(&source_turn_id).await?;
        let command = retry_command(input, idempotency_key, &source, &context)?;
        Ok(self.store.load_turn_by_command(&command).await?)
    }

    /// Creates or exactly replays a provider-free child at a completed turn.
    ///
    /// # Errors
    ///
    /// Returns conflict for a stale revision or scoped key reuse, invalid-state
    /// for an ineligible or unbound source, and integrity/storage failures when
    /// canonical context cannot be proven.
    pub async fn branch(
        &self,
        input: BranchConversationThread,
        idempotency_key: &str,
    ) -> Result<StartedConversationFork, ApplicationError> {
        self.prepare_fork(
            ConversationForkRequest::Branch(input),
            idempotency_key,
            None,
        )
        .await
    }

    /// Creates a child with an edited final prompt and reserves new provider work.
    ///
    /// # Errors
    ///
    /// Returns before reservation unless source, identity, model, context, and
    /// edited content are all canonically valid.
    pub async fn edit_and_branch(
        &self,
        input: EditAndBranchConversationTurn,
        idempotency_key: &str,
        cancellation: ConversationCancellationSignal,
    ) -> Result<StartedConversationFork, ApplicationError> {
        self.prepare_fork(
            ConversationForkRequest::EditAndBranch(input),
            idempotency_key,
            Some(cancellation),
        )
        .await
    }

    /// Creates a child and reserves another explicit billable completed-source attempt.
    ///
    /// # Errors
    ///
    /// Returns before reservation unless the source is completed and its exact
    /// recorded model and credential generation remain available.
    pub async fn regenerate(
        &self,
        input: RegenerateConversationTurn,
        idempotency_key: &str,
        cancellation: ConversationCancellationSignal,
    ) -> Result<StartedConversationFork, ApplicationError> {
        self.prepare_fork(
            ConversationForkRequest::Regenerate(input),
            idempotency_key,
            Some(cancellation),
        )
        .await
    }

    /// Loads an exact Branch replay without provider or credential I/O.
    ///
    /// # Errors
    ///
    /// Returns not-found for a missing source, conflict for scoped key reuse,
    /// or a canonical context/storage error.
    pub async fn replay_branch(
        &self,
        input: &BranchConversationThread,
        idempotency_key: &str,
    ) -> Result<Option<ConversationForkCommandResolution>, ApplicationError> {
        self.replay_fork(
            &ConversationForkRequest::Branch(input.clone()),
            idempotency_key,
        )
        .await
    }

    /// Loads an exact Edit-and-branch replay without provider or credential I/O.
    ///
    /// # Errors
    ///
    /// Returns not-found for a missing source, conflict for scoped key reuse,
    /// or a canonical context/storage error.
    pub async fn replay_edit_and_branch(
        &self,
        input: &EditAndBranchConversationTurn,
        idempotency_key: &str,
    ) -> Result<Option<ConversationForkCommandResolution>, ApplicationError> {
        self.replay_fork(
            &ConversationForkRequest::EditAndBranch(input.clone()),
            idempotency_key,
        )
        .await
    }

    /// Loads an exact Regenerate replay without provider or credential I/O.
    ///
    /// # Errors
    ///
    /// Returns not-found for a missing source, conflict for scoped key reuse,
    /// or a canonical context/storage error.
    pub async fn replay_regenerate(
        &self,
        input: &RegenerateConversationTurn,
        idempotency_key: &str,
    ) -> Result<Option<ConversationForkCommandResolution>, ApplicationError> {
        self.replay_fork(
            &ConversationForkRequest::Regenerate(input.clone()),
            idempotency_key,
        )
        .await
    }

    /// Loads bounded immutable fork metadata for renderer aggregate validation.
    ///
    /// # Errors
    ///
    /// Returns not-found/storage errors or integrity failure for an incomplete,
    /// unbounded, or uncorrelated family projection.
    pub async fn fork_metadata(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationForkMetadata, ApplicationError> {
        let metadata = self
            .store
            .load_conversation_fork_metadata(thread_id)
            .await?;
        if !conversation_fork_metadata_is_within_bounds(&metadata)
            || !metadata
                .family_threads
                .iter()
                .any(|thread| thread.id == *thread_id)
        {
            return Err(ApplicationError::Integrity(
                "conversation fork metadata is incomplete or exceeds its bound".into(),
            ));
        }
        Ok(metadata)
    }

    async fn replay_fork(
        &self,
        request: &ConversationForkRequest,
        idempotency_key: &str,
    ) -> Result<Option<ConversationForkCommandResolution>, ApplicationError> {
        let source_turn_id = ConversationTurnId::new(request.source_turn_id().to_owned())?;
        let source = self
            .store
            .load_turn(&source_turn_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let source_context = self.store.load_turn_context(&source_turn_id).await?;
        let command = fork_command(request, idempotency_key, &source, &source_context)?;
        Ok(self
            .store
            .resolve_conversation_fork_command(&command)
            .await?)
    }

    #[allow(clippy::too_many_lines)]
    async fn prepare_fork(
        &self,
        request: ConversationForkRequest,
        idempotency_key: &str,
        cancellation: Option<ConversationCancellationSignal>,
    ) -> Result<StartedConversationFork, ApplicationError> {
        let source_turn_id = ConversationTurnId::new(request.source_turn_id().to_owned())?;
        let source = self
            .store
            .load_turn(&source_turn_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let source_context = self.store.load_turn_context(&source_turn_id).await?;
        let command = fork_command(&request, idempotency_key, &source, &source_context)?;
        if let Some(existing) = self
            .store
            .resolve_conversation_fork_command(&command)
            .await?
            && (existing.reconciled_pending_delivery
                || existing
                    .snapshot
                    .started_turn
                    .as_ref()
                    .is_none_or(|turn| turn.turn.state != ConversationTurnState::Reserved))
        {
            return Ok(StartedConversationFork {
                snapshot: existing.snapshot,
                dispatch: None,
                reconciled_pending_delivery: existing.reconciled_pending_delivery,
            });
        }

        if source.turn.revision != request.expected_revision() {
            return Err(ApplicationError::Conflict);
        }
        ensure_fork_source_eligible(request.kind(), &source)?;
        validate_frozen_source_context(&source, &source_context)?;

        let parent = self.workspace.get_thread(&source.turn.thread_id).await?;
        let project = self.workspace.get_project(&source.turn.project_id).await?;
        if parent.project_id != source.turn.project_id || project.state != ProjectState::Active {
            return Err(ApplicationError::InvalidState(
                "conversation forks require an active source project".into(),
            ));
        }
        let source_binding = source
            .lineage
            .credential_binding_id
            .clone()
            .ok_or_else(|| {
                ApplicationError::InvalidState(
                    "legacy conversation turns without a credential binding cannot be forked"
                        .into(),
                )
            })?;
        match self
            .store
            .thread_credential_binding(&source.turn.thread_id)
            .await?
        {
            ConversationThreadCredentialBinding::Bound(binding) if binding == source_binding => {}
            ConversationThreadCredentialBinding::Bound(_)
            | ConversationThreadCredentialBinding::UnboundEmpty
            | ConversationThreadCredentialBinding::LegacyUnbound => {
                return Err(ApplicationError::InvalidState(
                    "the fork source has no trustworthy thread credential binding".into(),
                ));
            }
        }

        let kind = request.kind();
        let source_rail = source.lineage.rail;
        let model_id = source.turn.model_id.clone();
        // Build and validate the complete renderer-influenced plan before any
        // credential-bearing model discovery or provider I/O.
        let plan = self.build_fork_plan(
            &request,
            command,
            source,
            &source_context,
            &parent,
            &source_binding,
        )?;
        if kind == ConversationForkKind::Branch {
            let reservation = self.store.reserve_conversation_fork(plan).await?;
            if reservation.snapshot.started_turn.is_some() || reservation.context.is_some() {
                return Err(ApplicationError::Integrity(
                    "a provider-free branch returned provider work".into(),
                ));
            }
            let reconciled_pending_delivery = reservation.reconciled_pending_delivery;
            return Ok(StartedConversationFork {
                snapshot: reservation.snapshot,
                dispatch: None,
                reconciled_pending_delivery,
            });
        }

        let cancellation = cancellation.ok_or_else(|| {
            ApplicationError::Integrity("a dispatching fork is missing its deadline".into())
        })?;
        let (model, current_binding, canonical_model, _cancellation) = self
            .preflight_model(
                source_rail,
                &model_id,
                true,
                Some(&source_binding),
                cancellation,
            )
            .await?;
        if canonical_model != model_id {
            return Err(ApplicationError::InvalidState(
                "the fork source model is no longer canonical".into(),
            ));
        }
        if current_binding != source_binding {
            return Err(ApplicationError::Integrity(
                "conversation fork credential generation changed during preflight".into(),
            ));
        }
        let reservation = self.store.reserve_conversation_fork(plan).await?;
        if reservation.reconciled_pending_delivery {
            return Ok(StartedConversationFork {
                snapshot: reservation.snapshot,
                dispatch: None,
                reconciled_pending_delivery: true,
            });
        }
        let Some(snapshot) = reservation.snapshot.started_turn.clone() else {
            return Err(ApplicationError::Integrity(
                "a dispatching fork did not return its child turn".into(),
            ));
        };
        if snapshot.turn.state != ConversationTurnState::Reserved {
            return Ok(StartedConversationFork {
                snapshot: reservation.snapshot,
                dispatch: None,
                reconciled_pending_delivery: false,
            });
        }
        let context = reservation.context.ok_or_else(|| {
            ApplicationError::Integrity("a dispatching fork is missing frozen context".into())
        })?;
        let provider_request = provider_request(&model_id, &context)?;
        Ok(StartedConversationFork {
            snapshot: reservation.snapshot,
            dispatch: Some(ConversationTurnDispatch {
                snapshot,
                request: provider_request,
                model,
            }),
            reconciled_pending_delivery: false,
        })
    }

    /// Marks one exact daemon-owned fork-result presentation handoff without
    /// model, credential, provider, workspace, or fork-metadata I/O.
    ///
    /// This journal edge is not approval, authorization, or proof that a user
    /// viewed the result.
    ///
    /// # Errors
    ///
    /// Returns conflict for key reuse, a stale/non-pending revision, or a child
    /// which does not identify a canonical fork delivery; returns not-found for
    /// a missing child.
    pub async fn acknowledge_fork_delivery(
        &self,
        input: AcknowledgeConversationForkDelivery,
        idempotency_key: &str,
    ) -> Result<ConversationForkDelivery, ApplicationError> {
        let child_thread_id = ThreadId::new(input.child_thread_id)?;
        let command = mutation_command(
            CONVERSATION_FORK_DELIVERY_ACK_COMMAND_SCOPE,
            idempotency_key,
            &[
                child_thread_id.to_string(),
                input.expected_revision.to_string(),
            ],
        )?;
        Ok(self
            .store
            .acknowledge_conversation_fork_delivery(
                command,
                child_thread_id,
                input.expected_revision,
            )
            .await?)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn build_fork_plan(
        &self,
        request: &ConversationForkRequest,
        command: MutationCommand,
        source: ConversationTurnSnapshot,
        source_context: &[Message],
        parent: &Thread,
        credential_binding_id: &str,
    ) -> Result<ConversationForkPlan, ApplicationError> {
        if source_context.last() != Some(&source.user_message) {
            return Err(ApplicationError::Integrity(
                "fork source context does not end at its canonical user message".into(),
            ));
        }
        let kind = request.kind();
        let source_message = match kind {
            ConversationForkKind::EditAndBranch => &source.user_message,
            ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
                source.assistant_message.as_ref().ok_or_else(|| {
                    ApplicationError::Integrity(
                        "completed fork source is missing its assistant message".into(),
                    )
                })?
            }
        };
        let now = self
            .clock
            .now()
            .max(source.turn.updated_at)
            .max(parent.updated_at);
        let child_thread = Thread::new_fork(
            ThreadId::new(self.ids.generate("thread"))?,
            parent.project_id.clone(),
            parent.title.clone(),
            parent.id.clone(),
            &parent.lineage,
            source.turn.id.clone(),
            source_message.id.clone(),
            source_message.role,
            kind,
            now,
        )?;

        let context_copy_count = if kind == ConversationForkKind::EditAndBranch {
            source_context
                .len()
                .checked_sub(1)
                .ok_or_else(|| ApplicationError::Integrity("fork source context is empty".into()))?
        } else {
            source_context.len()
        };
        let mut messages = Vec::with_capacity(
            context_copy_count
                + usize::from(matches!(
                    kind,
                    ConversationForkKind::Branch | ConversationForkKind::EditAndBranch
                )),
        );
        for (index, source_message) in source_context.iter().take(context_copy_count).enumerate() {
            let sequence = u64::try_from(index + 1).map_err(|_| {
                ApplicationError::InvalidState("conversation fork sequence exhausted".into())
            })?;
            let source_context_sequence = u32::try_from(index + 1).map_err(|_| {
                ApplicationError::InvalidState(
                    "conversation fork context position exhausted".into(),
                )
            })?;
            messages.push(Message::new_derived(
                MessageId::new(self.ids.generate("message"))?,
                child_thread.id.clone(),
                sequence,
                source_message.role,
                source_message.content.clone(),
                source_message.id.clone(),
                source.turn.id.clone(),
                Some(source_context_sequence),
                ConversationMessageDerivationKind::ContextCopy,
                now,
            )?);
        }

        if kind == ConversationForkKind::EditAndBranch {
            let content = request.edited_content().ok_or_else(|| {
                ApplicationError::Integrity("edit-and-branch content is missing".into())
            })?;
            if content == source.user_message.content {
                return Err(ApplicationError::InvalidInput(
                    "edited conversation content must differ from the source prompt".into(),
                ));
            }
            let sequence = u64::try_from(messages.len() + 1).map_err(|_| {
                ApplicationError::InvalidState("conversation fork sequence exhausted".into())
            })?;
            let context_position = u32::try_from(source_context.len()).map_err(|_| {
                ApplicationError::InvalidState(
                    "conversation fork context position exhausted".into(),
                )
            })?;
            messages.push(Message::new_derived(
                MessageId::new(self.ids.generate("message"))?,
                child_thread.id.clone(),
                sequence,
                MessageRole::User,
                content.to_owned(),
                source.user_message.id.clone(),
                source.turn.id.clone(),
                Some(context_position),
                ConversationMessageDerivationKind::EditedUser,
                now,
            )?);
        } else if kind == ConversationForkKind::Branch {
            let assistant = source.assistant_message.as_ref().ok_or_else(|| {
                ApplicationError::Integrity(
                    "completed branch source is missing its assistant message".into(),
                )
            })?;
            let sequence = u64::try_from(messages.len() + 1).map_err(|_| {
                ApplicationError::InvalidState("conversation fork sequence exhausted".into())
            })?;
            messages.push(Message::new_derived(
                MessageId::new(self.ids.generate("message"))?,
                child_thread.id.clone(),
                sequence,
                MessageRole::Assistant,
                assistant.content.clone(),
                assistant.id.clone(),
                source.turn.id.clone(),
                None,
                ConversationMessageDerivationKind::SourceAssistantCopy,
                now,
            )?);
        }

        let started_turn = match kind {
            ConversationForkKind::Branch => None,
            ConversationForkKind::EditAndBranch | ConversationForkKind::Regenerate => {
                let user_message = messages.last().ok_or_else(|| {
                    ApplicationError::Integrity("dispatching fork has no user message".into())
                })?;
                if user_message.role != MessageRole::User {
                    return Err(ApplicationError::Integrity(
                        "dispatching fork does not end with a user message".into(),
                    ));
                }
                // Reject an edited context which would reserve a child that can
                // never be dispatched under the canonical provider bounds.
                provider_request(&source.turn.model_id, &messages)?;
                let run = Run::queued(
                    grok_domain::RunId::new(self.ids.generate("run"))?,
                    child_thread.project_id.clone(),
                    child_thread.id.clone(),
                    now,
                );
                let turn = ConversationTurn::reserve(
                    ConversationTurnId::new(self.ids.generate("turn"))?,
                    command.key.clone(),
                    command.fingerprint,
                    child_thread.project_id.clone(),
                    child_thread.id.clone(),
                    user_message.id.clone(),
                    run.id.clone(),
                    source.turn.model_id.clone(),
                    now,
                )?;
                let lineage = match kind {
                    ConversationForkKind::EditAndBranch => {
                        ConversationTurnLineage::edit_and_branch_on(
                            source.turn.id.clone(),
                            source.lineage.rail,
                            credential_binding_id.to_owned(),
                        )?
                    }
                    ConversationForkKind::Regenerate => ConversationTurnLineage::regenerate_on(
                        source.turn.id.clone(),
                        source.lineage.rail,
                        credential_binding_id.to_owned(),
                    )?,
                    ConversationForkKind::Branch => unreachable!("handled above"),
                };
                Some(ConversationForkTurnPlan {
                    turn,
                    lineage,
                    run,
                    run_event: NewRunEvent {
                        occurred_at: now,
                        kind: RunEventKind::Created,
                    },
                    turn_event: ConversationTurnEventKind::Created,
                })
            }
        };
        Ok(ConversationForkPlan {
            command,
            source_turn_id: source.turn.id,
            expected_source_revision: request.expected_revision(),
            child_thread,
            messages,
            started_turn,
        })
    }

    fn start_command(
        &self,
        input: &StartConversationTurn,
        idempotency_key: &str,
        selected_model: &str,
    ) -> Result<MutationCommand, ApplicationError> {
        let mut command_parts = vec![
            input.thread_id.clone(),
            selected_model.to_owned(),
            input.content.clone(),
            "tools:none".into(),
            "provider_store:false".into(),
        ];
        if self.default_rail.current() == grok_domain::ChatRail::SuperGrokApi {
            command_parts.push("rail:supergrok_api".into());
        }
        mutation_command(CONVERSATION_COMMAND_SCOPE, idempotency_key, &command_parts)
    }

    async fn expected_start_credential_binding(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<String>, ApplicationError> {
        match self.store.thread_credential_binding(thread_id).await? {
            ConversationThreadCredentialBinding::UnboundEmpty => Ok(None),
            ConversationThreadCredentialBinding::Bound(binding) => Ok(Some(binding)),
            ConversationThreadCredentialBinding::LegacyUnbound => {
                Err(ApplicationError::InvalidState(
                    "this legacy conversation thread has no trustworthy credential binding; start a new thread to continue"
                        .into(),
                ))
            }
        }
    }

    async fn writable_thread(&self, thread_id: &ThreadId) -> Result<Thread, ApplicationError> {
        let thread = self.workspace.get_thread(thread_id).await?;
        if thread.state != ThreadState::Open {
            return Err(ApplicationError::InvalidState(
                "the conversation thread is archived".into(),
            ));
        }
        let project = self.workspace.get_project(&thread.project_id).await?;
        if project.state != ProjectState::Active {
            return Err(ApplicationError::InvalidState(
                "the conversation project is archived".into(),
            ));
        }
        Ok(thread)
    }

    async fn preflight_start_model(
        &self,
        input: &StartConversationTurn,
        thread_id: &ThreadId,
        cancellation: ConversationCancellationSignal,
    ) -> Result<
        (
            Arc<dyn crate::ConversationModel>,
            String,
            String,
            ConversationCancellationSignal,
        ),
        ApplicationError,
    > {
        let expected_credential_binding = self.expected_start_credential_binding(thread_id).await?;
        let model_binding = self.store.thread_model_binding(thread_id).await?;
        let (requested_model, require_canonical_model) = match &model_binding {
            ConversationThreadModelBinding::Bound(bound) => {
                if input
                    .model_id
                    .as_ref()
                    .is_some_and(|requested| requested != bound)
                {
                    return Err(ApplicationError::Conflict);
                }
                (bound.clone(), true)
            }
            ConversationThreadModelBinding::UnboundEmpty => {
                let preference = self.model_preferences.get_chat_model_preference().await?;
                (
                    input
                        .model_id
                        .clone()
                        .unwrap_or(preference.selected_model_id),
                    input.model_id.is_some() || preference.revision > 0,
                )
            }
            ConversationThreadModelBinding::LegacyUnbound => {
                return Err(ApplicationError::InvalidState(
                    "this legacy conversation thread has no trustworthy model binding; start a new thread to continue"
                        .into(),
                ));
            }
        };
        let prepared = self
            .preflight_model(
                self.default_rail.current(),
                &requested_model,
                require_canonical_model,
                expected_credential_binding.as_deref(),
                cancellation,
            )
            .await?;
        if let ConversationThreadModelBinding::Bound(bound) = model_binding
            && bound != prepared.2
        {
            return Err(ApplicationError::InvalidState(
                "the requested model conflicts with this conversation thread's model binding"
                    .into(),
            ));
        }
        Ok(prepared)
    }

    async fn prepare_start(
        &self,
        input: StartConversationTurn,
        idempotency_key: &str,
        cancellation: ConversationCancellationSignal,
    ) -> Result<(StartedConversationTurn, ConversationCancellationSignal), ApplicationError> {
        if let Some(existing) = self.replay_start(&input, idempotency_key).await?
            && existing.turn.state != ConversationTurnState::Reserved
        {
            return Ok((
                StartedConversationTurn {
                    snapshot: existing,
                    dispatch: None,
                },
                cancellation,
            ));
        }
        let thread_id = ThreadId::new(input.thread_id.clone())?;
        let thread = self.writable_thread(&thread_id).await?;
        let (model, credential_binding_id, selected_model, cancellation) = self
            .preflight_start_model(&input, &thread_id, cancellation)
            .await?;
        let command = self.start_command(&input, idempotency_key, &selected_model)?;

        if let Some(existing) = self.store.load_turn_by_command(&command).await?
            && existing.turn.state != ConversationTurnState::Reserved
        {
            return Ok((
                StartedConversationTurn {
                    snapshot: existing,
                    dispatch: None,
                },
                cancellation,
            ));
        }

        let now = self.clock.now();
        let user_message = Message::new(
            MessageId::new(self.ids.generate("message"))?,
            thread_id.clone(),
            MessageRole::User,
            input.content,
            now,
        )?;
        let run = Run::queued(
            grok_domain::RunId::new(self.ids.generate("run"))?,
            thread.project_id.clone(),
            thread_id,
            now,
        );
        let turn = ConversationTurn::reserve(
            ConversationTurnId::new(self.ids.generate("turn"))?,
            idempotency_key.into(),
            command.fingerprint,
            thread.project_id,
            run.thread_id.clone(),
            user_message.id.clone(),
            run.id.clone(),
            selected_model.clone(),
            now,
        )?;
        let reservation = self
            .store
            .reserve_turn(
                turn,
                ConversationTurnLineage::original_on(
                    self.default_rail.current(),
                    credential_binding_id,
                )?,
                ConversationTurnReservationSource::CurrentThread,
                user_message,
                run,
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::Created,
                },
                ConversationTurnEventKind::Created,
            )
            .await?;
        if reservation.snapshot.turn.state != ConversationTurnState::Reserved {
            return Ok((
                StartedConversationTurn {
                    snapshot: reservation.snapshot,
                    dispatch: None,
                },
                cancellation,
            ));
        }

        let request = provider_request(&selected_model, &reservation.context)?;
        let snapshot = reservation.snapshot;
        let dispatch = ConversationTurnDispatch {
            snapshot: snapshot.clone(),
            request,
            model,
        };
        Ok((
            StartedConversationTurn {
                snapshot,
                dispatch: Some(dispatch),
            },
            cancellation,
        ))
    }

    #[allow(clippy::too_many_lines)]
    async fn prepare_retry(
        &self,
        input: RetryConversationTurn,
        idempotency_key: &str,
        cancellation: ConversationCancellationSignal,
    ) -> Result<StartedConversationTurn, ApplicationError> {
        let source_turn_id = ConversationTurnId::new(input.source_turn_id.clone())?;
        let source = self
            .store
            .load_turn(&source_turn_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let source_context = self.store.load_turn_context(&source_turn_id).await?;
        let command = retry_command(&input, idempotency_key, &source, &source_context)?;
        let existing_command = self.store.load_turn_by_command(&command).await?;
        if let Some(existing) = existing_command.as_ref()
            && existing.turn.state != ConversationTurnState::Reserved
        {
            return Ok(StartedConversationTurn {
                snapshot: existing.clone(),
                dispatch: None,
            });
        }
        let replaying_reserved_command = existing_command.is_some();

        if source.turn.revision != input.expected_revision {
            return Err(ApplicationError::Conflict);
        }
        ensure_retry_source_eligible(&source)?;
        if source.lineage.retry_depth >= 64 {
            return Err(ApplicationError::InvalidState(
                "the conversation retry depth is exhausted".into(),
            ));
        }
        self.writable_thread(&source.turn.thread_id).await?;
        let source_binding = source
            .lineage
            .credential_binding_id
            .as_deref()
            .ok_or_else(|| {
                ApplicationError::InvalidState(
                    "legacy conversation turns without a credential binding cannot be retried"
                        .into(),
                )
            })?;
        if !replaying_reserved_command
            && !self.store.retry_source_is_latest(&source_turn_id).await?
        {
            return Err(ApplicationError::Conflict);
        }
        match self
            .store
            .thread_credential_binding(&source.turn.thread_id)
            .await?
        {
            ConversationThreadCredentialBinding::Bound(binding) if binding == source_binding => {}
            ConversationThreadCredentialBinding::Bound(_)
            | ConversationThreadCredentialBinding::UnboundEmpty
            | ConversationThreadCredentialBinding::LegacyUnbound => {
                return Err(ApplicationError::InvalidState(
                    "the retry source has no trustworthy thread credential binding; start a new thread to continue"
                        .into(),
                ));
            }
        }
        let model_id = source.turn.model_id.clone();
        let (model, current_binding, canonical_model, _cancellation) = self
            .preflight_model(
                source.lineage.rail,
                &model_id,
                true,
                Some(source_binding),
                cancellation,
            )
            .await?;
        if canonical_model != model_id {
            return Err(ApplicationError::InvalidState(
                "the retry source model is no longer canonical".into(),
            ));
        }
        let source_retry_depth = source.lineage.retry_depth;

        let now = self.clock.now().max(source.turn.updated_at);
        let user_message = Message::new(
            MessageId::new(self.ids.generate("message"))?,
            source.turn.thread_id.clone(),
            MessageRole::User,
            source.user_message.content.clone(),
            now,
        )?;
        let run = Run::queued(
            grok_domain::RunId::new(self.ids.generate("run"))?,
            source.turn.project_id.clone(),
            source.turn.thread_id.clone(),
            now,
        );
        let turn = ConversationTurn::reserve(
            ConversationTurnId::new(self.ids.generate("turn"))?,
            command.key.clone(),
            command.fingerprint,
            source.turn.project_id,
            source.turn.thread_id,
            user_message.id.clone(),
            run.id.clone(),
            model_id.clone(),
            now,
        )?;
        let lineage = ConversationTurnLineage::retry_on(
            source_turn_id.clone(),
            source.lineage.rail,
            current_binding,
            source_retry_depth,
        )?;
        let reservation = self
            .store
            .reserve_turn(
                turn,
                lineage,
                ConversationTurnReservationSource::Retry {
                    source_turn_id,
                    expected_source_revision: input.expected_revision,
                },
                user_message,
                run,
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::Created,
                },
                ConversationTurnEventKind::Created,
            )
            .await?;
        if reservation.snapshot.turn.state != ConversationTurnState::Reserved {
            return Ok(StartedConversationTurn {
                snapshot: reservation.snapshot,
                dispatch: None,
            });
        }
        let request = provider_request(&model_id, &reservation.context)?;
        let snapshot = reservation.snapshot;
        let dispatch = ConversationTurnDispatch {
            snapshot: snapshot.clone(),
            request,
            model,
        };
        Ok(StartedConversationTurn {
            snapshot,
            dispatch: Some(dispatch),
        })
    }

    /// Executes provider work for one previously returned durable dispatch plan.
    ///
    /// # Errors
    ///
    /// Returns a bounded application error only when no durable winner can be
    /// reconciled. Cancellation never replays a provider-started request.
    pub async fn dispatch(
        &self,
        dispatch: ConversationTurnDispatch,
        cancellation: ConversationCancellationSignal,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let ConversationTurnDispatch {
            snapshot,
            request,
            model,
        } = dispatch;
        let credential_binding = snapshot
            .lineage
            .credential_binding_id
            .as_deref()
            .ok_or_else(|| {
                ApplicationError::Integrity(
                    "conversation dispatch is missing its credential binding".into(),
                )
            })?;
        let credential_use = match snapshot.lineage.rail {
            grok_domain::ChatRail::XaiApiKey => ConversationCredentialUseGuard::Xai {
                _guard: self
                    .credentials
                    .acquire_xai_credential_use(credential_binding)
                    .await?,
            },
            grok_domain::ChatRail::SuperGrokApi => {
                let generation = parse_supergrok_binding(credential_binding)?;
                let service = self.supergrok.as_ref().ok_or_else(|| {
                    ApplicationError::Unavailable("SuperGrok API Chat is not configured".into())
                })?;
                ConversationCredentialUseGuard::SuperGrok {
                    _guard: service.acquire_credential_use(generation).await?,
                }
            }
        };
        let provider_fingerprint = provider_request_fingerprint(&request);
        let started = match self
            .start_provider(snapshot.clone(), provider_fingerprint)
            .await
        {
            Ok(started) => started,
            Err(ApplicationError::Conflict) => {
                return self
                    .store
                    .load_turn(&snapshot.turn.id)
                    .await?
                    .ok_or_else(|| {
                        ApplicationError::Storage(
                            "conversation turn disappeared after a revision conflict".into(),
                        )
                    });
            }
            Err(error) => return Err(error),
        };

        let stream_outcome = race_cancellation(model.stream(request), cancellation).await;
        drop(credential_use);
        let (stream, cancellation) = match stream_outcome {
            Ok((result, cancellation)) => match result {
                Ok(stream) => (stream, cancellation),
                Err(error) => return self.finish_provider_error(started, error).await,
            },
            Err(()) => return self.interrupt_or_reconcile(started).await,
        };
        self.consume_stream(started, stream, cancellation).await
    }

    /// Compatibility helper for inward-facing tests of the former unary flow.
    ///
    /// # Errors
    ///
    /// Returns an application error only before a durable terminal turn exists.
    pub async fn execute(
        &self,
        input: ExecuteConversationTurn,
        idempotency_key: &str,
        cancellation: ConversationCancellationSignal,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let (started, cancellation) = self
            .prepare_start(input, idempotency_key, cancellation)
            .await?;
        match started.dispatch {
            Some(dispatch) => self.dispatch(dispatch, cancellation).await,
            None => Ok(started.snapshot),
        }
    }

    async fn preflight_model(
        &self,
        rail: grok_domain::ChatRail,
        selected_model: &str,
        require_canonical: bool,
        expected_credential_binding: Option<&str>,
        cancellation: ConversationCancellationSignal,
    ) -> Result<
        (
            Arc<dyn crate::ConversationModel>,
            String,
            String,
            ConversationCancellationSignal,
        ),
        ApplicationError,
    > {
        let (model, credential_binding_id, models, cancellation) = match rail {
            grok_domain::ChatRail::XaiApiKey => {
                let (credential, _credential_use) =
                    self.credentials.load_xai_api_credential_for_use().await?;
                let (secret, binding) = credential.into_parts();
                if expected_credential_binding.is_some_and(|expected| expected != binding) {
                    return Err(ApplicationError::InvalidState(
                        "the Chat credential changed after this conversation thread was bound; start a new thread to use the current credential"
                            .into(),
                    ));
                }
                let model = self.factory.create(secret).map_err(map_preflight_error)?;
                let (models, cancellation) = race_cancellation(model.list_models(), cancellation)
                    .await
                    .map_err(|()| ApplicationError::DeadlineExceeded)?;
                (model, binding, models, cancellation)
            }
            grok_domain::ChatRail::SuperGrokApi => {
                let (credential, _credential_use) =
                    self.load_supergrok_credential_for_use().await?;
                let binding = supergrok_binding(credential.generation);
                if expected_credential_binding.is_some_and(|expected| expected != binding) {
                    return Err(ApplicationError::InvalidState(
                        "the Chat credential changed after this conversation thread was bound; start a new thread to use the current credential"
                            .into(),
                    ));
                }
                let secret = crate::SecretValue::new(credential.access_token.as_bytes().to_vec())
                    .map_err(|_| {
                    ApplicationError::Integrity("OAuth credential is invalid".into())
                })?;
                let factory = self.supergrok_factory.as_ref().ok_or_else(|| {
                    ApplicationError::Unavailable("SuperGrok API Chat is not configured".into())
                })?;
                let model = factory.create(secret).map_err(map_preflight_error)?;
                let (models, cancellation) = race_cancellation(model.list_models(), cancellation)
                    .await
                    .map_err(|()| ApplicationError::DeadlineExceeded)?;
                (model, binding, models, cancellation)
            }
        };
        let canonical_model = ensure_selected_model(
            selected_model,
            models.map_err(map_preflight_error)?,
            require_canonical,
        )?;
        Ok((model, credential_binding_id, canonical_model, cancellation))
    }

    async fn load_supergrok_credential(
        &self,
    ) -> Result<crate::SuperGrokCredential, ApplicationError> {
        let service = self.supergrok.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("SuperGrok API Chat is not configured".into())
        })?;
        let now_ms = i64::try_from(self.clock.now()).unwrap_or(i64::MAX);
        service.credential(now_ms, 120_000).await
    }

    async fn load_supergrok_credential_for_use(
        &self,
    ) -> Result<
        (
            crate::SuperGrokCredential,
            tokio::sync::OwnedRwLockReadGuard<()>,
        ),
        ApplicationError,
    > {
        let service = self.supergrok.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("SuperGrok API Chat is not configured".into())
        })?;
        let now_ms = i64::try_from(self.clock.now()).unwrap_or(i64::MAX);
        service.credential_for_use(now_ms, 120_000).await
    }

    /// Converts at most `limit` crash-left reservations and in-flight calls into
    /// explicit non-replaying terminal states before the daemon accepts IPC.
    ///
    /// # Errors
    ///
    /// Returns a storage or lifecycle error if recovery cannot be committed.
    pub async fn recover_incomplete(
        &self,
        limit: usize,
    ) -> Result<ConversationRecoverySummary, ApplicationError> {
        if limit == 0 || limit > MAX_CONVERSATION_RECOVERY_BATCH {
            return Err(ApplicationError::InvalidInput(format!(
                "conversation recovery limit must be between 1 and {MAX_CONVERSATION_RECOVERY_BATCH}"
            )));
        }
        let query_limit = limit
            .checked_add(1)
            .ok_or_else(|| ApplicationError::InvalidInput("recovery limit overflow".into()))?;
        let turns = self
            .store
            .list_incomplete_turns_for_recovery(query_limit)
            .await?;
        let mut summary = ConversationRecoverySummary {
            truncated: turns.len() > limit,
            ..ConversationRecoverySummary::default()
        };
        for snapshot in turns.into_iter().take(limit) {
            match snapshot.turn.state {
                ConversationTurnState::Reserved => {
                    self.cancel_reserved(snapshot).await?;
                    summary.cancelled_reserved += 1;
                }
                ConversationTurnState::ProviderStarted => {
                    self.interrupt(snapshot).await?;
                    summary.interrupted_needs_review += 1;
                }
                state if state.is_terminal() => {
                    return Err(ApplicationError::Storage(
                        "conversation recovery store returned a terminal turn".into(),
                    ));
                }
                _ => {
                    return Err(ApplicationError::Storage(
                        "unsupported incomplete conversation state".into(),
                    ));
                }
            }
        }
        Ok(summary)
    }

    /// Lists a bounded chronological page of durable outcomes for one thread.
    ///
    /// # Errors
    ///
    /// Returns invalid input for an unsupported page size and propagates
    /// canonical workspace or storage failures.
    pub async fn list_for_thread(
        &self,
        thread_id: &ThreadId,
        after: Option<&ConversationTurnId>,
        limit: usize,
    ) -> Result<Page<ConversationTurnSnapshot>, ApplicationError> {
        if limit == 0 || limit > MAX_CONVERSATION_TURN_PAGE_SIZE {
            return Err(ApplicationError::InvalidInput(
                "conversation turn page size is invalid".into(),
            ));
        }
        self.workspace.get_thread(thread_id).await?;
        let materialized_limit = limit.min(MAX_CONVERSATION_TURN_MATERIALIZED_PAGE_SIZE);
        let mut turns = self
            .store
            .list_thread_turns(thread_id, after, materialized_limit.saturating_add(1))
            .await?;
        let next_cursor = (turns.len() > materialized_limit)
            .then(|| turns[materialized_limit - 1].turn.id.to_string());
        turns.truncate(materialized_limit);
        Ok(Page {
            items: turns,
            next_cursor,
        })
    }

    /// Lists a bounded reconnect page of durable normalized turn events.
    ///
    /// # Errors
    ///
    /// Returns invalid input for a zero or oversized batch and propagates
    /// canonical storage failures for missing or corrupt event streams.
    pub async fn events_since(
        &self,
        turn_id: &ConversationTurnId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<ConversationTurnEventPage, ApplicationError> {
        if !(1..=MAX_CONVERSATION_EVENT_BATCH).contains(&limit) {
            return Err(ApplicationError::InvalidInput(format!(
                "conversation event limit must be between 1 and {MAX_CONVERSATION_EVENT_BATCH}"
            )));
        }
        Ok(self
            .store
            .list_turn_events_since(turn_id, after_sequence, limit)
            .await?)
    }

    /// Returns whether one source is still the latest structurally retryable
    /// attempt. Lifecycle and account availability remain separate policy facts.
    ///
    /// # Errors
    ///
    /// Returns not-found or a validated storage failure.
    pub async fn retry_source_is_latest(
        &self,
        turn_id: &ConversationTurnId,
    ) -> Result<bool, ApplicationError> {
        Ok(self.store.retry_source_is_latest(turn_id).await?)
    }

    /// Returns whether the source's canonical thread and project still accept
    /// a new child attempt.
    ///
    /// # Errors
    ///
    /// Propagates canonical workspace lookup or ownership-integrity failures.
    pub async fn retry_source_is_writable(
        &self,
        snapshot: &ConversationTurnSnapshot,
    ) -> Result<bool, ApplicationError> {
        let thread = self.workspace.get_thread(&snapshot.turn.thread_id).await?;
        if thread.project_id != snapshot.turn.project_id {
            return Err(ApplicationError::Integrity(
                "conversation thread ownership changed".into(),
            ));
        }
        let project = self.workspace.get_project(&thread.project_id).await?;
        Ok(thread.state == ThreadState::Open && project.state == ProjectState::Active)
    }

    /// Checks whether the source turn and its thread remain bound to the
    /// currently loaded local xAI credential generation without exposing the
    /// binding over IPC.
    ///
    /// # Errors
    ///
    /// Propagates validated storage failures for a missing or corrupt thread
    /// identity row.
    pub async fn retry_source_account_available(
        &self,
        snapshot: &ConversationTurnSnapshot,
    ) -> Result<bool, ApplicationError> {
        let Some(source_binding) = snapshot.lineage.credential_binding_id.as_deref() else {
            return Ok(false);
        };
        let ConversationThreadCredentialBinding::Bound(thread_binding) = self
            .store
            .thread_credential_binding(&snapshot.turn.thread_id)
            .await?
        else {
            return Ok(false);
        };
        if thread_binding != source_binding {
            return Ok(false);
        }
        match snapshot.lineage.rail {
            grok_domain::ChatRail::XaiApiKey => Ok(self
                .credentials
                .current_xai_credential_binding_id()
                .is_ok_and(|current| current == source_binding)),
            grok_domain::ChatRail::SuperGrokApi => Ok(self
                .load_supergrok_credential()
                .await
                .is_ok_and(|current| supergrok_binding(current.generation) == source_binding)),
        }
    }

    /// Commits an exact durable cancellation classification before task abort.
    ///
    /// Reserved turns become cancelled without provider dispatch;
    /// provider-started turns become interrupted-needs-review; and a terminal
    /// state which won the observed-revision race is returned unchanged.
    ///
    /// # Errors
    ///
    /// Returns not-found, exact command conflict, stale nonterminal revision,
    /// or a durable storage/lifecycle failure.
    pub async fn cancel(
        &self,
        turn_id: &ConversationTurnId,
        expected_revision: u64,
        idempotency_key: &str,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let command = mutation_command(
            CONVERSATION_CANCEL_COMMAND_SCOPE,
            idempotency_key,
            &[turn_id.to_string(), expected_revision.to_string()],
        )?;
        let snapshot = self
            .store
            .load_turn(turn_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let terminal = self.build_cancellation_terminal(snapshot)?;
        Ok(self
            .store
            .commit_cancellation(CancelConversationTurnCommit {
                command,
                turn_id: turn_id.clone(),
                expected_turn_revision: expected_revision,
                terminal,
            })
            .await?)
    }

    /// Durably resolves a daemon task which exited without a terminal snapshot.
    ///
    /// A still-reserved turn is cancelled without provider dispatch. A turn
    /// whose provider boundary may have committed ambiguously is moved to
    /// interrupted-needs-review. Concurrent terminal completion wins exactly.
    /// The internal command key is stable per turn so daemon retries are exact.
    ///
    /// # Errors
    ///
    /// Returns not-found, a repeated optimistic conflict, or a storage/lifecycle
    /// error when the durable task-exit classification cannot be committed.
    pub async fn reconcile_dispatch_exit(
        &self,
        turn_id: &ConversationTurnId,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let idempotency_key = dispatch_exit_idempotency_key(turn_id);
        for _ in 0..4 {
            let snapshot = self
                .store
                .load_turn(turn_id)
                .await?
                .ok_or(ApplicationError::NotFound)?;
            if snapshot.turn.state.is_terminal() {
                return Ok(snapshot);
            }
            let expected_turn_revision = snapshot.turn.revision;
            let command = mutation_command(
                CONVERSATION_RECONCILIATION_COMMAND_SCOPE,
                &idempotency_key,
                &[turn_id.to_string(), expected_turn_revision.to_string()],
            )?;
            let terminal = self.build_cancellation_terminal(snapshot)?;
            match self
                .store
                .commit_dispatch_exit_reconciliation(CancelConversationTurnCommit {
                    command,
                    turn_id: turn_id.clone(),
                    expected_turn_revision,
                    terminal,
                })
                .await
            {
                Ok(terminal) => return Ok(terminal),
                Err(StoreError::Conflict) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(ApplicationError::Conflict)
    }

    fn build_cancellation_terminal(
        &self,
        snapshot: ConversationTurnSnapshot,
    ) -> Result<Option<TerminalTurnCommit>, ApplicationError> {
        match snapshot.turn.state {
            ConversationTurnState::Reserved => {
                Ok(Some(self.build_cancel_reserved_commit(snapshot)?))
            }
            ConversationTurnState::ProviderStarted => {
                Ok(Some(self.build_interrupt_commit(snapshot)?))
            }
            state if state.is_terminal() => Ok(None),
            _ => Err(ApplicationError::InvalidState(
                "conversation turn cannot be cancelled from this state".into(),
            )),
        }
    }

    async fn start_provider(
        &self,
        snapshot: ConversationTurnSnapshot,
        provider_fingerprint: [u8; 32],
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        if snapshot.turn.state != ConversationTurnState::Reserved
            || snapshot.run.state != RunState::Queued
            || snapshot.effect.is_some()
        {
            return Err(ApplicationError::InvalidState(
                "conversation reservation cannot start provider dispatch".into(),
            ));
        }
        let now = self.clock.now();
        let expected_turn_revision = snapshot.turn.revision;
        let expected_run_revision = snapshot.run.revision;
        let mut turn = snapshot.turn;
        let mut run = snapshot.run;
        let from_queued = run.state;
        run.transition(RunState::Planning, now)?;
        let planning_event = NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::StateChanged {
                from: from_queued,
                to: RunState::Planning,
            },
        };
        let from_planning = run.state;
        run.transition(RunState::Running, now)?;
        let running_event = NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::StateChanged {
                from: from_planning,
                to: RunState::Running,
            },
        };
        let mut effect = SideEffect::prepare(
            EffectId::new(self.ids.generate("effect"))?,
            run.id.clone(),
            EffectKind::ExternalMutation,
            format!("official xAI Responses API model {}", turn.model_id),
            Idempotency::NonIdempotent,
            now,
        );
        effect.start(now)?;
        turn.start_provider(effect.id.clone(), provider_fingerprint, now)?;
        let effect_event = NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::EffectPrepared {
                effect_id: effect.id.clone(),
            },
        };
        Ok(self
            .store
            .commit_provider_start(ProviderStartCommit {
                turn,
                expected_turn_revision,
                run,
                expected_run_revision,
                effect,
                events: vec![planning_event, running_event, effect_event],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::ProviderStarted,
                },
            })
            .await?)
    }

    #[allow(clippy::too_many_lines)]
    async fn consume_stream(
        &self,
        started: ConversationTurnSnapshot,
        mut stream: crate::ConversationStream,
        mut cancellation: ConversationCancellationSignal,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let mut output = String::new();
        let mut pending_text = String::new();
        let mut durable_text_offset = 0u64;
        let mut citations = Vec::new();
        let mut usage = None;
        let mut provider_response_id = None;
        let mut zero_data_retention = None;
        let mut completed = false;
        loop {
            let Ok((event, next_cancellation)) =
                race_cancellation(stream.next(), cancellation).await
            else {
                return self.interrupt_or_reconcile(started).await;
            };
            cancellation = next_cancellation;
            let Some(event) = event else { break };
            // Completion is a terminal provider event. Any later delta,
            // metadata, duplicate completion, or failure is contradictory and
            // cannot be classified as a known outcome.
            if completed {
                return self.interrupt_or_reconcile(started).await;
            }
            match event {
                Ok(ConversationEvent::Started { continuation }) => {
                    if continuation.is_some() {
                        provider_response_id = continuation;
                    }
                }
                Ok(ConversationEvent::TextDelta(delta)) => {
                    if output.len().saturating_add(delta.len()) > MAX_MESSAGE_BYTES {
                        return self.interrupt_or_reconcile(started).await;
                    }
                    output.push_str(&delta);
                    pending_text.push_str(&delta);
                    while pending_text.len() >= CONVERSATION_TEXT_COALESCE_BYTES {
                        let split =
                            utf8_prefix_length(&pending_text, CONVERSATION_TEXT_COALESCE_BYTES);
                        let remainder = pending_text.split_off(split);
                        let chunk = std::mem::replace(&mut pending_text, remainder);
                        cancellation = match self
                            .append_normalized_text_or_cancel(
                                &started,
                                durable_text_offset,
                                chunk,
                                cancellation,
                            )
                            .await
                        {
                            Ok(cancellation) => cancellation,
                            Err(()) => return self.interrupt_or_reconcile(started).await,
                        };
                        let Ok(split_length) = u64::try_from(split) else {
                            return self.interrupt_or_reconcile(started).await;
                        };
                        let Some(next_offset) = durable_text_offset.checked_add(split_length)
                        else {
                            return self.interrupt_or_reconcile(started).await;
                        };
                        durable_text_offset = next_offset;
                    }
                }
                Ok(ConversationEvent::Citation(citation)) => {
                    if !citations
                        .iter()
                        .any(|known: &Citation| known.url == citation.url)
                    {
                        if citations.len() >= 256 {
                            return self.interrupt_or_reconcile(started).await;
                        }
                        citations.push(citation);
                    }
                }
                Ok(ConversationEvent::Usage(value)) => usage = Some(value),
                Ok(ConversationEvent::RetentionObserved {
                    zero_data_retention: value,
                }) => zero_data_retention = Some(value),
                Ok(ConversationEvent::Completed { continuation }) => {
                    if continuation.is_some() {
                        provider_response_id = continuation;
                    }
                    completed = true;
                }
                Err(error) => return self.finish_provider_error(started, error).await,
            }
        }
        let Some(usage) = usage else {
            return self.interrupt_or_reconcile(started).await;
        };
        if !completed || output.is_empty() {
            return self.interrupt_or_reconcile(started).await;
        }
        if !pending_text.is_empty() {
            cancellation = match self
                .append_normalized_text_or_cancel(
                    &started,
                    durable_text_offset,
                    pending_text,
                    cancellation,
                )
                .await
            {
                Ok(cancellation) => cancellation,
                Err(()) => return self.interrupt_or_reconcile(started).await,
            };
        }
        let recovery = started.clone();
        match race_cancellation_first(
            Box::pin(self.complete(
                started,
                output,
                provider_response_id,
                citations,
                usage,
                zero_data_retention,
            )),
            cancellation,
        )
        .await
        {
            Ok((result, _)) => result,
            Err(()) => self.interrupt_or_reconcile(recovery).await,
        }
    }

    async fn append_normalized_text(
        &self,
        snapshot: &ConversationTurnSnapshot,
        start_utf8_offset: u64,
        text: String,
    ) -> Result<(), ApplicationError> {
        let expected_text = text.clone();
        let events = self
            .store
            .append_turn_text(
                &snapshot.turn.id,
                snapshot.turn.revision,
                start_utf8_offset,
                text,
            )
            .await?;
        validate_appended_text_events(
            &snapshot.turn.id,
            start_utf8_offset,
            &expected_text,
            &events,
        )?;
        Ok(())
    }

    async fn append_normalized_text_or_cancel(
        &self,
        snapshot: &ConversationTurnSnapshot,
        start_utf8_offset: u64,
        text: String,
        cancellation: ConversationCancellationSignal,
    ) -> Result<ConversationCancellationSignal, ()> {
        match race_cancellation_first(
            self.append_normalized_text(snapshot, start_utf8_offset, text),
            cancellation,
        )
        .await
        {
            Ok((Ok(()), cancellation)) => Ok(cancellation),
            Ok((Err(_), _)) | Err(()) => Err(()),
        }
    }

    async fn complete(
        &self,
        snapshot: ConversationTurnSnapshot,
        output: String,
        provider_response_id: Option<String>,
        citations: Vec<Citation>,
        usage: Usage,
        zero_data_retention: Option<bool>,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let recovery_snapshot = snapshot.clone();
        // Provider-derived text, citations, continuation IDs, and usage are
        // untrusted until every canonical domain object has accepted them. If
        // canonicalization fails after dispatch, the outcome is uncertain and
        // must be durably reviewed rather than leaving an active turn behind.
        let Ok(commit) = self.build_completed_commit(
            snapshot,
            output,
            provider_response_id,
            citations,
            usage,
            zero_data_retention,
        ) else {
            return self.interrupt_or_reconcile(recovery_snapshot).await;
        };
        match self.store.commit_terminal(commit).await {
            Ok(committed) => Ok(committed),
            // Terminal persistence is atomic at the store boundary. A rejected
            // provider-derived representation therefore leaves the dispatch
            // active, so make one exact optimistic transition to review. If a
            // concurrent terminal commit won, its revision makes this fallback
            // conflict rather than overwrite the winner.
            Err(_) => self.interrupt_or_reconcile(recovery_snapshot).await,
        }
    }

    fn build_completed_commit(
        &self,
        snapshot: ConversationTurnSnapshot,
        output: String,
        provider_response_id: Option<String>,
        citations: Vec<Citation>,
        usage: Usage,
        zero_data_retention: Option<bool>,
    ) -> Result<TerminalTurnCommit, ApplicationError> {
        let now = self.terminal_timestamp(&snapshot);
        let mut turn = snapshot.turn;
        let mut run = snapshot.run;
        let mut effect = required_executing_effect(snapshot.effect)?;
        let expected_turn_revision = turn.revision;
        let expected_run_revision = run.revision;
        let expected_effect_revision = effect.revision;
        let assistant = Message::new(
            MessageId::new(self.ids.generate("message"))?,
            turn.thread_id.clone(),
            MessageRole::Assistant,
            output,
            now,
        )?;
        turn.complete(
            assistant.id.clone(),
            provider_response_id,
            citations
                .into_iter()
                .map(|citation| ConversationCitation {
                    title: citation.title,
                    url: citation.url,
                })
                .collect(),
            ConversationUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cost_in_usd_ticks: usage.cost_in_usd_ticks,
            },
            zero_data_retention,
            now,
        )?;
        effect.finish(true, now)?;
        let from = run.state;
        run.transition(RunState::Completed, now)?;
        Ok(TerminalTurnCommit {
            turn,
            expected_turn_revision,
            run,
            expected_run_revision,
            effect: Some(effect),
            expected_effect_revision: Some(expected_effect_revision),
            assistant_message: Some(assistant),
            events: vec![NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::StateChanged {
                    from,
                    to: RunState::Completed,
                },
            }],
            turn_event: ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::ProviderStarted,
                to: ConversationTurnState::Completed,
            },
        })
    }

    async fn finish_provider_error(
        &self,
        snapshot: ConversationTurnSnapshot,
        error: ModelError,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        if error.certainty == ModelFailureCertainty::OutcomeUnknown {
            return self.interrupt_or_reconcile(snapshot).await;
        }
        let recovery_snapshot = snapshot.clone();
        // A malformed provider failure cannot be persisted as a known outcome.
        // Treat it as uncertain without retaining its untrusted explanation.
        let Ok(commit) = self.build_failed_commit(snapshot, error) else {
            return self.interrupt_or_reconcile(recovery_snapshot).await;
        };
        match self.store.commit_terminal(commit).await {
            Ok(committed) => Ok(committed),
            Err(_) => self.interrupt_or_reconcile(recovery_snapshot).await,
        }
    }

    fn build_failed_commit(
        &self,
        snapshot: ConversationTurnSnapshot,
        error: ModelError,
    ) -> Result<TerminalTurnCommit, ApplicationError> {
        let now = self.terminal_timestamp(&snapshot);
        let mut turn = snapshot.turn;
        let mut run = snapshot.run;
        let mut effect = required_executing_effect(snapshot.effect)?;
        let expected_turn_revision = turn.revision;
        let expected_run_revision = run.revision;
        let expected_effect_revision = effect.revision;
        turn.fail(
            ConversationFailure {
                kind: failure_kind(error.kind),
                message: error.message,
                retryable: error.retryable,
            },
            now,
        )?;
        effect.finish(false, now)?;
        let from = run.state;
        run.transition(RunState::Failed, now)?;
        Ok(TerminalTurnCommit {
            turn,
            expected_turn_revision,
            run,
            expected_run_revision,
            effect: Some(effect),
            expected_effect_revision: Some(expected_effect_revision),
            assistant_message: None,
            events: vec![NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::StateChanged {
                    from,
                    to: RunState::Failed,
                },
            }],
            turn_event: ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::ProviderStarted,
                to: ConversationTurnState::Failed,
            },
        })
    }

    async fn interrupt(
        &self,
        snapshot: ConversationTurnSnapshot,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let commit = self.build_interrupt_commit(snapshot)?;
        Ok(self.store.commit_terminal(commit).await?)
    }

    fn build_interrupt_commit(
        &self,
        snapshot: ConversationTurnSnapshot,
    ) -> Result<TerminalTurnCommit, ApplicationError> {
        let now = self.terminal_timestamp(&snapshot);
        let mut turn = snapshot.turn;
        let mut run = snapshot.run;
        let mut effect = required_executing_effect(snapshot.effect)?;
        let expected_turn_revision = turn.revision;
        let expected_run_revision = run.revision;
        let expected_effect_revision = effect.revision;
        turn.interrupt(now)?;
        effect.interrupt(now)?;
        let from = run.state;
        run.transition(RunState::InterruptedNeedsReview, now)?;
        Ok(TerminalTurnCommit {
            turn,
            expected_turn_revision,
            run,
            expected_run_revision,
            effect: Some(effect.clone()),
            expected_effect_revision: Some(expected_effect_revision),
            assistant_message: None,
            events: vec![
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::EffectNeedsReview {
                        effect_id: effect.id,
                    },
                },
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::StateChanged {
                        from,
                        to: RunState::InterruptedNeedsReview,
                    },
                },
            ],
            turn_event: ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::ProviderStarted,
                to: ConversationTurnState::InterruptedNeedsReview,
            },
        })
    }

    async fn cancel_reserved(
        &self,
        snapshot: ConversationTurnSnapshot,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let commit = self.build_cancel_reserved_commit(snapshot)?;
        Ok(self.store.commit_terminal(commit).await?)
    }

    fn build_cancel_reserved_commit(
        &self,
        snapshot: ConversationTurnSnapshot,
    ) -> Result<TerminalTurnCommit, ApplicationError> {
        let now = self.terminal_timestamp(&snapshot);
        let mut turn = snapshot.turn;
        let mut run = snapshot.run;
        let expected_turn_revision = turn.revision;
        let expected_run_revision = run.revision;
        turn.cancel(now)?;
        let from = run.state;
        run.transition(RunState::Cancelled, now)?;
        Ok(TerminalTurnCommit {
            turn,
            expected_turn_revision,
            run,
            expected_run_revision,
            effect: None,
            expected_effect_revision: None,
            assistant_message: None,
            events: vec![NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::StateChanged {
                    from,
                    to: RunState::Cancelled,
                },
            }],
            turn_event: ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::Reserved,
                to: ConversationTurnState::Cancelled,
            },
        })
    }

    async fn interrupt_or_reconcile(
        &self,
        snapshot: ConversationTurnSnapshot,
    ) -> Result<ConversationTurnSnapshot, ApplicationError> {
        let command_scope = match &snapshot.lineage.origin {
            ConversationTurnOrigin::Original => CONVERSATION_COMMAND_SCOPE,
            ConversationTurnOrigin::Retry { .. } => CONVERSATION_RETRY_COMMAND_SCOPE,
            ConversationTurnOrigin::EditAndBranch { .. } => CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE,
            ConversationTurnOrigin::Regenerate { .. } => CONVERSATION_REGENERATE_COMMAND_SCOPE,
        };
        let command = MutationCommand {
            scope: command_scope.into(),
            key: snapshot.turn.idempotency_key.clone(),
            fingerprint: snapshot.turn.request_fingerprint,
        };
        match self.interrupt(snapshot).await {
            Ok(interrupted) => Ok(interrupted),
            Err(interrupt_error) => {
                if let Some(current) = self.store.load_turn_by_command(&command).await?
                    && current.turn.state.is_terminal()
                {
                    return Ok(current);
                }
                Err(interrupt_error)
            }
        }
    }

    fn terminal_timestamp(&self, snapshot: &ConversationTurnSnapshot) -> UnixMillis {
        snapshot.effect.as_ref().map_or_else(
            || {
                self.clock
                    .now()
                    .max(snapshot.turn.updated_at)
                    .max(snapshot.run.updated_at)
            },
            |effect| {
                self.clock
                    .now()
                    .max(snapshot.turn.updated_at)
                    .max(snapshot.run.updated_at)
                    .max(effect.updated_at)
            },
        )
    }
}

fn ensure_fork_source_eligible(
    kind: ConversationForkKind,
    source: &ConversationTurnSnapshot,
) -> Result<(), ApplicationError> {
    let eligible = match kind {
        ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
            source.turn.state == ConversationTurnState::Completed
                && source.assistant_message.is_some()
        }
        ConversationForkKind::EditAndBranch => matches!(
            source.turn.state,
            ConversationTurnState::Completed
                | ConversationTurnState::Cancelled
                | ConversationTurnState::Failed
        ),
    };
    if !eligible {
        let reason = match kind {
            ConversationForkKind::Branch => {
                "only a completed conversation response may be branched"
            }
            ConversationForkKind::EditAndBranch => {
                "only a completed, cancelled, or known failed turn may be edited into a branch"
            }
            ConversationForkKind::Regenerate => {
                "only a completed conversation response may be regenerated"
            }
        };
        return Err(ApplicationError::InvalidState(reason.into()));
    }
    Ok(())
}

fn validate_frozen_source_context(
    source: &ConversationTurnSnapshot,
    source_context: &[Message],
) -> Result<[u8; 32], ApplicationError> {
    if source_context.last() != Some(&source.user_message) {
        return Err(ApplicationError::Integrity(
            "fork source context does not end at its canonical user message".into(),
        ));
    }
    let request = provider_request(&source.turn.model_id, source_context)?;
    let fingerprint = provider_request_fingerprint(&request);
    match source.turn.state {
        ConversationTurnState::Completed | ConversationTurnState::Failed
            if source.turn.provider_request_fingerprint != Some(fingerprint) =>
        {
            return Err(ApplicationError::Integrity(
                "fork source context does not match its provider request".into(),
            ));
        }
        ConversationTurnState::Cancelled if source.turn.provider_request_fingerprint.is_some() => {
            return Err(ApplicationError::Integrity(
                "cancelled fork source contains impossible provider evidence".into(),
            ));
        }
        ConversationTurnState::Reserved
        | ConversationTurnState::ProviderStarted
        | ConversationTurnState::InterruptedNeedsReview
        | ConversationTurnState::Completed
        | ConversationTurnState::Failed
        | ConversationTurnState::Cancelled => {}
    }
    Ok(fingerprint)
}

fn fork_command(
    request: &ConversationForkRequest,
    idempotency_key: &str,
    source: &ConversationTurnSnapshot,
    source_context: &[Message],
) -> Result<MutationCommand, ApplicationError> {
    let source_context_fingerprint = validate_frozen_source_context(source, source_context)?;
    let (scope, kind) = match request.kind() {
        ConversationForkKind::Branch => (CONVERSATION_BRANCH_COMMAND_SCOPE, "branch"),
        ConversationForkKind::EditAndBranch => {
            (CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE, "edit_and_branch")
        }
        ConversationForkKind::Regenerate => (CONVERSATION_REGENERATE_COMMAND_SCOPE, "regenerate"),
    };
    let state = match source.turn.state {
        ConversationTurnState::Reserved => "reserved",
        ConversationTurnState::ProviderStarted => "provider_started",
        ConversationTurnState::Completed => "completed",
        ConversationTurnState::Failed => "failed",
        ConversationTurnState::Cancelled => "cancelled",
        ConversationTurnState::InterruptedNeedsReview => "interrupted_needs_review",
    };
    let mut command = mutation_command(
        scope,
        idempotency_key,
        &[
            kind.into(),
            request.source_turn_id().to_owned(),
            request.expected_revision().to_string(),
            state.into(),
            encode_digest(&source.turn.request_fingerprint),
            encode_digest(&source_context_fingerprint),
            source.turn.model_id.clone(),
            source
                .lineage
                .credential_binding_id
                .clone()
                .unwrap_or_else(|| "legacy-unbound".into()),
            source
                .assistant_message
                .as_ref()
                .map_or_else(|| "no-assistant".into(), |message| message.id.to_string()),
            request.edited_content().unwrap_or_default().to_owned(),
        ],
    )?;
    command.key = scoped_conversation_command_key(scope, idempotency_key);
    Ok(command)
}

fn retry_command(
    input: &RetryConversationTurn,
    idempotency_key: &str,
    source: &ConversationTurnSnapshot,
    source_context: &[Message],
) -> Result<MutationCommand, ApplicationError> {
    let source_provider_request = provider_request(&source.turn.model_id, source_context)?;
    let source_provider_fingerprint = provider_request_fingerprint(&source_provider_request);
    if source.turn.state == ConversationTurnState::Failed
        && source.turn.provider_request_fingerprint != Some(source_provider_fingerprint)
    {
        return Err(ApplicationError::Integrity(
            "retry source context does not match its provider request".into(),
        ));
    }
    let mut command = mutation_command(
        CONVERSATION_RETRY_COMMAND_SCOPE,
        idempotency_key,
        &[
            input.source_turn_id.clone(),
            input.expected_revision.to_string(),
            encode_digest(&source.turn.request_fingerprint),
            source.turn.model_id.clone(),
            source
                .lineage
                .credential_binding_id
                .clone()
                .unwrap_or_else(|| "legacy-unbound".into()),
            encode_digest(&source_provider_fingerprint),
        ],
    )?;
    command.key =
        scoped_conversation_command_key(CONVERSATION_RETRY_COMMAND_SCOPE, idempotency_key);
    Ok(command)
}

fn encode_digest(value: &[u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in value {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn scoped_conversation_command_key(scope: &str, idempotency_key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(scope.as_bytes());
    digest.update([0]);
    digest.update(idempotency_key.as_bytes());
    let prefix = if scope == CONVERSATION_RETRY_COMMAND_SCOPE {
        "retry-"
    } else {
        "fork-"
    };
    let mut key = String::with_capacity(prefix.len() + 64);
    key.push_str(prefix);
    for byte in digest.finalize() {
        write!(&mut key, "{byte:02x}").expect("writing to a String cannot fail");
    }
    key
}

fn ensure_retry_source_eligible(
    snapshot: &ConversationTurnSnapshot,
) -> Result<(), ApplicationError> {
    let eligible = match snapshot.turn.state {
        ConversationTurnState::Cancelled => true,
        ConversationTurnState::Failed => snapshot
            .turn
            .failure
            .as_ref()
            .is_some_and(|failure| failure.retryable),
        _ => false,
    };
    if !eligible {
        return Err(ApplicationError::InvalidState(
            "only a cancelled or retryable failed conversation turn may be retried".into(),
        ));
    }
    Ok(())
}

fn provider_request(
    model: &str,
    context: &[Message],
) -> Result<ConversationRequest, ApplicationError> {
    if context.is_empty() || context.len() > MAX_CONVERSATION_CONTEXT_MESSAGES {
        return Err(ApplicationError::InvalidInput(
            "conversation context exceeds the supported message limit".into(),
        ));
    }
    let bytes = context
        .iter()
        .try_fold(0usize, |total, message| {
            total.checked_add(message.content.len())
        })
        .ok_or_else(|| {
            ApplicationError::InvalidInput("conversation context is too large".into())
        })?;
    if bytes > MAX_CONVERSATION_CONTEXT_BYTES {
        return Err(ApplicationError::InvalidInput(
            "conversation context exceeds the supported byte limit".into(),
        ));
    }
    let messages = context
        .iter()
        .filter(|message| message.state == MessageState::Active)
        .map(|message| ConversationMessage {
            role: match message.role {
                MessageRole::System => ConversationRole::System,
                MessageRole::User => ConversationRole::User,
                MessageRole::Assistant => ConversationRole::Assistant,
            },
            content: vec![ContentPart::Text(message.content.clone())],
        })
        .collect();
    Ok(ConversationRequest {
        model: model.into(),
        messages,
        continuation: None,
        tools: Vec::new(),
        store: false,
    })
}

fn utf8_prefix_length(value: &str, maximum: usize) -> usize {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn validate_appended_text_events(
    turn_id: &ConversationTurnId,
    start_utf8_offset: u64,
    expected_text: &str,
    events: &[ConversationTurnEvent],
) -> Result<(), ApplicationError> {
    if expected_text.is_empty() || events.is_empty() {
        return Err(ApplicationError::Storage(
            "conversation text append returned an empty event batch".into(),
        ));
    }
    let mut expected_offset = start_utf8_offset;
    let mut previous_sequence = None;
    let mut actual_text = String::with_capacity(expected_text.len());
    for event in events {
        let event = ConversationTurnEvent::restore(event.clone()).map_err(|error| {
            ApplicationError::Storage(format!(
                "conversation text append returned an invalid event: {error}"
            ))
        })?;
        if &event.turn_id != turn_id
            || previous_sequence
                .is_some_and(|previous: u64| previous.checked_add(1) != Some(event.sequence))
        {
            return Err(ApplicationError::Storage(
                "conversation text append returned a noncontiguous event batch".into(),
            ));
        }
        let ConversationTurnEventKind::TextAppended {
            start_utf8_offset,
            text,
        } = &event.kind
        else {
            return Err(ApplicationError::Storage(
                "conversation text append returned a lifecycle event".into(),
            ));
        };
        if *start_utf8_offset != expected_offset {
            return Err(ApplicationError::Storage(
                "conversation text append returned an invalid UTF-8 offset".into(),
            ));
        }
        expected_offset = expected_offset
            .checked_add(u64::try_from(text.len()).map_err(|_| {
                ApplicationError::Storage("conversation text append length overflow".into())
            })?)
            .ok_or_else(|| {
                ApplicationError::Storage("conversation text append offset overflow".into())
            })?;
        actual_text.push_str(text);
        previous_sequence = Some(event.sequence);
    }
    if actual_text != expected_text {
        return Err(ApplicationError::Storage(
            "conversation text append did not persist the exact normalized text".into(),
        ));
    }
    Ok(())
}

fn provider_request_fingerprint(request: &ConversationRequest) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hash_part(&mut hasher, request.model.as_bytes());
    hash_part(&mut hasher, &[u8::from(request.store)]);
    for message in &request.messages {
        let role = match message.role {
            ConversationRole::System => b"system".as_slice(),
            ConversationRole::User => b"user".as_slice(),
            ConversationRole::Assistant => b"assistant".as_slice(),
        };
        hash_part(&mut hasher, role);
        for part in &message.content {
            match part {
                ContentPart::Text(value) => {
                    hash_part(&mut hasher, b"text");
                    hash_part(&mut hasher, value.as_bytes());
                }
                ContentPart::FileReference(value) => {
                    hash_part(&mut hasher, b"file");
                    hash_part(&mut hasher, value.as_bytes());
                }
                ContentPart::ImageUrl(value) => {
                    hash_part(&mut hasher, b"image");
                    hash_part(&mut hasher, value.as_bytes());
                }
            }
        }
    }
    hasher.finalize().into()
}

fn dispatch_exit_idempotency_key(turn_id: &ConversationTurnId) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(turn_id.as_str().as_bytes());
    let mut key = String::with_capacity("daemon-dispatch-exit-".len() + digest.len() * 2);
    key.push_str("daemon-dispatch-exit-");
    for byte in digest {
        key.push(char::from(HEX[usize::from(byte >> 4)]));
        key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    key
}

fn hash_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

async fn race_cancellation<F, T>(
    future: F,
    cancellation: ConversationCancellationSignal,
) -> Result<(T, ConversationCancellationSignal), ()>
where
    F: Future<Output = T> + Send,
{
    match futures_util::future::select(Box::pin(future), cancellation).await {
        Either::Left((value, cancellation)) => Ok((value, cancellation)),
        Either::Right(((), _unfinished)) => Err(()),
    }
}

async fn race_cancellation_first<F, T>(
    future: F,
    cancellation: ConversationCancellationSignal,
) -> Result<(T, ConversationCancellationSignal), ()>
where
    F: Future<Output = T> + Send,
{
    match futures_util::future::select(cancellation, Box::pin(future)).await {
        Either::Left(((), _unfinished)) => Err(()),
        Either::Right((value, cancellation)) => Ok((value, cancellation)),
    }
}

fn required_executing_effect(effect: Option<SideEffect>) -> Result<SideEffect, ApplicationError> {
    effect
        .filter(|effect| effect.state == EffectState::Executing)
        .ok_or_else(|| {
            ApplicationError::Storage(
                "provider-started conversation is missing its executing effect".into(),
            )
        })
}

fn ensure_selected_model(
    selected: &str,
    models: Vec<crate::ModelDescriptor>,
    require_canonical: bool,
) -> Result<String, ApplicationError> {
    let catalog = crate::chat_models::validate_catalog(models)?;
    let canonical = crate::chat_models::canonical_text_model_id(selected, &catalog)?;
    if require_canonical && canonical != selected {
        return Err(ApplicationError::Unavailable(
            "the persisted xAI model selection is not canonical".into(),
        ));
    }
    Ok(canonical)
}

fn supergrok_binding(generation: u64) -> String {
    format!("supergrok-api:{generation}")
}

fn parse_supergrok_binding(binding: &str) -> Result<u64, ApplicationError> {
    binding
        .strip_prefix("supergrok-api:")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ApplicationError::Integrity("SuperGrok binding is invalid".into()))
}

fn map_preflight_error(error: ModelError) -> ApplicationError {
    match error.kind {
        ModelErrorKind::Authentication => ApplicationError::Unauthorized(error.message),
        ModelErrorKind::Forbidden => ApplicationError::Unavailable(
            "the configured xAI key cannot resolve conversation capabilities".into(),
        ),
        ModelErrorKind::InvalidRequest => ApplicationError::InvalidInput(error.message),
        ModelErrorKind::RateLimited | ModelErrorKind::Unavailable | ModelErrorKind::Protocol => {
            ApplicationError::Unavailable(error.message)
        }
    }
}

const fn failure_kind(kind: ModelErrorKind) -> ConversationFailureKind {
    match kind {
        ModelErrorKind::Authentication => ConversationFailureKind::Authentication,
        ModelErrorKind::Forbidden => ConversationFailureKind::Forbidden,
        ModelErrorKind::InvalidRequest => ConversationFailureKind::InvalidRequest,
        ModelErrorKind::RateLimited => ConversationFailureKind::RateLimited,
        ModelErrorKind::Unavailable => ConversationFailureKind::Unavailable,
        ModelErrorKind::Protocol => ConversationFailureKind::Protocol,
    }
}
