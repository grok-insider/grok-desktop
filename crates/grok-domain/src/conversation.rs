use thiserror::Error;

use crate::{
    ConversationTurnId, EffectId, MAX_MESSAGE_BYTES, MessageId, ProjectId, RunId, ThreadId,
    UnixMillis,
};

const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MAX_CREDENTIAL_BINDING_ID_BYTES: usize = 128;
const MAX_MODEL_ID_BYTES: usize = 512;
const MAX_PROVIDER_RESPONSE_ID_BYTES: usize = 512;
const MAX_FAILURE_MESSAGE_BYTES: usize = 512;
const MAX_CITATIONS: usize = 256;
const MAX_CITATION_TITLE_BYTES: usize = 500;
const MAX_CITATION_URL_BYTES: usize = 8192;
// A one-megabyte raw bound remains below the SQL JSON column limit even when
// every quote or backslash requires JSON escaping plus per-record structure.
/// Maximum aggregate UTF-8 bytes retained across one response's citations.
pub const MAX_CONVERSATION_CITATION_TOTAL_BYTES: usize = 1_000_000;
/// Largest usage counter that remains exact in every supported IPC consumer.
pub const MAX_CONVERSATION_USAGE_VALUE: u64 = 9_007_199_254_740_991;
/// Maximum UTF-8 bytes stored in one normalized conversation text event.
pub const MAX_CONVERSATION_TEXT_CHUNK_BYTES: usize = 16 * 1024;
/// Maximum normalized text events retained for one conversation response.
///
/// The application coalesces provider deltas up to 16 KiB. This wider durable
/// bound preserves compatibility with older event histories while rejecting
/// corrupted one-byte event floods.
pub const MAX_CONVERSATION_TEXT_EVENTS: usize = 4_097;

/// Immutable reason one direct conversation turn exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationTurnOrigin {
    /// A user submitted a new prompt through the ordinary start command.
    Original,
    /// A new attempt was explicitly requested from a known-safe terminal turn.
    Retry {
        /// Preserved source attempt. The source is never mutated or replayed.
        source_turn_id: ConversationTurnId,
    },
    /// A new prompt was reserved in an explicit Edit-and-branch child thread.
    EditAndBranch {
        /// Parent attempt whose final user content was explicitly replaced.
        source_turn_id: ConversationTurnId,
    },
    /// Another billable response was requested in a Regenerate child thread.
    Regenerate {
        /// Completed parent attempt whose exact prompt and context were reused.
        source_turn_id: ConversationTurnId,
    },
}

/// Durable non-secret lineage and local credential-generation binding.
///
/// The binding is an opaque local generation identifier. It is deliberately
/// neither an xAI account identifier nor a digest of credential bytes. Legacy
/// original turns may be unbound, but they are never eligible as retry sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurnLineage {
    /// Immutable origin classification.
    pub origin: ConversationTurnOrigin,
    /// Opaque local credential generation used for provider preflight.
    pub credential_binding_id: Option<String>,
    /// Bounded retry-chain depth. Originals are zero.
    pub retry_depth: u8,
}

impl ConversationTurnLineage {
    /// Creates lineage for a newly submitted prompt.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for an invalid local binding ID.
    pub fn original(credential_binding_id: String) -> Result<Self, ConversationTurnError> {
        validate_credential_binding_id(&credential_binding_id)?;
        Ok(Self {
            origin: ConversationTurnOrigin::Original,
            credential_binding_id: Some(credential_binding_id),
            retry_depth: 0,
        })
    }

    /// Creates lineage for an explicit safe retry.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for an invalid local binding ID.
    pub fn retry(
        source_turn_id: ConversationTurnId,
        credential_binding_id: String,
        source_retry_depth: u8,
    ) -> Result<Self, ConversationTurnError> {
        validate_credential_binding_id(&credential_binding_id)?;
        let retry_depth = source_retry_depth
            .checked_add(1)
            .filter(|depth| *depth <= 64)
            .ok_or(ConversationTurnError::InvalidField("retry_depth"))?;
        Ok(Self {
            origin: ConversationTurnOrigin::Retry { source_turn_id },
            credential_binding_id: Some(credential_binding_id),
            retry_depth,
        })
    }

    /// Creates lineage for a new Edit-and-branch provider attempt.
    ///
    /// This is a fresh billable attempt rather than a Retry, so its retry depth
    /// begins at zero. A later safe Retry may reference it normally.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for an invalid local binding ID.
    pub fn edit_and_branch(
        source_turn_id: ConversationTurnId,
        credential_binding_id: String,
    ) -> Result<Self, ConversationTurnError> {
        validate_credential_binding_id(&credential_binding_id)?;
        Ok(Self {
            origin: ConversationTurnOrigin::EditAndBranch { source_turn_id },
            credential_binding_id: Some(credential_binding_id),
            retry_depth: 0,
        })
    }

    /// Creates lineage for a new Regenerate provider attempt.
    ///
    /// This is a fresh billable attempt rather than a Retry, so its retry depth
    /// begins at zero. A later safe Retry may reference it normally.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for an invalid local binding ID.
    pub fn regenerate(
        source_turn_id: ConversationTurnId,
        credential_binding_id: String,
    ) -> Result<Self, ConversationTurnError> {
        validate_credential_binding_id(&credential_binding_id)?;
        Ok(Self {
            origin: ConversationTurnOrigin::Regenerate { source_turn_id },
            credential_binding_id: Some(credential_binding_id),
            retry_depth: 0,
        })
    }

    /// Rehydrates lineage after validating its owner and bounded metadata.
    ///
    /// Legacy unbound originals remain readable. A retry must always be bound
    /// and must reference a different durable turn.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for an impossible lineage shape.
    pub fn restore(
        lineage: Self,
        owner_turn_id: &ConversationTurnId,
    ) -> Result<Self, ConversationTurnError> {
        if let Some(binding) = lineage.credential_binding_id.as_deref() {
            validate_credential_binding_id(binding)?;
        }
        match &lineage.origin {
            ConversationTurnOrigin::Original if lineage.retry_depth == 0 => {}
            ConversationTurnOrigin::Retry { source_turn_id }
                if source_turn_id != owner_turn_id
                    && lineage.credential_binding_id.is_some()
                    && (1..=64).contains(&lineage.retry_depth) => {}
            ConversationTurnOrigin::EditAndBranch { source_turn_id }
            | ConversationTurnOrigin::Regenerate { source_turn_id }
                if source_turn_id != owner_turn_id
                    && lineage.credential_binding_id.is_some()
                    && lineage.retry_depth == 0 => {}
            ConversationTurnOrigin::Original
            | ConversationTurnOrigin::Retry { .. }
            | ConversationTurnOrigin::EditAndBranch { .. }
            | ConversationTurnOrigin::Regenerate { .. } => {
                return Err(ConversationTurnError::InvalidField("lineage"));
            }
        }
        Ok(lineage)
    }
}

/// Durable lifecycle of one direct xAI conversation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationTurnState {
    /// The local user message, run, and immutable provider input were committed.
    Reserved,
    /// A non-idempotent provider request may have crossed the network boundary.
    ProviderStarted,
    /// The provider completed and the assistant message was committed atomically.
    Completed,
    /// The provider returned a known terminal failure.
    Failed,
    /// The request was cancelled before provider dispatch.
    Cancelled,
    /// Provider completion is uncertain and must never be replayed automatically.
    InterruptedNeedsReview,
}

impl ConversationTurnState {
    /// Returns whether the turn must not perform any further provider call.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::InterruptedNeedsReview
        )
    }

    const fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Reserved, Self::ProviderStarted | Self::Cancelled)
                | (
                    Self::ProviderStarted,
                    Self::Completed | Self::Failed | Self::InterruptedNeedsReview
                )
        )
    }
}

/// Append-only, turn-local event clients can replay after reconnecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurnEvent {
    /// Sequence scoped to one turn, beginning at one without gaps.
    pub sequence: u64,
    /// Owning durable conversation turn.
    pub turn_id: ConversationTurnId,
    /// Canonical normalized event payload.
    pub kind: ConversationTurnEventKind,
}

impl ConversationTurnEvent {
    /// Rehydrates one event after validating its self-contained bounds.
    ///
    /// Stream ordering, lifecycle continuity, and aggregate text offsets are
    /// validated by [`ConversationTurnEventLog::restore`].
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnEventError`] for a zero sequence, illegal
    /// lifecycle edge, or malformed text append.
    pub fn restore(event: Self) -> Result<Self, ConversationTurnEventError> {
        if event.sequence == 0 {
            return Err(ConversationTurnEventError::InvalidSequence);
        }
        validate_turn_event_kind(&event.kind)?;
        Ok(event)
    }
}

/// Durable normalized event kinds for one direct conversation turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationTurnEventKind {
    /// The turn reservation and immutable provider input were committed.
    Created,
    /// The durable turn lifecycle moved along one legal edge.
    StateChanged {
        /// State before the atomic transition.
        from: ConversationTurnState,
        /// State after the atomic transition.
        to: ConversationTurnState,
    },
    /// One normalized assistant-text chunk was durably appended.
    TextAppended {
        /// Exact UTF-8 byte offset of this chunk in the accumulated response.
        start_utf8_offset: u64,
        /// Nonempty normalized provider text, never a raw transport frame.
        text: String,
    },
}

/// Invalid durable event shape or turn-local event history.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConversationTurnEventError {
    /// Event sequences begin at one and must advance without overflow.
    #[error("invalid conversation turn event sequence")]
    InvalidSequence,
    /// The event belongs to a different turn than the enclosing stream.
    #[error("conversation turn event owner does not match the stream")]
    WrongTurn,
    /// The event is not legal after the current stream projection.
    #[error("invalid conversation turn event order")]
    InvalidOrder,
    /// A lifecycle event contains an unsupported state edge.
    #[error("invalid conversation turn event transition from {from:?} to {to:?}")]
    InvalidTransition {
        /// Existing lifecycle state.
        from: ConversationTurnState,
        /// Requested lifecycle state.
        to: ConversationTurnState,
    },
    /// A text chunk is empty, oversized, or contains unsupported controls.
    #[error("invalid conversation turn event text")]
    InvalidText,
    /// A text append did not begin at the exact accumulated UTF-8 byte offset.
    #[error("invalid conversation turn event UTF-8 offset")]
    InvalidTextOffset,
    /// Accumulated text would exceed the canonical message limit.
    #[error("conversation turn event text exceeds the message limit")]
    TextLimitExceeded,
    /// A corrupted or adversarial stream contains too many text events.
    #[error("conversation turn event stream contains too many text events")]
    EventLimitExceeded,
    /// The event stream does not represent the linked durable turn snapshot.
    #[error("conversation turn event stream does not match the turn snapshot")]
    SnapshotMismatch,
}

/// Validated projection of one complete, contiguous turn event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurnEventLog {
    turn_id: ConversationTurnId,
    state: Option<ConversationTurnState>,
    text: String,
    text_events: usize,
    last_sequence: u64,
}

impl ConversationTurnEventLog {
    /// Creates an empty projection which accepts only a sequence-one `Created` event.
    #[must_use]
    pub const fn new(turn_id: ConversationTurnId) -> Self {
        Self {
            turn_id,
            state: None,
            text: String::new(),
            text_events: 0,
            last_sequence: 0,
        }
    }

    /// Rehydrates and validates a complete turn-local event stream.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnEventError`] unless the stream starts with
    /// `Created`, has contiguous sequences, legal lifecycle edges, and exact
    /// UTF-8 append offsets within the canonical message bound.
    pub fn restore(
        turn_id: ConversationTurnId,
        events: &[ConversationTurnEvent],
    ) -> Result<Self, ConversationTurnEventError> {
        let mut log = Self::new(turn_id);
        for event in events {
            log.append_event(event.clone())?;
        }
        if log.state.is_none() {
            return Err(ConversationTurnEventError::InvalidOrder);
        }
        Ok(log)
    }

    /// Creates and applies the next canonical event for this projection.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnEventError`] if the sequence is exhausted or
    /// the event kind does not continue the current projection exactly.
    pub fn append_kind(
        &mut self,
        kind: ConversationTurnEventKind,
    ) -> Result<ConversationTurnEvent, ConversationTurnEventError> {
        let sequence = self
            .last_sequence
            .checked_add(1)
            .ok_or(ConversationTurnEventError::InvalidSequence)?;
        let event = ConversationTurnEvent {
            sequence,
            turn_id: self.turn_id.clone(),
            kind,
        };
        self.append_event(event.clone())?;
        Ok(event)
    }

    /// Applies one caller- or storage-provided event after full continuity checks.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnEventError`] for a forged owner, sequence,
    /// lifecycle edge, text offset, text shape, or post-terminal event.
    pub fn append_event(
        &mut self,
        event: ConversationTurnEvent,
    ) -> Result<(), ConversationTurnEventError> {
        let event = ConversationTurnEvent::restore(event)?;
        if event.turn_id != self.turn_id {
            return Err(ConversationTurnEventError::WrongTurn);
        }
        let expected_sequence = self
            .last_sequence
            .checked_add(1)
            .ok_or(ConversationTurnEventError::InvalidSequence)?;
        if event.sequence != expected_sequence {
            return Err(ConversationTurnEventError::InvalidSequence);
        }
        if self.state.is_some_and(ConversationTurnState::is_terminal) {
            return Err(ConversationTurnEventError::InvalidOrder);
        }

        match &event.kind {
            ConversationTurnEventKind::Created => {
                if self.state.is_some() || event.sequence != 1 {
                    return Err(ConversationTurnEventError::InvalidOrder);
                }
                self.state = Some(ConversationTurnState::Reserved);
            }
            ConversationTurnEventKind::StateChanged { from, to } => {
                if self.state != Some(*from) {
                    return Err(ConversationTurnEventError::InvalidOrder);
                }
                if !from.permits(*to) {
                    return Err(ConversationTurnEventError::InvalidTransition {
                        from: *from,
                        to: *to,
                    });
                }
                self.state = Some(*to);
            }
            ConversationTurnEventKind::TextAppended {
                start_utf8_offset,
                text,
            } => {
                if self.state != Some(ConversationTurnState::ProviderStarted) {
                    return Err(ConversationTurnEventError::InvalidOrder);
                }
                if self.text_events >= MAX_CONVERSATION_TEXT_EVENTS {
                    return Err(ConversationTurnEventError::EventLimitExceeded);
                }
                let expected_offset = u64::try_from(self.text.len())
                    .map_err(|_| ConversationTurnEventError::TextLimitExceeded)?;
                if *start_utf8_offset != expected_offset {
                    return Err(ConversationTurnEventError::InvalidTextOffset);
                }
                let new_length = self
                    .text
                    .len()
                    .checked_add(text.len())
                    .ok_or(ConversationTurnEventError::TextLimitExceeded)?;
                if new_length > MAX_MESSAGE_BYTES {
                    return Err(ConversationTurnEventError::TextLimitExceeded);
                }
                self.text.push_str(text);
                self.text_events += 1;
            }
        }
        self.last_sequence = event.sequence;
        Ok(())
    }

    /// Ensures the event projection represents the linked canonical turn exactly.
    ///
    /// A completed assistant message must exactly equal the concatenated durable
    /// text events. Other states must not expose an assistant message, while
    /// failed or uncertain turns may retain partial durable text for review.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnEventError::SnapshotMismatch`] for any owner,
    /// state, or assistant-text mismatch.
    pub fn validate_snapshot(
        &self,
        turn: &ConversationTurn,
        assistant_text: Option<&str>,
    ) -> Result<(), ConversationTurnEventError> {
        if self.turn_id != turn.id || self.state != Some(turn.state) {
            return Err(ConversationTurnEventError::SnapshotMismatch);
        }
        match turn.state {
            ConversationTurnState::Completed => {
                if self.text.is_empty() || assistant_text != Some(self.text.as_str()) {
                    return Err(ConversationTurnEventError::SnapshotMismatch);
                }
            }
            _ if assistant_text.is_some() => {
                return Err(ConversationTurnEventError::SnapshotMismatch);
            }
            _ => {}
        }
        Ok(())
    }

    /// Current projected lifecycle, absent only for a newly created empty log.
    #[must_use]
    pub const fn state(&self) -> Option<ConversationTurnState> {
        self.state
    }

    /// Concatenated normalized assistant text accumulated by the stream.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Last applied sequence, or zero for an empty projection.
    #[must_use]
    pub const fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Exact UTF-8 byte offset at which the next text chunk must begin.
    #[must_use]
    pub fn next_utf8_offset(&self) -> u64 {
        u64::try_from(self.text.len()).unwrap_or(u64::MAX)
    }

    /// Number of normalized text chunks in this projection.
    #[must_use]
    pub const fn text_event_count(&self) -> usize {
        self.text_events
    }
}

fn validate_turn_event_kind(
    kind: &ConversationTurnEventKind,
) -> Result<(), ConversationTurnEventError> {
    match kind {
        ConversationTurnEventKind::Created => Ok(()),
        ConversationTurnEventKind::StateChanged { from, to } => {
            if from.permits(*to) {
                Ok(())
            } else {
                Err(ConversationTurnEventError::InvalidTransition {
                    from: *from,
                    to: *to,
                })
            }
        }
        ConversationTurnEventKind::TextAppended {
            start_utf8_offset,
            text,
        } => {
            if text.is_empty()
                || text.len() > MAX_CONVERSATION_TEXT_CHUNK_BYTES
                || text.chars().any(|character| {
                    character == '\0'
                        || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
                })
            {
                return Err(ConversationTurnEventError::InvalidText);
            }
            let text_length = u64::try_from(text.len())
                .map_err(|_| ConversationTurnEventError::TextLimitExceeded)?;
            let maximum = u64::try_from(MAX_MESSAGE_BYTES)
                .map_err(|_| ConversationTurnEventError::TextLimitExceeded)?;
            if start_utf8_offset
                .checked_add(text_length)
                .is_none_or(|end| end > maximum)
            {
                return Err(ConversationTurnEventError::TextLimitExceeded);
            }
            Ok(())
        }
    }
}

/// Stable class for a provider-confirmed conversation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationFailureKind {
    /// The configured xAI credential was rejected.
    Authentication,
    /// The credential is valid but lacks endpoint/model scope or entitlement.
    Forbidden,
    /// xAI rejected the selected model or canonical input.
    InvalidRequest,
    /// xAI rate limited the request.
    RateLimited,
    /// The official provider was temporarily unavailable.
    Unavailable,
    /// The provider response violated the supported protocol.
    Protocol,
}

/// Sanitized provider failure retained for deterministic command replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationFailure {
    /// Stable failure class.
    pub kind: ConversationFailureKind,
    /// Bounded non-secret explanation.
    pub message: String,
    /// Whether a new command may succeed later.
    pub retryable: bool,
}

/// One bounded source attached to a completed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationCitation {
    /// Optional source title.
    pub title: Option<String>,
    /// Credential-free HTTPS source URL.
    pub url: String,
}

/// Provider-reported usage retained with the assistant outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConversationUsage {
    /// Input token count when reported.
    pub input_tokens: u64,
    /// Output token count when reported.
    pub output_tokens: u64,
    /// Exact xAI cost unit. One USD is 10,000,000,000 ticks.
    pub cost_in_usd_ticks: u64,
}

/// Invalid direct-conversation lifecycle or bounded metadata.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConversationTurnError {
    /// A persisted field violated its product bound.
    #[error("invalid conversation turn field: {0}")]
    InvalidField(&'static str),
    /// The requested state edge is not legal.
    #[error("invalid conversation turn transition from {from:?} to {to:?}")]
    InvalidTransition {
        /// Existing state.
        from: ConversationTurnState,
        /// Requested state.
        to: ConversationTurnState,
    },
    /// The new timestamp predates the current revision.
    #[error("conversation turn timestamp predates the current revision")]
    ClockRegression,
    /// The optimistic revision cannot advance further.
    #[error("conversation turn revision is exhausted")]
    RevisionExhausted,
}

/// Durable aggregate coordinating local state around one billable xAI request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurn {
    /// Stable turn identifier.
    pub id: ConversationTurnId,
    /// Caller command key, scoped to direct conversation execution.
    pub idempotency_key: String,
    /// SHA-256 of normalized command input.
    pub request_fingerprint: [u8; 32],
    /// SHA-256 of the immutable provider request once dispatch is reserved.
    pub provider_request_fingerprint: Option<[u8; 32]>,
    /// Owning project.
    pub project_id: ProjectId,
    /// Owning conversation thread.
    pub thread_id: ThreadId,
    /// Canonical user message committed with the reservation.
    pub user_message_id: MessageId,
    /// Durable execution run committed with the reservation.
    pub run_id: RunId,
    /// Product-selected and provider-validated xAI model identifier.
    pub model_id: String,
    /// Current lifecycle.
    pub state: ConversationTurnState,
    /// Non-idempotent provider effect after dispatch is reserved.
    pub effect_id: Option<EffectId>,
    /// Canonical assistant message for a completed turn.
    pub assistant_message_id: Option<MessageId>,
    /// Sanitized known provider failure.
    pub failure: Option<ConversationFailure>,
    /// Provider response identifier retained locally for diagnostics/continuation.
    pub provider_response_id: Option<String>,
    /// Bounded completed-response citations.
    pub citations: Vec<ConversationCitation>,
    /// Completed-response usage.
    pub usage: ConversationUsage,
    /// Per-response xAI ZDR header observation; absent means not observed.
    pub zero_data_retention: Option<bool>,
    /// Optimistic revision.
    pub revision: u64,
    /// Reservation timestamp.
    pub created_at: UnixMillis,
    /// Last durable transition timestamp.
    pub updated_at: UnixMillis,
}

impl ConversationTurn {
    /// Creates a reserved turn linked to already-validated local entity IDs.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for an unsafe command key or model ID.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve(
        id: ConversationTurnId,
        idempotency_key: String,
        request_fingerprint: [u8; 32],
        project_id: ProjectId,
        thread_id: ThreadId,
        user_message_id: MessageId,
        run_id: RunId,
        model_id: String,
        now: UnixMillis,
    ) -> Result<Self, ConversationTurnError> {
        validate_text(
            &idempotency_key,
            MAX_IDEMPOTENCY_KEY_BYTES,
            "idempotency_key",
        )?;
        validate_text(&model_id, MAX_MODEL_ID_BYTES, "model_id")?;
        Ok(Self {
            id,
            idempotency_key,
            request_fingerprint,
            provider_request_fingerprint: None,
            project_id,
            thread_id,
            user_message_id,
            run_id,
            model_id,
            state: ConversationTurnState::Reserved,
            effect_id: None,
            assistant_message_id: None,
            failure: None,
            provider_response_id: None,
            citations: Vec::new(),
            usage: ConversationUsage::default(),
            zero_data_retention: None,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rehydrates a durable snapshot after checking every aggregate invariant.
    ///
    /// Adapters must use this constructor instead of trusting persisted fields.
    /// The lifecycle state, provider-dispatch evidence, canonical outcome,
    /// revision, and timestamps are checked as one unit so a cancelled or
    /// uncertain request can never be made replayable by corrupt state.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError::InvalidField`] when bounded metadata is
    /// invalid or the snapshot could not have been produced by this aggregate.
    pub fn restore(snapshot: Self) -> Result<Self, ConversationTurnError> {
        validate_text(
            &snapshot.idempotency_key,
            MAX_IDEMPOTENCY_KEY_BYTES,
            "idempotency_key",
        )?;
        validate_text(&snapshot.model_id, MAX_MODEL_ID_BYTES, "model_id")?;
        validate_response_id(snapshot.provider_response_id.as_deref())?;
        validate_citations(&snapshot.citations)?;
        validate_usage(snapshot.usage)?;
        if let Some(failure) = &snapshot.failure {
            validate_failure(failure)?;
        }
        if !snapshot.is_reachable() {
            return Err(ConversationTurnError::InvalidField("persisted_state"));
        }
        Ok(snapshot)
    }

    /// Records the persisted non-idempotent provider boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] unless the turn is reserved.
    pub fn start_provider(
        &mut self,
        effect_id: EffectId,
        provider_request_fingerprint: [u8; 32],
        now: UnixMillis,
    ) -> Result<(), ConversationTurnError> {
        self.move_to(ConversationTurnState::ProviderStarted, now)?;
        self.effect_id = Some(effect_id);
        self.provider_request_fingerprint = Some(provider_request_fingerprint);
        Ok(())
    }

    /// Commits canonical assistant metadata after a provider completion event.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for invalid metadata or lifecycle state.
    pub fn complete(
        &mut self,
        assistant_message_id: MessageId,
        provider_response_id: Option<String>,
        citations: Vec<ConversationCitation>,
        usage: ConversationUsage,
        zero_data_retention: Option<bool>,
        now: UnixMillis,
    ) -> Result<(), ConversationTurnError> {
        validate_response_id(provider_response_id.as_deref())?;
        validate_citations(&citations)?;
        validate_usage(usage)?;
        self.move_to(ConversationTurnState::Completed, now)?;
        self.assistant_message_id = Some(assistant_message_id);
        self.provider_response_id = provider_response_id;
        self.citations = citations;
        self.usage = usage;
        self.zero_data_retention = zero_data_retention;
        Ok(())
    }

    /// Records a provider-confirmed failure.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] for invalid metadata or lifecycle state.
    pub fn fail(
        &mut self,
        failure: ConversationFailure,
        now: UnixMillis,
    ) -> Result<(), ConversationTurnError> {
        validate_failure(&failure)?;
        self.move_to(ConversationTurnState::Failed, now)?;
        self.failure = Some(failure);
        Ok(())
    }

    /// Cancels a reservation that provably did not reach xAI.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] unless provider dispatch has not started.
    pub fn cancel(&mut self, now: UnixMillis) -> Result<(), ConversationTurnError> {
        self.move_to(ConversationTurnState::Cancelled, now)
    }

    /// Marks an in-flight provider request uncertain and non-replayable.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationTurnError`] unless dispatch was previously persisted.
    pub fn interrupt(&mut self, now: UnixMillis) -> Result<(), ConversationTurnError> {
        self.move_to(ConversationTurnState::InterruptedNeedsReview, now)
    }

    fn move_to(
        &mut self,
        next: ConversationTurnState,
        now: UnixMillis,
    ) -> Result<(), ConversationTurnError> {
        if !self.state.permits(next) {
            return Err(ConversationTurnError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        if now < self.updated_at {
            return Err(ConversationTurnError::ClockRegression);
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ConversationTurnError::RevisionExhausted)?;
        self.state = next;
        self.updated_at = now;
        Ok(())
    }

    fn is_reachable(&self) -> bool {
        if self.updated_at < self.created_at {
            return false;
        }

        let dispatch_absent =
            self.provider_request_fingerprint.is_none() && self.effect_id.is_none();
        let dispatch_present =
            self.provider_request_fingerprint.is_some() && self.effect_id.is_some();
        let outcome_absent = self.assistant_message_id.is_none()
            && self.failure.is_none()
            && self.provider_response_id.is_none()
            && self.citations.is_empty()
            && self.usage == ConversationUsage::default()
            && self.zero_data_retention.is_none();

        match self.state {
            ConversationTurnState::Reserved => {
                self.revision == 0
                    && self.updated_at == self.created_at
                    && dispatch_absent
                    && outcome_absent
            }
            ConversationTurnState::ProviderStarted => {
                self.revision == 1 && dispatch_present && outcome_absent
            }
            ConversationTurnState::Completed => {
                self.revision == 2
                    && dispatch_present
                    && self.assistant_message_id.is_some()
                    && self.failure.is_none()
            }
            ConversationTurnState::Failed => {
                self.revision == 2
                    && dispatch_present
                    && self.assistant_message_id.is_none()
                    && self.failure.is_some()
                    && self.provider_response_id.is_none()
                    && self.citations.is_empty()
                    && self.usage == ConversationUsage::default()
                    && self.zero_data_retention.is_none()
            }
            ConversationTurnState::Cancelled => {
                self.revision == 1 && dispatch_absent && outcome_absent
            }
            ConversationTurnState::InterruptedNeedsReview => {
                self.revision == 2 && dispatch_present && outcome_absent
            }
        }
    }
}

fn validate_failure(failure: &ConversationFailure) -> Result<(), ConversationTurnError> {
    validate_text(
        &failure.message,
        MAX_FAILURE_MESSAGE_BYTES,
        "failure.message",
    )
}

fn validate_text(
    value: &str,
    maximum: usize,
    field: &'static str,
) -> Result<(), ConversationTurnError> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ConversationTurnError::InvalidField(field));
    }
    Ok(())
}

fn validate_credential_binding_id(value: &str) -> Result<(), ConversationTurnError> {
    if value.is_empty()
        || value.len() > MAX_CREDENTIAL_BINDING_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(ConversationTurnError::InvalidField("credential_binding_id"));
    }
    Ok(())
}

fn validate_response_id(value: Option<&str>) -> Result<(), ConversationTurnError> {
    if let Some(value) = value {
        validate_text(
            value,
            MAX_PROVIDER_RESPONSE_ID_BYTES,
            "provider_response_id",
        )?;
    }
    Ok(())
}

fn validate_citations(values: &[ConversationCitation]) -> Result<(), ConversationTurnError> {
    if values.len() > MAX_CITATIONS {
        return Err(ConversationTurnError::InvalidField("citations"));
    }
    let mut total_bytes = 0usize;
    for citation in values {
        if !is_credential_free_https_url(&citation.url) {
            return Err(ConversationTurnError::InvalidField("citation.url"));
        }
        if citation.title.as_deref().is_some_and(|title| {
            title.is_empty()
                || title.len() > MAX_CITATION_TITLE_BYTES
                || title.chars().any(char::is_control)
        }) {
            return Err(ConversationTurnError::InvalidField("citation.title"));
        }
        total_bytes = total_bytes
            .checked_add(citation.url.len())
            .and_then(|total| {
                citation
                    .title
                    .as_ref()
                    .map_or(Some(total), |title| total.checked_add(title.len()))
            })
            .ok_or(ConversationTurnError::InvalidField("citations"))?;
        if total_bytes > MAX_CONVERSATION_CITATION_TOTAL_BYTES {
            return Err(ConversationTurnError::InvalidField("citations"));
        }
    }
    Ok(())
}

fn is_credential_free_https_url(value: &str) -> bool {
    if value.len() > MAX_CITATION_URL_BYTES
        || !value.starts_with("https://")
        || value.contains('@')
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return false;
    }
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn validate_usage(usage: ConversationUsage) -> Result<(), ConversationTurnError> {
    if usage.input_tokens > MAX_CONVERSATION_USAGE_VALUE
        || usage.output_tokens > MAX_CONVERSATION_USAGE_VALUE
        || usage.cost_in_usd_ticks > MAX_CONVERSATION_USAGE_VALUE
    {
        return Err(ConversationTurnError::InvalidField("usage"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_lineage_is_bounded_and_never_self_references() {
        let owner = ConversationTurnId::new("turn-1").expect("owner");
        let source = ConversationTurnId::new("turn-0").expect("source");
        let original =
            ConversationTurnLineage::original("xai-binding-1".into()).expect("bound original");
        assert_eq!(
            ConversationTurnLineage::restore(original.clone(), &owner),
            Ok(original)
        );

        let retry =
            ConversationTurnLineage::retry(source, "xai-binding-1".into(), 0).expect("bound retry");
        assert_eq!(
            ConversationTurnLineage::restore(retry.clone(), &owner),
            Ok(retry)
        );
        let self_retry = ConversationTurnLineage {
            origin: ConversationTurnOrigin::Retry {
                source_turn_id: owner.clone(),
            },
            credential_binding_id: Some("xai-binding-1".into()),
            retry_depth: 1,
        };
        assert_eq!(
            ConversationTurnLineage::restore(self_retry, &owner),
            Err(ConversationTurnError::InvalidField("lineage"))
        );
        assert!(ConversationTurnLineage::original("contains space".into()).is_err());
    }

    #[test]
    fn fork_attempt_lineage_starts_a_fresh_retry_chain() {
        let owner = ConversationTurnId::new("turn-child").expect("owner");
        let source = ConversationTurnId::new("turn-parent").expect("source");
        let edited =
            ConversationTurnLineage::edit_and_branch(source.clone(), "xai-binding-1".into())
                .expect("edited lineage");
        assert_eq!(edited.retry_depth, 0);
        assert!(matches!(
            &edited.origin,
            ConversationTurnOrigin::EditAndBranch { .. }
        ));
        assert_eq!(
            ConversationTurnLineage::restore(edited.clone(), &owner),
            Ok(edited)
        );

        let regenerated =
            ConversationTurnLineage::regenerate(source.clone(), "xai-binding-1".into())
                .expect("regenerate lineage");
        assert_eq!(regenerated.retry_depth, 0);
        assert!(matches!(
            &regenerated.origin,
            ConversationTurnOrigin::Regenerate { .. }
        ));
        assert_eq!(
            ConversationTurnLineage::restore(regenerated.clone(), &owner),
            Ok(regenerated)
        );

        let retry = ConversationTurnLineage::retry(owner.clone(), "xai-binding-1".into(), 0)
            .expect("retry fork attempt");
        let retry_owner = ConversationTurnId::new("turn-retry").expect("retry owner");
        assert_eq!(retry.retry_depth, 1);
        assert_eq!(
            ConversationTurnLineage::restore(retry.clone(), &retry_owner),
            Ok(retry)
        );
    }

    #[test]
    fn fork_attempt_lineage_requires_binding_depth_zero_and_a_distinct_source() {
        let owner = ConversationTurnId::new("turn-child").expect("owner");
        for origin in [
            ConversationTurnOrigin::EditAndBranch {
                source_turn_id: ConversationTurnId::new("turn-parent").expect("source"),
            },
            ConversationTurnOrigin::Regenerate {
                source_turn_id: ConversationTurnId::new("turn-parent").expect("source"),
            },
        ] {
            let unbound = ConversationTurnLineage {
                origin: origin.clone(),
                credential_binding_id: None,
                retry_depth: 0,
            };
            assert_eq!(
                ConversationTurnLineage::restore(unbound, &owner),
                Err(ConversationTurnError::InvalidField("lineage"))
            );
            let wrong_depth = ConversationTurnLineage {
                origin,
                credential_binding_id: Some("xai-binding-1".into()),
                retry_depth: 1,
            };
            assert_eq!(
                ConversationTurnLineage::restore(wrong_depth, &owner),
                Err(ConversationTurnError::InvalidField("lineage"))
            );
        }

        for self_origin in [
            ConversationTurnOrigin::EditAndBranch {
                source_turn_id: owner.clone(),
            },
            ConversationTurnOrigin::Regenerate {
                source_turn_id: owner.clone(),
            },
        ] {
            assert_eq!(
                ConversationTurnLineage::restore(
                    ConversationTurnLineage {
                        origin: self_origin,
                        credential_binding_id: Some("xai-binding-1".into()),
                        retry_depth: 0,
                    },
                    &owner,
                ),
                Err(ConversationTurnError::InvalidField("lineage"))
            );
        }
    }

    #[test]
    fn only_legacy_original_lineage_may_be_unbound() {
        let owner = ConversationTurnId::new("turn-1").expect("owner");
        let legacy = ConversationTurnLineage {
            origin: ConversationTurnOrigin::Original,
            credential_binding_id: None,
            retry_depth: 0,
        };
        assert_eq!(
            ConversationTurnLineage::restore(legacy.clone(), &owner),
            Ok(legacy)
        );
        let unbound_retry = ConversationTurnLineage {
            origin: ConversationTurnOrigin::Retry {
                source_turn_id: ConversationTurnId::new("turn-0").expect("source"),
            },
            credential_binding_id: None,
            retry_depth: 1,
        };
        assert_eq!(
            ConversationTurnLineage::restore(unbound_retry, &owner),
            Err(ConversationTurnError::InvalidField("lineage"))
        );
    }

    fn reserved() -> ConversationTurn {
        ConversationTurn::reserve(
            ConversationTurnId::new("turn-1").expect("id"),
            "command-1".into(),
            [1; 32],
            ProjectId::new("project-1").expect("id"),
            ThreadId::new("thread-1").expect("id"),
            MessageId::new("message-1").expect("id"),
            RunId::new("run-1").expect("id"),
            "grok-test".into(),
            1,
        )
        .expect("turn")
    }

    fn started() -> ConversationTurn {
        let mut turn = reserved();
        turn.start_provider(EffectId::new("effect-1").expect("id"), [2; 32], 2)
            .expect("start");
        turn
    }

    fn completed() -> ConversationTurn {
        let mut turn = started();
        turn.complete(
            MessageId::new("message-2").expect("id"),
            Some("response-1".into()),
            vec![citation()],
            usage(),
            Some(true),
            3,
        )
        .expect("complete");
        turn
    }

    fn failed() -> ConversationTurn {
        let mut turn = started();
        turn.fail(failure(), 3).expect("fail");
        turn
    }

    fn cancelled() -> ConversationTurn {
        let mut turn = reserved();
        turn.cancel(2).expect("cancel");
        turn
    }

    fn interrupted() -> ConversationTurn {
        let mut turn = started();
        turn.interrupt(3).expect("interrupt");
        turn
    }

    fn failure() -> ConversationFailure {
        ConversationFailure {
            kind: ConversationFailureKind::Unavailable,
            message: "provider unavailable".into(),
            retryable: true,
        }
    }

    fn citation() -> ConversationCitation {
        ConversationCitation {
            title: Some("Source".into()),
            url: "https://example.test/source".into(),
        }
    }

    const fn usage() -> ConversationUsage {
        ConversationUsage {
            input_tokens: 3,
            output_tokens: 5,
            cost_in_usd_ticks: 7,
        }
    }

    fn reachable_snapshots() -> Vec<ConversationTurn> {
        vec![
            reserved(),
            started(),
            completed(),
            failed(),
            cancelled(),
            interrupted(),
        ]
    }

    fn assert_invalid_persisted_state(snapshot: ConversationTurn) {
        assert_eq!(
            ConversationTurn::restore(snapshot),
            Err(ConversationTurnError::InvalidField("persisted_state"))
        );
    }

    fn assert_rejects_each_outcome_field(snapshot: &ConversationTurn) {
        let mut with_assistant = snapshot.clone();
        with_assistant.assistant_message_id = Some(MessageId::new("message-2").expect("id"));
        assert_invalid_persisted_state(with_assistant);

        let mut with_failure = snapshot.clone();
        with_failure.failure = Some(failure());
        assert_invalid_persisted_state(with_failure);

        let mut with_response = snapshot.clone();
        with_response.provider_response_id = Some("response-1".into());
        assert_invalid_persisted_state(with_response);

        let mut with_citation = snapshot.clone();
        with_citation.citations = vec![citation()];
        assert_invalid_persisted_state(with_citation);

        let mut with_usage = snapshot.clone();
        with_usage.usage = usage();
        assert_invalid_persisted_state(with_usage);

        let mut with_certainty = snapshot.clone();
        with_certainty.zero_data_retention = Some(false);
        assert_invalid_persisted_state(with_certainty);
    }

    fn assert_rejects_missing_dispatch_evidence(snapshot: &ConversationTurn) {
        let mut without_request = snapshot.clone();
        without_request.provider_request_fingerprint = None;
        assert_invalid_persisted_state(without_request);

        let mut without_effect = snapshot.clone();
        without_effect.effect_id = None;
        assert_invalid_persisted_state(without_effect);

        let mut without_either = snapshot.clone();
        without_either.provider_request_fingerprint = None;
        without_either.effect_id = None;
        assert_invalid_persisted_state(without_either);
    }

    fn assert_rejects_dispatch_evidence(snapshot: &ConversationTurn) {
        let mut with_request = snapshot.clone();
        with_request.provider_request_fingerprint = Some([2; 32]);
        assert_invalid_persisted_state(with_request);

        let mut with_effect = snapshot.clone();
        with_effect.effect_id = Some(EffectId::new("effect-1").expect("id"));
        assert_invalid_persisted_state(with_effect);

        let mut with_both = snapshot.clone();
        with_both.provider_request_fingerprint = Some([2; 32]);
        with_both.effect_id = Some(EffectId::new("effect-1").expect("id"));
        assert_invalid_persisted_state(with_both);
    }

    #[test]
    fn provider_started_turn_cannot_be_cancelled_or_restarted() {
        let mut turn = reserved();
        turn.start_provider(EffectId::new("effect-1").expect("id"), [2; 32], 2)
            .expect("start");
        assert!(turn.cancel(3).is_err());
        assert!(
            turn.start_provider(EffectId::new("effect-2").expect("id"), [3; 32], 3,)
                .is_err()
        );
    }

    #[test]
    fn completed_turn_retains_bounded_canonical_outcome() {
        let turn = completed();
        assert_eq!(turn.state, ConversationTurnState::Completed);
        assert_eq!(turn.usage.cost_in_usd_ticks, 7);
        assert_eq!(turn.zero_data_retention, Some(true));
        assert!(turn.state.is_terminal());
    }

    #[test]
    fn restore_accepts_every_reachable_state_and_preserves_exact_fields() {
        for snapshot in reachable_snapshots() {
            assert_eq!(
                ConversationTurn::restore(snapshot.clone()).expect("reachable snapshot"),
                snapshot
            );
        }

        let mut same_timestamp = reachable_snapshots();
        same_timestamp.remove(0);
        for mut snapshot in same_timestamp {
            snapshot.updated_at = snapshot.created_at;
            assert_eq!(
                ConversationTurn::restore(snapshot.clone()).expect("same-time transitions"),
                snapshot
            );
        }
    }

    #[test]
    fn restore_rejects_dispatch_evidence_in_pre_dispatch_states() {
        assert_rejects_dispatch_evidence(&reserved());
        assert_rejects_dispatch_evidence(&cancelled());
    }

    #[test]
    fn restore_requires_complete_dispatch_evidence_after_provider_start() {
        for snapshot in [started(), completed(), failed(), interrupted()] {
            assert_rejects_missing_dispatch_evidence(&snapshot);
        }
    }

    #[test]
    fn restore_enforces_the_outcome_shape_for_every_lifecycle_state() {
        for snapshot in [reserved(), started(), cancelled(), interrupted()] {
            assert_rejects_each_outcome_field(&snapshot);
        }

        let mut completed_without_assistant = completed();
        completed_without_assistant.assistant_message_id = None;
        assert_invalid_persisted_state(completed_without_assistant);

        let mut completed_with_failure = completed();
        completed_with_failure.failure = Some(failure());
        assert_invalid_persisted_state(completed_with_failure);

        let mut failed_without_failure = failed();
        failed_without_failure.failure = None;
        assert_invalid_persisted_state(failed_without_failure);

        let failed = failed();
        let mut failed_with_assistant = failed.clone();
        failed_with_assistant.assistant_message_id = Some(MessageId::new("message-2").expect("id"));
        assert_invalid_persisted_state(failed_with_assistant);

        let mut failed_with_response = failed.clone();
        failed_with_response.provider_response_id = Some("response-1".into());
        assert_invalid_persisted_state(failed_with_response);

        let mut failed_with_citations = failed.clone();
        failed_with_citations.citations = vec![citation()];
        assert_invalid_persisted_state(failed_with_citations);

        let mut failed_with_usage = failed.clone();
        failed_with_usage.usage = usage();
        assert_invalid_persisted_state(failed_with_usage);

        let mut failed_with_certainty = failed;
        failed_with_certainty.zero_data_retention = Some(true);
        assert_invalid_persisted_state(failed_with_certainty);
    }

    #[test]
    fn restore_rejects_unreachable_revisions_and_timestamps() {
        for snapshot in reachable_snapshots() {
            let mut wrong_revision = snapshot.clone();
            wrong_revision.revision += 1;
            assert_invalid_persisted_state(wrong_revision);

            let mut regressed = snapshot;
            regressed.created_at = regressed.updated_at + 1;
            assert_invalid_persisted_state(regressed);
        }

        let mut reserved_with_transition_time = reserved();
        reserved_with_transition_time.updated_at += 1;
        assert_invalid_persisted_state(reserved_with_transition_time);
    }

    #[test]
    fn restore_revalidates_all_bounded_untrusted_metadata() {
        let mut invalid_idempotency_key = reserved();
        invalid_idempotency_key.idempotency_key = " command-1".into();
        assert_eq!(
            ConversationTurn::restore(invalid_idempotency_key),
            Err(ConversationTurnError::InvalidField("idempotency_key"))
        );

        let mut invalid_model = reserved();
        invalid_model.model_id = "grok\nsecret".into();
        assert_eq!(
            ConversationTurn::restore(invalid_model),
            Err(ConversationTurnError::InvalidField("model_id"))
        );

        let mut invalid_response = completed();
        invalid_response.provider_response_id = Some("response\rsecret".into());
        assert_eq!(
            ConversationTurn::restore(invalid_response),
            Err(ConversationTurnError::InvalidField("provider_response_id"))
        );

        let mut invalid_failure = failed();
        invalid_failure.failure.as_mut().expect("failure").message = String::new();
        assert_eq!(
            ConversationTurn::restore(invalid_failure),
            Err(ConversationTurnError::InvalidField("failure.message"))
        );

        for invalid_url in [
            "http://example.test/source".to_owned(),
            "https://".to_owned(),
            "https:// example.test/source".to_owned(),
            "https://#fragment".to_owned(),
            "https://example.test:99999/source".to_owned(),
            "https://user:password@example.test/source".to_owned(),
            "https://example.test/source\nsecret".to_owned(),
            format!("https://{}", "a".repeat(MAX_CITATION_URL_BYTES)),
        ] {
            let mut invalid_citation_url = completed();
            invalid_citation_url.citations[0].url = invalid_url;
            assert_eq!(
                ConversationTurn::restore(invalid_citation_url),
                Err(ConversationTurnError::InvalidField("citation.url"))
            );
        }

        for invalid_title in [
            String::new(),
            "Source\nsecret".to_owned(),
            "a".repeat(MAX_CITATION_TITLE_BYTES + 1),
        ] {
            let mut invalid_citation_title = completed();
            invalid_citation_title.citations[0].title = Some(invalid_title);
            assert_eq!(
                ConversationTurn::restore(invalid_citation_title),
                Err(ConversationTurnError::InvalidField("citation.title"))
            );
        }

        let mut too_many_citations = completed();
        too_many_citations.citations = vec![citation(); MAX_CITATIONS + 1];
        assert_eq!(
            ConversationTurn::restore(too_many_citations),
            Err(ConversationTurnError::InvalidField("citations"))
        );
    }

    #[test]
    fn restore_accepts_exact_metadata_bounds_and_rejects_one_byte_over() {
        let mut at_bounds = completed();
        at_bounds.idempotency_key = "i".repeat(MAX_IDEMPOTENCY_KEY_BYTES);
        at_bounds.model_id = "m".repeat(MAX_MODEL_ID_BYTES);
        at_bounds.provider_response_id = Some("r".repeat(MAX_PROVIDER_RESPONSE_ID_BYTES));
        let boundary_citation = ConversationCitation {
            title: Some("t".repeat(MAX_CITATION_TITLE_BYTES)),
            url: format!(
                "https://example.test/{}",
                "u".repeat(MAX_CITATION_URL_BYTES - "https://example.test/".len())
            ),
        };
        at_bounds.citations = vec![boundary_citation; 115];
        at_bounds.citations.push(ConversationCitation {
            title: None,
            url: format!(
                "https://example.test/{}",
                "u".repeat(420 - "https://example.test/".len())
            ),
        });
        at_bounds.usage = ConversationUsage {
            input_tokens: MAX_CONVERSATION_USAGE_VALUE,
            output_tokens: MAX_CONVERSATION_USAGE_VALUE,
            cost_in_usd_ticks: MAX_CONVERSATION_USAGE_VALUE,
        };
        assert_eq!(
            ConversationTurn::restore(at_bounds.clone()).expect("exact metadata bounds"),
            at_bounds
        );

        let mut citation_count_at_bound = completed();
        citation_count_at_bound.citations = vec![citation(); MAX_CITATIONS];
        assert!(ConversationTurn::restore(citation_count_at_bound).is_ok());

        let mut failure_at_bound = failed();
        failure_at_bound.failure.as_mut().expect("failure").message =
            "f".repeat(MAX_FAILURE_MESSAGE_BYTES);
        assert_eq!(
            ConversationTurn::restore(failure_at_bound.clone()).expect("failure bound"),
            failure_at_bound
        );

        let mut idempotency_over = reserved();
        idempotency_over.idempotency_key = "i".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1);
        assert!(ConversationTurn::restore(idempotency_over).is_err());

        let mut model_over = reserved();
        model_over.model_id = "m".repeat(MAX_MODEL_ID_BYTES + 1);
        assert!(ConversationTurn::restore(model_over).is_err());

        let mut response_over = completed();
        response_over.provider_response_id = Some("r".repeat(MAX_PROVIDER_RESPONSE_ID_BYTES + 1));
        assert!(ConversationTurn::restore(response_over).is_err());

        let mut failure_over = failed();
        failure_over.failure.as_mut().expect("failure").message =
            "f".repeat(MAX_FAILURE_MESSAGE_BYTES + 1);
        assert!(ConversationTurn::restore(failure_over).is_err());

        let mut citation_url_over = completed();
        citation_url_over.citations[0].url = format!(
            "https://example.test/{}",
            "u".repeat(MAX_CITATION_URL_BYTES + 1 - "https://example.test/".len())
        );
        assert!(ConversationTurn::restore(citation_url_over).is_err());

        let mut citation_title_over = completed();
        citation_title_over.citations[0].title = Some("t".repeat(MAX_CITATION_TITLE_BYTES + 1));
        assert!(ConversationTurn::restore(citation_title_over).is_err());

        let mut citation_count_over = completed();
        citation_count_over.citations = vec![citation(); MAX_CITATIONS + 1];
        assert!(ConversationTurn::restore(citation_count_over).is_err());

        let mut citation_total_over = at_bounds;
        citation_total_over
            .citations
            .last_mut()
            .expect("citation")
            .url
            .push('u');
        assert_eq!(
            ConversationTurn::restore(citation_total_over),
            Err(ConversationTurnError::InvalidField("citations"))
        );

        let mut usage_over = completed();
        usage_over.usage.input_tokens = MAX_CONVERSATION_USAGE_VALUE + 1;
        assert_eq!(
            ConversationTurn::restore(usage_over),
            Err(ConversationTurnError::InvalidField("usage"))
        );
    }

    #[test]
    fn restored_turns_preserve_cancellation_and_terminal_semantics() {
        let mut provider_started =
            ConversationTurn::restore(started()).expect("restore provider-started");
        assert!(matches!(
            provider_started.cancel(4),
            Err(ConversationTurnError::InvalidTransition {
                from: ConversationTurnState::ProviderStarted,
                to: ConversationTurnState::Cancelled,
            })
        ));
        provider_started.interrupt(4).expect("mark uncertain");

        for snapshot in [completed(), failed(), cancelled(), interrupted()] {
            let expected_state = snapshot.state;
            let mut restored = ConversationTurn::restore(snapshot).expect("restore terminal");
            assert!(restored.state.is_terminal());
            assert!(matches!(
                restored.start_provider(
                    EffectId::new("effect-replay").expect("id"),
                    [9; 32],
                    4,
                ),
                Err(ConversationTurnError::InvalidTransition { from, to })
                    if from == expected_state && to == ConversationTurnState::ProviderStarted
            ));
        }
    }

    fn started_event_log() -> ConversationTurnEventLog {
        let mut log = ConversationTurnEventLog::new(ConversationTurnId::new("turn-1").expect("id"));
        log.append_kind(ConversationTurnEventKind::Created)
            .expect("created");
        log.append_kind(ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::Reserved,
            to: ConversationTurnState::ProviderStarted,
        })
        .expect("provider started");
        log
    }

    #[test]
    fn turn_event_log_projects_exact_utf8_offsets_and_completed_text() {
        let mut log = started_event_log();
        let first = log
            .append_kind(ConversationTurnEventKind::TextAppended {
                start_utf8_offset: 0,
                text: "é".into(),
            })
            .expect("first text");
        let second = log
            .append_kind(ConversationTurnEventKind::TextAppended {
                start_utf8_offset: 2,
                text: "🙂x".into(),
            })
            .expect("second text");
        assert_eq!(first.sequence, 3);
        assert_eq!(second.sequence, 4);
        assert_eq!(log.next_utf8_offset(), 7);
        assert_eq!(log.text(), "é🙂x");

        log.append_kind(ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::ProviderStarted,
            to: ConversationTurnState::Completed,
        })
        .expect("completed");
        let mut turn = started();
        turn.complete(
            MessageId::new("assistant-event").expect("id"),
            None,
            Vec::new(),
            ConversationUsage::default(),
            None,
            3,
        )
        .expect("completed turn");
        assert!(log.validate_snapshot(&turn, Some("é🙂x")).is_ok());
        assert_eq!(
            log.validate_snapshot(&turn, Some("é🙂y")),
            Err(ConversationTurnEventError::SnapshotMismatch)
        );
        assert_eq!(
            log.validate_snapshot(&turn, None),
            Err(ConversationTurnEventError::SnapshotMismatch)
        );
    }

    #[test]
    fn turn_event_restore_rejects_forged_sequence_owner_order_and_offsets() {
        let turn_id = ConversationTurnId::new("turn-1").expect("id");
        let canonical = vec![
            ConversationTurnEvent {
                sequence: 1,
                turn_id: turn_id.clone(),
                kind: ConversationTurnEventKind::Created,
            },
            ConversationTurnEvent {
                sequence: 2,
                turn_id: turn_id.clone(),
                kind: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::ProviderStarted,
                },
            },
            ConversationTurnEvent {
                sequence: 3,
                turn_id: turn_id.clone(),
                kind: ConversationTurnEventKind::TextAppended {
                    start_utf8_offset: 0,
                    text: "answer".into(),
                },
            },
        ];
        assert!(ConversationTurnEventLog::restore(turn_id.clone(), &canonical).is_ok());

        let mut gap = canonical.clone();
        gap[1].sequence = 3;
        assert_eq!(
            ConversationTurnEventLog::restore(turn_id.clone(), &gap),
            Err(ConversationTurnEventError::InvalidSequence)
        );
        let mut wrong_owner = canonical.clone();
        wrong_owner[1].turn_id = ConversationTurnId::new("turn-other").expect("id");
        assert_eq!(
            ConversationTurnEventLog::restore(turn_id.clone(), &wrong_owner),
            Err(ConversationTurnEventError::WrongTurn)
        );
        let mut duplicate_created = canonical.clone();
        duplicate_created[1].kind = ConversationTurnEventKind::Created;
        assert_eq!(
            ConversationTurnEventLog::restore(turn_id.clone(), &duplicate_created),
            Err(ConversationTurnEventError::InvalidOrder)
        );
        let mut wrong_from = canonical.clone();
        wrong_from[1].kind = ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::ProviderStarted,
            to: ConversationTurnState::Failed,
        };
        assert_eq!(
            ConversationTurnEventLog::restore(turn_id.clone(), &wrong_from),
            Err(ConversationTurnEventError::InvalidOrder)
        );
        let mut wrong_offset = canonical;
        wrong_offset[2].kind = ConversationTurnEventKind::TextAppended {
            start_utf8_offset: 1,
            text: "answer".into(),
        };
        assert_eq!(
            ConversationTurnEventLog::restore(turn_id, &wrong_offset),
            Err(ConversationTurnEventError::InvalidTextOffset)
        );
    }

    #[test]
    fn turn_event_text_shape_and_chunk_bounds_fail_closed() {
        let turn_id = ConversationTurnId::new("turn-1").expect("id");
        for text in [
            String::new(),
            "x".repeat(MAX_CONVERSATION_TEXT_CHUNK_BYTES + 1),
            "unsafe\0text".into(),
            "unsafe\u{0007}text".into(),
        ] {
            assert_eq!(
                ConversationTurnEvent::restore(ConversationTurnEvent {
                    sequence: 1,
                    turn_id: turn_id.clone(),
                    kind: ConversationTurnEventKind::TextAppended {
                        start_utf8_offset: 0,
                        text,
                    },
                }),
                Err(ConversationTurnEventError::InvalidText)
            );
        }
        assert!(
            ConversationTurnEvent::restore(ConversationTurnEvent {
                sequence: 1,
                turn_id: turn_id.clone(),
                kind: ConversationTurnEventKind::TextAppended {
                    start_utf8_offset: 0,
                    text: "x".repeat(MAX_CONVERSATION_TEXT_CHUNK_BYTES),
                },
            })
            .is_ok()
        );
        assert_eq!(
            ConversationTurnEvent::restore(ConversationTurnEvent {
                sequence: 0,
                turn_id,
                kind: ConversationTurnEventKind::Created,
            }),
            Err(ConversationTurnEventError::InvalidSequence)
        );
    }

    #[test]
    fn turn_event_log_enforces_cumulative_text_and_event_count_bounds() {
        let mut full = started_event_log();
        while full.text().len() < MAX_MESSAGE_BYTES {
            let remaining = MAX_MESSAGE_BYTES - full.text().len();
            let length = remaining.min(MAX_CONVERSATION_TEXT_CHUNK_BYTES);
            full.append_kind(ConversationTurnEventKind::TextAppended {
                start_utf8_offset: full.next_utf8_offset(),
                text: "x".repeat(length),
            })
            .expect("bounded chunk");
        }
        assert_eq!(full.text().len(), MAX_MESSAGE_BYTES);
        assert_eq!(
            full.append_kind(ConversationTurnEventKind::TextAppended {
                start_utf8_offset: full.next_utf8_offset(),
                text: "x".into(),
            }),
            Err(ConversationTurnEventError::TextLimitExceeded)
        );

        let mut too_many = started_event_log();
        for _ in 0..MAX_CONVERSATION_TEXT_EVENTS {
            too_many
                .append_kind(ConversationTurnEventKind::TextAppended {
                    start_utf8_offset: too_many.next_utf8_offset(),
                    text: "x".into(),
                })
                .expect("event within count bound");
        }
        assert_eq!(too_many.text_event_count(), MAX_CONVERSATION_TEXT_EVENTS);
        assert_eq!(
            too_many.append_kind(ConversationTurnEventKind::TextAppended {
                start_utf8_offset: too_many.next_utf8_offset(),
                text: "x".into(),
            }),
            Err(ConversationTurnEventError::EventLimitExceeded)
        );
    }

    #[test]
    fn turn_event_log_rejects_post_terminal_events_and_sequence_exhaustion() {
        let turn_id = ConversationTurnId::new("turn-1").expect("id");
        let mut cancelled = ConversationTurnEventLog::new(turn_id.clone());
        cancelled
            .append_kind(ConversationTurnEventKind::Created)
            .expect("created");
        cancelled
            .append_kind(ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::Reserved,
                to: ConversationTurnState::Cancelled,
            })
            .expect("cancelled");
        assert_eq!(
            cancelled.append_kind(ConversationTurnEventKind::Created),
            Err(ConversationTurnEventError::InvalidOrder)
        );

        let mut exhausted = ConversationTurnEventLog::new(turn_id);
        exhausted.last_sequence = u64::MAX;
        assert_eq!(
            exhausted.append_kind(ConversationTurnEventKind::Created),
            Err(ConversationTurnEventError::InvalidSequence)
        );
    }
}
