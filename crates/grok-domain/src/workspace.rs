use thiserror::Error;

use crate::{
    ArtifactId, AutomationId, AutomationSchedule, ConversationTurnId, MessageId, ProjectId,
    ThreadId, UnixMillis,
};

const MAX_NAME_BYTES: usize = 200;
const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
/// Maximum UTF-8 byte length of a canonical conversation message.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_SOURCE_CONTEXT_SEQUENCE: u32 = 1_000;
/// Highest content version accepted for one durable artifact.
pub const MAX_ARTIFACT_CONTENT_VERSION: u32 = 1_000_000;

/// Invalid workspace entity input or lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkspaceError {
    /// A user-controlled field is missing, oversized, or malformed.
    #[error("invalid {field}: {reason}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Non-sensitive validation reason.
        reason: &'static str,
    },
    /// Archived or deleted content cannot be mutated.
    #[error("{entity} is not editable in its current state")]
    InvalidLifecycle {
        /// Stable entity kind.
        entity: &'static str,
    },
    /// An update timestamp predates the entity's current timestamp.
    #[error("workspace timestamp predates the current revision")]
    ClockRegression,
    /// Optimistic revision cannot advance without wrapping.
    #[error("workspace revision is exhausted")]
    RevisionExhausted,
}

/// Project lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectState {
    /// Project accepts new threads and content.
    Active,
    /// Project remains readable but is no longer editable.
    Archived,
}

/// Locally owned workspace and conversation container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Stable project identifier.
    pub id: ProjectId,
    /// User-visible name.
    pub name: String,
    /// Optional user-visible description.
    pub description: String,
    /// Current lifecycle state.
    pub state: ProjectState,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: UnixMillis,
    /// Last successful update timestamp.
    pub updated_at: UnixMillis,
}

impl Project {
    /// Creates an active project.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid text fields.
    pub fn new(
        id: ProjectId,
        name: String,
        description: String,
        now: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        validate_text("project.name", &name, 1, MAX_NAME_BYTES, true)?;
        validate_text(
            "project.description",
            &description,
            0,
            MAX_DESCRIPTION_BYTES,
            false,
        )?;
        Ok(Self {
            id,
            name,
            description,
            state: ProjectState::Active,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Updates editable project metadata.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid fields, clock regression, or an
    /// archived project.
    pub fn update(
        &mut self,
        name: String,
        description: String,
        now: UnixMillis,
    ) -> Result<(), WorkspaceError> {
        self.ensure_active("project")?;
        validate_text("project.name", &name, 1, MAX_NAME_BYTES, true)?;
        validate_text(
            "project.description",
            &description,
            0,
            MAX_DESCRIPTION_BYTES,
            false,
        )?;
        self.touch(now)?;
        self.name = name;
        self.description = description;
        Ok(())
    }

    /// Archives a project.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for clock regression or an archived project.
    pub fn archive(&mut self, now: UnixMillis) -> Result<(), WorkspaceError> {
        self.ensure_active("project")?;
        self.touch(now)?;
        self.state = ProjectState::Archived;
        Ok(())
    }

    fn ensure_active(&self, entity: &'static str) -> Result<(), WorkspaceError> {
        if self.state == ProjectState::Archived {
            return Err(WorkspaceError::InvalidLifecycle { entity });
        }
        Ok(())
    }

    fn touch(&mut self, now: UnixMillis) -> Result<(), WorkspaceError> {
        touch(&mut self.revision, &mut self.updated_at, now)
    }
}

/// Conversation thread lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    /// Thread accepts new messages.
    Open,
    /// Thread remains readable but does not accept edits.
    Archived,
}

/// Explicit operation that created a child conversation thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationForkKind {
    /// Copy completed context and its source assistant without a provider call.
    Branch,
    /// Replace the source user prompt and start a new billable turn.
    EditAndBranch,
    /// Reuse the exact source prompt and start a new billable turn.
    Regenerate,
}

impl ConversationForkKind {
    /// Canonical role of the immediate source message for this operation.
    #[must_use]
    pub const fn source_message_role(self) -> MessageRole {
        match self {
            Self::Branch | Self::Regenerate => MessageRole::Assistant,
            Self::EditAndBranch => MessageRole::User,
        }
    }
}

/// Immutable reason one conversation thread exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationThreadOrigin {
    /// An ordinary user-created thread with no parent.
    Original,
    /// An explicit child of one canonical parent thread.
    Fork {
        /// Immediate parent whose canonical context was selected.
        parent_thread_id: ThreadId,
        /// Turn at which the parent history diverged.
        source_turn_id: ConversationTurnId,
        /// Exact parent-owned message selected by the operation.
        source_message_id: MessageId,
        /// Explicit fork behavior; never inferred from copied content.
        kind: ConversationForkKind,
    },
}

/// Validated immutable ancestry for one conversation thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationThreadLineage {
    /// Stable family root. An original thread is its own root.
    pub root_thread_id: ThreadId,
    /// Immutable creation classification and immediate source.
    pub origin: ConversationThreadOrigin,
    /// Number of parent edges from the root, bounded to 64.
    pub fork_depth: u8,
}

impl ConversationThreadLineage {
    /// Creates lineage for a new root thread.
    #[must_use]
    pub fn original(owner_thread_id: ThreadId) -> Self {
        Self {
            root_thread_id: owner_thread_id,
            origin: ConversationThreadOrigin::Original,
            fork_depth: 0,
        }
    }

    /// Derives one child lineage from a validated immediate parent.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for a self-reference, exhausted depth, an
    /// invalid parent lineage, or a source-message role inconsistent with the
    /// explicit fork kind.
    #[allow(clippy::too_many_arguments)]
    pub fn fork(
        owner_thread_id: &ThreadId,
        parent_thread_id: ThreadId,
        parent_lineage: &Self,
        source_turn_id: ConversationTurnId,
        source_message_id: MessageId,
        source_message_role: MessageRole,
        kind: ConversationForkKind,
    ) -> Result<Self, WorkspaceError> {
        Self::restore(parent_lineage.clone(), &parent_thread_id)?;
        if &parent_thread_id == owner_thread_id || kind.source_message_role() != source_message_role
        {
            return Err(invalid_lineage("thread.lineage"));
        }
        let fork_depth = parent_lineage
            .fork_depth
            .checked_add(1)
            .filter(|depth| *depth <= 64)
            .ok_or_else(|| invalid_lineage("thread.fork_depth"))?;
        let lineage = Self {
            root_thread_id: parent_lineage.root_thread_id.clone(),
            origin: ConversationThreadOrigin::Fork {
                parent_thread_id,
                source_turn_id,
                source_message_id,
                kind,
            },
            fork_depth,
        };
        Self::restore(lineage, owner_thread_id)
    }

    /// Rehydrates lineage after checking every self-contained ancestry rule.
    ///
    /// Linked source ownership and role are revalidated by the atomic store
    /// transaction. Use [`Self::validate_source_message_role`] for the latter.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for an impossible owner, root, parent, or
    /// depth relationship.
    pub fn restore(lineage: Self, owner_thread_id: &ThreadId) -> Result<Self, WorkspaceError> {
        match &lineage.origin {
            ConversationThreadOrigin::Original
                if lineage.root_thread_id == *owner_thread_id && lineage.fork_depth == 0 => {}
            ConversationThreadOrigin::Fork {
                parent_thread_id, ..
            } if parent_thread_id != owner_thread_id
                && lineage.root_thread_id != *owner_thread_id
                && (1..=64).contains(&lineage.fork_depth)
                && ((lineage.fork_depth == 1) == (parent_thread_id == &lineage.root_thread_id)) => {
            }
            ConversationThreadOrigin::Original | ConversationThreadOrigin::Fork { .. } => {
                return Err(invalid_lineage("thread.lineage"));
            }
        }
        Ok(lineage)
    }

    /// Verifies the linked source message has the role required by the fork.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when a Branch or Regenerate does not point to
    /// an assistant, or Edit-and-branch does not point to a user message.
    pub fn validate_source_message_role(
        &self,
        source_message_role: MessageRole,
    ) -> Result<(), WorkspaceError> {
        if let ConversationThreadOrigin::Fork { kind, .. } = self.origin
            && kind.source_message_role() != source_message_role
        {
            return Err(invalid_lineage("thread.source_message_role"));
        }
        Ok(())
    }
}

/// Ordered conversation within a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// Stable thread identifier.
    pub id: ThreadId,
    /// Owning project.
    pub project_id: ProjectId,
    /// User-visible title.
    pub title: String,
    /// Current lifecycle state.
    pub state: ThreadState,
    /// Immutable local ancestry and canonical fork point.
    pub lineage: ConversationThreadLineage,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: UnixMillis,
    /// Last successful update timestamp.
    pub updated_at: UnixMillis,
}

impl Thread {
    /// Creates an open thread.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for an invalid title.
    pub fn new(
        id: ThreadId,
        project_id: ProjectId,
        title: String,
        now: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        validate_text("thread.title", &title, 1, MAX_NAME_BYTES, true)?;
        let lineage = ConversationThreadLineage::original(id.clone());
        Ok(Self {
            id,
            project_id,
            title,
            state: ThreadState::Open,
            lineage,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Creates an open child thread with lineage derived from its parent.
    ///
    /// The source-message role is checked here, while the store must still
    /// prove that the source turn and message belong to the immediate parent.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid text or fork lineage.
    #[allow(clippy::too_many_arguments)]
    pub fn new_fork(
        id: ThreadId,
        project_id: ProjectId,
        title: String,
        parent_thread_id: ThreadId,
        parent_lineage: &ConversationThreadLineage,
        source_turn_id: ConversationTurnId,
        source_message_id: MessageId,
        source_message_role: MessageRole,
        kind: ConversationForkKind,
        now: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        validate_text("thread.title", &title, 1, MAX_NAME_BYTES, true)?;
        let lineage = ConversationThreadLineage::fork(
            &id,
            parent_thread_id,
            parent_lineage,
            source_turn_id,
            source_message_id,
            source_message_role,
            kind,
        )?;
        Ok(Self {
            id,
            project_id,
            title,
            state: ThreadState::Open,
            lineage,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rehydrates a thread after validating metadata, lifecycle, and lineage.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for an invalid title, timestamp, revision,
    /// lifecycle, or ancestry shape.
    pub fn restore(snapshot: Self) -> Result<Self, WorkspaceError> {
        validate_text("thread.title", &snapshot.title, 1, MAX_NAME_BYTES, true)?;
        ConversationThreadLineage::restore(snapshot.lineage.clone(), &snapshot.id)?;
        if snapshot.updated_at < snapshot.created_at
            || (snapshot.revision == 0
                && (snapshot.state != ThreadState::Open
                    || snapshot.updated_at != snapshot.created_at))
            || (snapshot.state == ThreadState::Archived && snapshot.revision == 0)
        {
            return Err(invalid_lineage("thread.persisted_state"));
        }
        Ok(snapshot)
    }

    /// Updates the title of an open thread.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid input, clock regression, or an
    /// archived thread.
    pub fn update(&mut self, title: String, now: UnixMillis) -> Result<(), WorkspaceError> {
        self.ensure_open()?;
        validate_text("thread.title", &title, 1, MAX_NAME_BYTES, true)?;
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.title = title;
        Ok(())
    }

    /// Archives a thread.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for clock regression or an archived thread.
    pub fn archive(&mut self, now: UnixMillis) -> Result<(), WorkspaceError> {
        self.ensure_open()?;
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.state = ThreadState::Archived;
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), WorkspaceError> {
        if self.state == ThreadState::Archived {
            return Err(WorkspaceError::InvalidLifecycle { entity: "thread" });
        }
        Ok(())
    }
}

/// Canonical message author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    /// Product or project instruction.
    System,
    /// Human-authored message.
    User,
    /// Grok-authored message.
    Assistant,
}

/// Message lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageState {
    /// Message content is visible and searchable.
    Active,
    /// Message is a retained tombstone with its content removed.
    Deleted,
}

/// Exact role one child-owned message plays in a forked context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationMessageDerivationKind {
    /// A message copied from the source turn's immutable provider context.
    ContextCopy,
    /// The completed source assistant appended by a pure Branch operation.
    SourceAssistantCopy,
    /// A replacement user prompt authored for Edit-and-branch.
    EditedUser,
}

/// Immutable source record for one canonical conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationMessageDerivation {
    /// An ordinary message that was not created by a fork operation.
    Original,
    /// A child-owned message derived from one exact parent message and turn.
    Fork {
        /// Semantic role of this message in the forked child.
        kind: ConversationMessageDerivationKind,
        /// Parent-owned canonical source message; never the child ID.
        source_message_id: MessageId,
        /// Parent-owned canonical source turn.
        source_turn_id: ConversationTurnId,
        /// One-based position in the frozen source provider context.
        ///
        /// Required for context copies and edited users; absent for the source
        /// assistant appended after that context.
        source_context_sequence: Option<u32>,
    },
}

impl ConversationMessageDerivation {
    /// Creates a validated fork derivation for one child-owned message.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for a self-reference, invalid role, or an
    /// absent, zero, or unexpected source-context position.
    pub fn fork(
        owner_message_id: &MessageId,
        role: MessageRole,
        source_message_id: MessageId,
        source_turn_id: ConversationTurnId,
        source_context_sequence: Option<u32>,
        kind: ConversationMessageDerivationKind,
    ) -> Result<Self, WorkspaceError> {
        Self::restore(
            Self::Fork {
                kind,
                source_message_id,
                source_turn_id,
                source_context_sequence,
            },
            owner_message_id,
            role,
        )
    }

    /// Rehydrates a derivation after validating its self-contained rules.
    ///
    /// The atomic store still proves source ownership, source role/content,
    /// context ordering, and consistency with the enclosing thread fork.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for an impossible source or derivation shape.
    pub fn restore(
        derivation: Self,
        owner_message_id: &MessageId,
        role: MessageRole,
    ) -> Result<Self, WorkspaceError> {
        match &derivation {
            Self::Original => {}
            Self::Fork {
                kind: ConversationMessageDerivationKind::ContextCopy,
                source_message_id,
                source_context_sequence: Some(sequence),
                ..
            } if source_message_id != owner_message_id
                && (1..=MAX_SOURCE_CONTEXT_SEQUENCE).contains(sequence) => {}
            Self::Fork {
                kind: ConversationMessageDerivationKind::SourceAssistantCopy,
                source_message_id,
                source_context_sequence: None,
                ..
            } if source_message_id != owner_message_id && role == MessageRole::Assistant => {}
            Self::Fork {
                kind: ConversationMessageDerivationKind::EditedUser,
                source_message_id,
                source_context_sequence: Some(sequence),
                ..
            } if source_message_id != owner_message_id
                && role == MessageRole::User
                && (1..=MAX_SOURCE_CONTEXT_SEQUENCE).contains(sequence) => {}
            Self::Fork { .. } => return Err(invalid_lineage("message.derivation")),
        }
        Ok(derivation)
    }

    /// Returns whether this message is an ordinary independently editable row.
    #[must_use]
    pub const fn is_original(&self) -> bool {
        matches!(self, Self::Original)
    }
}

/// One canonical ordered conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Stable message identifier.
    pub id: MessageId,
    /// Owning thread.
    pub thread_id: ThreadId,
    /// Thread-local sequence assigned atomically by the store.
    pub sequence: u64,
    /// Author role.
    pub role: MessageRole,
    /// Canonical UTF-8 content. Deleted messages contain an empty string.
    pub content: String,
    /// Current lifecycle state.
    pub state: MessageState,
    /// Immutable source record for original or child-owned fork content.
    pub derivation: ConversationMessageDerivation,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: UnixMillis,
    /// Last successful update timestamp.
    pub updated_at: UnixMillis,
}

impl Message {
    /// Creates an unsequenced active message for atomic insertion by a store.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for empty, oversized, or invalid content.
    pub fn new(
        id: MessageId,
        thread_id: ThreadId,
        role: MessageRole,
        content: String,
        now: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        validate_text("message.content", &content, 1, MAX_MESSAGE_BYTES, false)?;
        Ok(Self {
            id,
            thread_id,
            sequence: 0,
            role,
            content,
            state: MessageState::Active,
            derivation: ConversationMessageDerivation::Original,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Creates one sequenced child-owned message with immutable derivation.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for zero sequence, invalid content, a source
    /// self-reference, an invalid role, or an invalid source-context position.
    #[allow(clippy::too_many_arguments)]
    pub fn new_derived(
        id: MessageId,
        thread_id: ThreadId,
        sequence: u64,
        role: MessageRole,
        content: String,
        source_message_id: MessageId,
        source_turn_id: ConversationTurnId,
        source_context_sequence: Option<u32>,
        kind: ConversationMessageDerivationKind,
        now: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        if sequence == 0 {
            return Err(invalid_lineage("message.sequence"));
        }
        validate_text("message.content", &content, 1, MAX_MESSAGE_BYTES, false)?;
        let derivation = ConversationMessageDerivation::fork(
            &id,
            role,
            source_message_id,
            source_turn_id,
            source_context_sequence,
            kind,
        )?;
        Ok(Self {
            id,
            thread_id,
            sequence,
            role,
            content,
            state: MessageState::Active,
            derivation,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rehydrates a persisted message after validating lifecycle and derivation.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid content, sequence, timestamps,
    /// lifecycle, or derivation metadata.
    pub fn restore(snapshot: Self) -> Result<Self, WorkspaceError> {
        if snapshot.sequence == 0 || snapshot.updated_at < snapshot.created_at {
            return Err(invalid_lineage("message.persisted_state"));
        }
        ConversationMessageDerivation::restore(
            snapshot.derivation.clone(),
            &snapshot.id,
            snapshot.role,
        )?;
        match snapshot.state {
            MessageState::Active => {
                validate_text(
                    "message.content",
                    &snapshot.content,
                    1,
                    MAX_MESSAGE_BYTES,
                    false,
                )?;
                if snapshot.revision == 0 && snapshot.updated_at != snapshot.created_at {
                    return Err(invalid_lineage("message.persisted_state"));
                }
            }
            MessageState::Deleted => {
                if !snapshot.content.is_empty()
                    || snapshot.revision == 0
                    || !snapshot.derivation.is_original()
                {
                    return Err(invalid_lineage("message.persisted_state"));
                }
            }
        }
        if !snapshot.derivation.is_original()
            && (snapshot.state != MessageState::Active
                || snapshot.revision != 0
                || snapshot.updated_at != snapshot.created_at)
        {
            return Err(invalid_lineage("message.persisted_state"));
        }
        Ok(snapshot)
    }

    /// Edits an active message without changing its role or order.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid content, clock regression, or a
    /// deleted message.
    pub fn update(&mut self, content: String, now: UnixMillis) -> Result<(), WorkspaceError> {
        self.ensure_active()?;
        self.ensure_original()?;
        validate_text("message.content", &content, 1, MAX_MESSAGE_BYTES, false)?;
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.content = content;
        Ok(())
    }

    /// Replaces content with a durable tombstone.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for clock regression or a deleted message.
    pub fn delete(&mut self, now: UnixMillis) -> Result<(), WorkspaceError> {
        self.ensure_active()?;
        self.ensure_original()?;
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.content.clear();
        self.state = MessageState::Deleted;
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), WorkspaceError> {
        if self.state == MessageState::Deleted {
            return Err(WorkspaceError::InvalidLifecycle { entity: "message" });
        }
        Ok(())
    }

    fn ensure_original(&self) -> Result<(), WorkspaceError> {
        if !self.derivation.is_original() {
            return Err(WorkspaceError::InvalidLifecycle {
                entity: "derived message",
            });
        }
        Ok(())
    }
}

/// Artifact lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    /// An import intent exists but no content is available to consumers.
    Unavailable,
    /// Artifact metadata points to an available local object.
    Available,
    /// Artifact metadata is retained after deletion.
    Deleted,
}

/// Bounded public metadata for the artifact's current immutable content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactContentSummary {
    /// Monotonic artifact-local version.
    pub content_version: u32,
    /// Non-secret media type.
    pub media_type: String,
    /// Stored byte count.
    pub byte_size: u64,
}

impl ArtifactContentSummary {
    /// Creates a validated current-content summary.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for a zero or oversized version or invalid
    /// media type.
    pub fn new(
        content_version: u32,
        media_type: String,
        byte_size: u64,
    ) -> Result<Self, WorkspaceError> {
        validate_content_version(content_version)?;
        validate_artifact_media_type(&media_type)?;
        Ok(Self {
            content_version,
            media_type,
            byte_size,
        })
    }

    fn validate(&self) -> Result<(), WorkspaceError> {
        validate_content_version(self.content_version)?;
        validate_artifact_media_type(&self.media_type)
    }
}

/// One immutable, content-addressed artifact version.
///
/// Storage location is deliberately absent. Infrastructure derives private
/// object identity from the artifact ID, version, and digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactVersion {
    /// Owning artifact.
    pub artifact_id: ArtifactId,
    /// Monotonic artifact-local version.
    pub version: u32,
    /// SHA-256 of the exact stored bytes.
    pub sha256: [u8; 32],
    /// Non-secret media type.
    pub media_type: String,
    /// Exact stored byte count.
    pub byte_size: u64,
    /// Time this immutable version was created.
    pub created_at: UnixMillis,
}

impl ArtifactVersion {
    /// Creates one validated immutable content version.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for a zero or oversized version or invalid
    /// media type.
    pub fn new(
        artifact_id: ArtifactId,
        version: u32,
        sha256: [u8; 32],
        media_type: String,
        byte_size: u64,
        created_at: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        let snapshot = Self {
            artifact_id,
            version,
            sha256,
            media_type,
            byte_size,
            created_at,
        };
        Self::restore(snapshot)
    }

    /// Rehydrates an immutable version after validating bounded metadata.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid persisted metadata.
    pub fn restore(snapshot: Self) -> Result<Self, WorkspaceError> {
        validate_content_version(snapshot.version)?;
        validate_artifact_media_type(&snapshot.media_type)?;
        Ok(snapshot)
    }

    /// Projects the version into public current-content metadata.
    #[must_use]
    pub fn summary(&self) -> ArtifactContentSummary {
        ArtifactContentSummary {
            content_version: self.version,
            media_type: self.media_type.clone(),
            byte_size: self.byte_size,
        }
    }
}

/// Durable metadata for a file or generated asset owned by a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Stable artifact identifier.
    pub id: ArtifactId,
    /// Owning project.
    pub project_id: ProjectId,
    /// Optional conversation association.
    pub thread_id: Option<ThreadId>,
    /// User-visible file name. Restored legacy names remain readable, while
    /// new imports use [`validate_imported_file_name`].
    pub name: String,
    /// Current content metadata, absent while unavailable or after deletion.
    pub content: Option<ArtifactContentSummary>,
    /// Current lifecycle state.
    pub state: ArtifactState,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: UnixMillis,
    /// Last successful update timestamp.
    pub updated_at: UnixMillis,
}

impl Artifact {
    /// Reserves unavailable metadata for a new portable file import.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for an invalid portable file name.
    pub fn new_unavailable(
        id: ArtifactId,
        project_id: ProjectId,
        thread_id: Option<ThreadId>,
        name: String,
        now: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        validate_imported_file_name(&name)?;
        Ok(Self {
            id,
            project_id,
            thread_id,
            name,
            content: None,
            state: ArtifactState::Unavailable,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rehydrates artifact metadata after validating its exact persisted shape.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for an invalid legacy display name,
    /// timestamp, lifecycle, revision, or content summary.
    pub fn restore(snapshot: Self) -> Result<Self, WorkspaceError> {
        validate_artifact_display_name(&snapshot.name)?;
        if snapshot.updated_at < snapshot.created_at {
            return Err(invalid_artifact_state());
        }
        if let Some(content) = &snapshot.content {
            content.validate()?;
        }
        let valid = match (&snapshot.state, &snapshot.content) {
            (ArtifactState::Unavailable, None) => {
                snapshot.revision == 0 && snapshot.updated_at == snapshot.created_at
            }
            (ArtifactState::Available, Some(content)) => {
                snapshot.revision == u64::from(content.content_version)
            }
            (ArtifactState::Deleted, None) => snapshot.revision > 0,
            (ArtifactState::Unavailable | ArtifactState::Deleted, Some(_))
            | (ArtifactState::Available, None) => false,
        };
        if !valid {
            return Err(invalid_artifact_state());
        }
        Ok(snapshot)
    }

    /// Records the next immutable version after its bytes are durably published.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] unless the version is the exact successor,
    /// metadata is valid, time is monotonic, and the artifact is not deleted.
    pub fn record_content(
        &mut self,
        content: ArtifactContentSummary,
        now: UnixMillis,
    ) -> Result<(), WorkspaceError> {
        if self.state == ArtifactState::Deleted {
            return Err(WorkspaceError::InvalidLifecycle { entity: "artifact" });
        }
        content.validate()?;
        let expected_version = self.content.as_ref().map_or(Ok(1), |current| {
            current
                .content_version
                .checked_add(1)
                .filter(|version| *version <= MAX_ARTIFACT_CONTENT_VERSION)
                .ok_or(WorkspaceError::RevisionExhausted)
        })?;
        if content.content_version != expected_version {
            return Err(WorkspaceError::InvalidField {
                field: "artifact.content_version",
                reason: "must be the exact next version",
            });
        }
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.content = Some(content);
        self.state = ArtifactState::Available;
        Ok(())
    }

    /// Replaces an available artifact with a metadata tombstone.
    ///
    /// Immutable version metadata and byte-retention accounting are owned by
    /// the application removal journal; this transition only revokes current
    /// content access.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] unless exact content is currently available
    /// and the artifact clock/revision can advance.
    pub fn remove(&mut self, now: UnixMillis) -> Result<(), WorkspaceError> {
        if self.state != ArtifactState::Available || self.content.is_none() {
            return Err(WorkspaceError::InvalidLifecycle { entity: "artifact" });
        }
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.content = None;
        self.state = ArtifactState::Deleted;
        Ok(())
    }
}

/// Behavior when the application was unavailable at a scheduled time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissedRunPolicy {
    /// Execute one catch-up run after startup.
    RunOnce,
    /// Do not execute missed occurrences.
    Skip,
}

/// Behavior when a scheduled occurrence overlaps an active run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlapPolicy {
    /// Retain at most one pending occurrence.
    QueueOne,
    /// Drop overlapping occurrences.
    Skip,
}

/// Automation lifecycle and enabled state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationState {
    /// Scheduler may create runs when scheduling is implemented.
    Enabled,
    /// Definition is editable but will not schedule runs.
    Disabled,
    /// Definition remains readable but is no longer editable.
    Archived,
}

/// Durable automation definition. Scheduling execution is intentionally outside
/// this model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Automation {
    /// Stable automation identifier.
    pub id: AutomationId,
    /// Owning project.
    pub project_id: ProjectId,
    /// User-visible title.
    pub title: String,
    /// Prompt used by a future scheduler.
    pub prompt: String,
    /// Canonical daemon-owned v1 schedule expression.
    pub schedule: String,
    /// IANA-style timezone identifier or `UTC`.
    pub timezone: String,
    /// Missed occurrence behavior.
    pub missed_run_policy: MissedRunPolicy,
    /// Overlapping occurrence behavior.
    pub overlap_policy: OverlapPolicy,
    /// Current enabled and lifecycle state.
    pub state: AutomationState,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: UnixMillis,
    /// Last successful update timestamp.
    pub updated_at: UnixMillis,
}

impl Automation {
    /// Creates an automation definition without starting a scheduler.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid text, schedule, or timezone.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: AutomationId,
        project_id: ProjectId,
        title: String,
        prompt: String,
        schedule: String,
        timezone: String,
        missed_run_policy: MissedRunPolicy,
        overlap_policy: OverlapPolicy,
        enabled: bool,
        now: UnixMillis,
    ) -> Result<Self, WorkspaceError> {
        let schedule = normalize_automation(&title, &prompt, schedule, &timezone)?;
        Ok(Self {
            id,
            project_id,
            title,
            prompt,
            schedule,
            timezone,
            missed_run_policy,
            overlap_policy,
            state: if enabled {
                AutomationState::Enabled
            } else {
                AutomationState::Disabled
            },
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Updates an active automation definition and its enabled state.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid fields, clock regression, or an
    /// archived definition.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        title: String,
        prompt: String,
        schedule: String,
        timezone: String,
        missed_run_policy: MissedRunPolicy,
        overlap_policy: OverlapPolicy,
        enabled: bool,
        now: UnixMillis,
    ) -> Result<(), WorkspaceError> {
        if self.state == AutomationState::Archived {
            return Err(WorkspaceError::InvalidLifecycle {
                entity: "automation",
            });
        }
        let schedule = normalize_automation(&title, &prompt, schedule, &timezone)?;
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.title = title;
        self.prompt = prompt;
        self.schedule = schedule;
        self.timezone = timezone;
        self.missed_run_policy = missed_run_policy;
        self.overlap_policy = overlap_policy;
        self.state = if enabled {
            AutomationState::Enabled
        } else {
            AutomationState::Disabled
        };
        Ok(())
    }

    /// Archives an automation definition.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for clock regression or an archived definition.
    pub fn archive(&mut self, now: UnixMillis) -> Result<(), WorkspaceError> {
        if self.state == AutomationState::Archived {
            return Err(WorkspaceError::InvalidLifecycle {
                entity: "automation",
            });
        }
        touch(&mut self.revision, &mut self.updated_at, now)?;
        self.state = AutomationState::Archived;
        Ok(())
    }

    /// Rehydrates an automation after validating canonical schedule and lifecycle state.
    ///
    /// Compatibility schedule formats are accepted only by [`Self::new`] and [`Self::update`]
    /// for normalization. Persisted rows must already contain canonical v1 text.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for malformed fields, a non-canonical schedule, timestamp
    /// regression, or an unreachable lifecycle/revision shape.
    pub fn restore(snapshot: Self) -> Result<Self, WorkspaceError> {
        validate_automation_text(&snapshot.title, &snapshot.prompt, &snapshot.timezone)?;
        AutomationSchedule::parse_canonical(&snapshot.schedule)
            .map_err(|_| invalid_automation_schedule())?;
        if snapshot.updated_at < snapshot.created_at
            || (snapshot.revision == 0
                && (snapshot.updated_at != snapshot.created_at
                    || snapshot.state == AutomationState::Archived))
        {
            return Err(WorkspaceError::InvalidField {
                field: "automation.persisted_state",
                reason: "has invalid lifecycle, revision, or timestamps",
            });
        }
        Ok(snapshot)
    }
}

/// Recorded result of one scheduled automation occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationHistoryStatus {
    /// A run completed successfully.
    Succeeded,
    /// A run completed with a known failure.
    Failed,
    /// Missed-run policy skipped the occurrence.
    SkippedMissed,
    /// Overlap policy skipped the occurrence.
    SkippedOverlap,
}

/// Ordered, append-only automation history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationHistoryEntry {
    /// Owning automation.
    pub automation_id: AutomationId,
    /// Automation-local sequence assigned by the store.
    pub sequence: u64,
    /// Intended schedule time.
    pub scheduled_for: UnixMillis,
    /// Time the result was recorded.
    pub recorded_at: UnixMillis,
    /// Stable result class.
    pub status: AutomationHistoryStatus,
    /// Bounded, non-secret user-visible summary.
    pub summary: String,
}

impl AutomationHistoryEntry {
    /// Creates an unsequenced history entry for atomic insertion by a store.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for invalid summary or timestamps.
    pub fn new(
        automation_id: AutomationId,
        scheduled_for: UnixMillis,
        recorded_at: UnixMillis,
        status: AutomationHistoryStatus,
        summary: String,
    ) -> Result<Self, WorkspaceError> {
        validate_text("automation_history.summary", &summary, 0, 1_000, false)?;
        if recorded_at < scheduled_for {
            return Err(WorkspaceError::InvalidField {
                field: "automation_history.recorded_at",
                reason: "must not predate the scheduled time",
            });
        }
        Ok(Self {
            automation_id,
            sequence: 0,
            scheduled_for,
            recorded_at,
            status,
            summary,
        })
    }

    /// Rehydrates a persisted history entry after validating sequence and bounded content.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when sequence is zero, summary is malformed, or the recorded
    /// timestamp predates its scheduled occurrence.
    pub fn restore(snapshot: Self) -> Result<Self, WorkspaceError> {
        if snapshot.sequence == 0
            || snapshot.recorded_at < snapshot.scheduled_for
            || validate_text(
                "automation_history.summary",
                &snapshot.summary,
                0,
                1_000,
                false,
            )
            .is_err()
        {
            return Err(WorkspaceError::InvalidField {
                field: "automation_history.persisted_state",
                reason: "has invalid sequence, timestamps, or summary",
            });
        }
        Ok(snapshot)
    }
}

/// Validates a portable basename for content newly imported into Grok Desktop.
///
/// This is intentionally stricter than validation used when restoring legacy
/// display names. It rejects separators, Windows-forbidden characters and
/// device names, and names whose trailing characters are rewritten by Windows.
///
/// # Errors
///
/// Returns [`WorkspaceError`] when `name` is not a portable UTF-8 basename.
pub fn validate_imported_file_name(name: &str) -> Result<(), WorkspaceError> {
    validate_artifact_display_name(name)?;
    if name != name.trim()
        || name.ends_with(['.', ' '])
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|character| "<>:\"/\\|?*".contains(character))
        || is_windows_device_name(name)
    {
        return Err(WorkspaceError::InvalidField {
            field: "artifact.name",
            reason: "must be a portable file name",
        });
    }
    Ok(())
}

fn validate_artifact_display_name(name: &str) -> Result<(), WorkspaceError> {
    validate_text("artifact.name", name, 1, MAX_NAME_BYTES, true)
}

fn validate_artifact_media_type(media_type: &str) -> Result<(), WorkspaceError> {
    validate_text("artifact.media_type", media_type, 1, 255, true)
}

fn validate_content_version(version: u32) -> Result<(), WorkspaceError> {
    if !(1..=MAX_ARTIFACT_CONTENT_VERSION).contains(&version) {
        return Err(WorkspaceError::InvalidField {
            field: "artifact.content_version",
            reason: "must be between 1 and 1000000",
        });
    }
    Ok(())
}

fn is_windows_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(['.', ' ']);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|suffix| matches!(suffix.as_bytes(), [b'1'..=b'9']))
}

fn invalid_artifact_state() -> WorkspaceError {
    WorkspaceError::InvalidField {
        field: "artifact.persisted_state",
        reason: "has invalid lifecycle, revision, or content metadata",
    }
}

fn normalize_automation(
    title: &str,
    prompt: &str,
    schedule: String,
    timezone: &str,
) -> Result<String, WorkspaceError> {
    validate_automation_text(title, prompt, timezone)?;
    let normalized = AutomationSchedule::parse_for_normalization(&schedule, timezone)
        .map(AutomationSchedule::to_canonical_string)
        .map_err(|_| invalid_automation_schedule());
    drop(schedule);
    normalized
}

fn validate_automation_text(
    title: &str,
    prompt: &str,
    timezone: &str,
) -> Result<(), WorkspaceError> {
    validate_text("automation.title", title, 1, MAX_NAME_BYTES, true)?;
    validate_text("automation.prompt", prompt, 1, MAX_PROMPT_BYTES, false)?;
    validate_text("automation.timezone", timezone, 1, 128, true)?;
    if !timezone
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_+-/".contains(character))
        || timezone.parse::<chrono_tz::Tz>().is_err()
    {
        return Err(WorkspaceError::InvalidField {
            field: "automation.timezone",
            reason: "must be an IANA-style timezone identifier",
        });
    }
    Ok(())
}

const fn invalid_automation_schedule() -> WorkspaceError {
    WorkspaceError::InvalidField {
        field: "automation.schedule",
        reason: "must use a supported canonical schedule",
    }
}

fn validate_text(
    field: &'static str,
    value: &str,
    minimum: usize,
    maximum: usize,
    single_line: bool,
) -> Result<(), WorkspaceError> {
    if value.trim().len() < minimum {
        return Err(WorkspaceError::InvalidField {
            field,
            reason: "is required",
        });
    }
    if value.len() > maximum {
        return Err(WorkspaceError::InvalidField {
            field,
            reason: "exceeds the size limit",
        });
    }
    if value.chars().any(|character| {
        character == '\0'
            || (single_line && character.is_control())
            || (!single_line && character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(WorkspaceError::InvalidField {
            field,
            reason: "contains unsupported control characters",
        });
    }
    Ok(())
}

fn invalid_lineage(field: &'static str) -> WorkspaceError {
    WorkspaceError::InvalidField {
        field,
        reason: "has invalid immutable lineage",
    }
}

fn touch(
    revision: &mut u64,
    updated_at: &mut UnixMillis,
    now: UnixMillis,
) -> Result<(), WorkspaceError> {
    if now < *updated_at {
        return Err(WorkspaceError::ClockRegression);
    }
    *revision = revision
        .checked_add(1)
        .ok_or(WorkspaceError::RevisionExhausted)?;
    *updated_at = now;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_id(value: &str) -> ThreadId {
        ThreadId::new(value).expect("thread id")
    }

    fn message_id(value: &str) -> MessageId {
        MessageId::new(value).expect("message id")
    }

    fn turn_id(value: &str) -> ConversationTurnId {
        ConversationTurnId::new(value).expect("turn id")
    }

    fn project_id() -> ProjectId {
        ProjectId::new("project-1").expect("project id")
    }

    #[test]
    fn ordinary_threads_and_messages_default_to_original_lineage() {
        let thread =
            Thread::new(thread_id("thread-root"), project_id(), "Root".into(), 10).expect("thread");
        assert_eq!(
            thread.lineage,
            ConversationThreadLineage {
                root_thread_id: thread.id.clone(),
                origin: ConversationThreadOrigin::Original,
                fork_depth: 0,
            }
        );
        assert_eq!(Thread::restore(thread.clone()), Ok(thread.clone()));

        let message = Message::new(
            message_id("message-original"),
            thread.id,
            MessageRole::User,
            "hello".into(),
            10,
        )
        .expect("message");
        assert_eq!(message.sequence, 0);
        assert_eq!(message.derivation, ConversationMessageDerivation::Original);
    }

    #[test]
    fn fork_lineage_derives_the_root_depth_and_exact_source_kind() {
        let root =
            Thread::new(thread_id("thread-root"), project_id(), "Root".into(), 10).expect("root");
        let branch = Thread::new_fork(
            thread_id("thread-branch"),
            project_id(),
            "Root".into(),
            root.id.clone(),
            &root.lineage,
            turn_id("turn-completed"),
            message_id("message-assistant"),
            MessageRole::Assistant,
            ConversationForkKind::Branch,
            11,
        )
        .expect("branch");
        assert_eq!(branch.lineage.root_thread_id, root.id);
        assert_eq!(branch.lineage.fork_depth, 1);
        assert!(matches!(
            branch.lineage.origin,
            ConversationThreadOrigin::Fork {
                kind: ConversationForkKind::Branch,
                ..
            }
        ));
        assert_eq!(Thread::restore(branch.clone()), Ok(branch.clone()));

        let edited = Thread::new_fork(
            thread_id("thread-edited"),
            project_id(),
            "Root".into(),
            branch.id.clone(),
            &branch.lineage,
            turn_id("turn-edit-source"),
            message_id("message-user"),
            MessageRole::User,
            ConversationForkKind::EditAndBranch,
            12,
        )
        .expect("edited child");
        assert_eq!(edited.lineage.root_thread_id, branch.lineage.root_thread_id);
        assert_eq!(edited.lineage.fork_depth, 2);
        edited
            .lineage
            .validate_source_message_role(MessageRole::User)
            .expect("edit source role");
        assert!(
            edited
                .lineage
                .validate_source_message_role(MessageRole::Assistant)
                .is_err()
        );

        assert!(
            Thread::new_fork(
                thread_id("thread-invalid-regenerate"),
                project_id(),
                "Root".into(),
                branch.id.clone(),
                &branch.lineage,
                turn_id("turn-regenerate-source"),
                message_id("message-user-wrong-role"),
                MessageRole::User,
                ConversationForkKind::Regenerate,
                12,
            )
            .is_err()
        );
    }

    #[test]
    fn thread_fork_depth_is_bounded_and_structural_forgery_is_rejected() {
        let root =
            Thread::new(thread_id("thread-0"), project_id(), "Root".into(), 10).expect("root");
        let mut parent = root;
        for depth in 1..=64_u8 {
            let child = Thread::new_fork(
                thread_id(&format!("thread-{depth}")),
                project_id(),
                "Root".into(),
                parent.id.clone(),
                &parent.lineage,
                turn_id(&format!("turn-{depth}")),
                message_id(&format!("message-{depth}")),
                MessageRole::Assistant,
                ConversationForkKind::Branch,
                10 + u64::from(depth),
            )
            .expect("bounded child");
            assert_eq!(child.lineage.fork_depth, depth);
            parent = child;
        }
        assert!(
            Thread::new_fork(
                thread_id("thread-65"),
                project_id(),
                "Root".into(),
                parent.id.clone(),
                &parent.lineage,
                turn_id("turn-65"),
                message_id("message-65"),
                MessageRole::Assistant,
                ConversationForkKind::Branch,
                75,
            )
            .is_err()
        );

        let owner = thread_id("forged-owner");
        let forged_original = ConversationThreadLineage {
            root_thread_id: thread_id("other-root"),
            origin: ConversationThreadOrigin::Original,
            fork_depth: 0,
        };
        assert!(ConversationThreadLineage::restore(forged_original, &owner).is_err());

        let forged_depth = ConversationThreadLineage {
            root_thread_id: thread_id("root"),
            origin: ConversationThreadOrigin::Fork {
                parent_thread_id: thread_id("root"),
                source_turn_id: turn_id("source-turn"),
                source_message_id: message_id("source-message"),
                kind: ConversationForkKind::Branch,
            },
            fork_depth: 2,
        };
        assert!(ConversationThreadLineage::restore(forged_depth, &owner).is_err());
    }

    #[test]
    fn thread_metadata_changes_never_change_lineage() {
        let root =
            Thread::new(thread_id("thread-root"), project_id(), "Root".into(), 10).expect("root");
        let mut child = Thread::new_fork(
            thread_id("thread-child"),
            project_id(),
            "Root".into(),
            root.id.clone(),
            &root.lineage,
            turn_id("turn-source"),
            message_id("message-source"),
            MessageRole::Assistant,
            ConversationForkKind::Regenerate,
            11,
        )
        .expect("child");
        let lineage = child.lineage.clone();
        child.update("Renamed".into(), 12).expect("rename");
        child.archive(13).expect("archive");
        assert_eq!(child.lineage, lineage);
        assert_eq!(Thread::restore(child.clone()), Ok(child));
    }

    #[test]
    fn message_derivation_accepts_each_canonical_fork_shape() {
        let context = Message::new_derived(
            message_id("child-context"),
            thread_id("child-thread"),
            1,
            MessageRole::System,
            "system".into(),
            message_id("source-context"),
            turn_id("source-turn"),
            Some(1),
            ConversationMessageDerivationKind::ContextCopy,
            10,
        )
        .expect("context copy");
        assert_eq!(Message::restore(context.clone()), Ok(context));

        let assistant = Message::new_derived(
            message_id("child-assistant"),
            thread_id("child-thread"),
            2,
            MessageRole::Assistant,
            "answer".into(),
            message_id("source-assistant"),
            turn_id("source-turn"),
            None,
            ConversationMessageDerivationKind::SourceAssistantCopy,
            10,
        )
        .expect("assistant copy");
        assert_eq!(Message::restore(assistant.clone()), Ok(assistant));

        let edited = Message::new_derived(
            message_id("child-edited"),
            thread_id("child-thread"),
            2,
            MessageRole::User,
            "edited".into(),
            message_id("source-user"),
            turn_id("source-turn"),
            Some(2),
            ConversationMessageDerivationKind::EditedUser,
            10,
        )
        .expect("edited user");
        assert_eq!(Message::restore(edited.clone()), Ok(edited));
    }

    #[test]
    fn message_derivation_rejects_invalid_source_context_and_roles() {
        for invalid in [
            Message::new_derived(
                message_id("missing-context"),
                thread_id("child-thread"),
                1,
                MessageRole::User,
                "copy".into(),
                message_id("source-1"),
                turn_id("source-turn"),
                None,
                ConversationMessageDerivationKind::ContextCopy,
                10,
            ),
            Message::new_derived(
                message_id("oversized-context"),
                thread_id("child-thread"),
                1,
                MessageRole::User,
                "copy".into(),
                message_id("source-oversized"),
                turn_id("source-turn"),
                Some(MAX_SOURCE_CONTEXT_SEQUENCE + 1),
                ConversationMessageDerivationKind::ContextCopy,
                10,
            ),
            Message::new_derived(
                message_id("zero-context"),
                thread_id("child-thread"),
                1,
                MessageRole::User,
                "copy".into(),
                message_id("source-2"),
                turn_id("source-turn"),
                Some(0),
                ConversationMessageDerivationKind::ContextCopy,
                10,
            ),
            Message::new_derived(
                message_id("assistant-with-context"),
                thread_id("child-thread"),
                1,
                MessageRole::Assistant,
                "copy".into(),
                message_id("source-3"),
                turn_id("source-turn"),
                Some(1),
                ConversationMessageDerivationKind::SourceAssistantCopy,
                10,
            ),
            Message::new_derived(
                message_id("edited-assistant"),
                thread_id("child-thread"),
                1,
                MessageRole::Assistant,
                "edit".into(),
                message_id("source-4"),
                turn_id("source-turn"),
                Some(1),
                ConversationMessageDerivationKind::EditedUser,
                10,
            ),
        ] {
            assert!(invalid.is_err());
        }

        let same_id = message_id("same-message");
        assert!(
            ConversationMessageDerivation::fork(
                &same_id,
                MessageRole::User,
                same_id.clone(),
                turn_id("source-turn"),
                Some(1),
                ConversationMessageDerivationKind::ContextCopy,
            )
            .is_err()
        );
    }

    #[test]
    fn derived_messages_are_immutable_and_restore_rejects_forged_state() {
        let mut derived = Message::new_derived(
            message_id("child-message"),
            thread_id("child-thread"),
            1,
            MessageRole::User,
            "prompt".into(),
            message_id("source-message"),
            turn_id("source-turn"),
            Some(1),
            ConversationMessageDerivationKind::EditedUser,
            10,
        )
        .expect("derived");
        assert!(derived.update("changed".into(), 11).is_err());
        assert!(derived.delete(11).is_err());

        let mut revised = derived.clone();
        revised.revision = 1;
        revised.updated_at = 11;
        assert!(Message::restore(revised).is_err());

        let mut deleted = derived;
        deleted.state = MessageState::Deleted;
        deleted.content.clear();
        deleted.revision = 1;
        deleted.updated_at = 11;
        assert!(Message::restore(deleted).is_err());

        let mut original = Message::new(
            message_id("original-message"),
            thread_id("root-thread"),
            MessageRole::User,
            "prompt".into(),
            10,
        )
        .expect("original");
        original.sequence = 1;
        original
            .update("changed".into(), 11)
            .expect("edit original");
        assert_eq!(Message::restore(original.clone()), Ok(original.clone()));
        original.delete(12).expect("delete original");
        assert_eq!(Message::restore(original.clone()), Ok(original));
    }

    #[test]
    fn lifecycle_and_content_bounds_fail_closed() {
        let mut project = Project::new(
            ProjectId::new("project-1").expect("id"),
            "Research".into(),
            String::new(),
            10,
        )
        .expect("project");
        project.archive(11).expect("archive");
        assert!(project.update("Other".into(), String::new(), 12).is_err());
        assert!(
            Message::new(
                MessageId::new("message-1").expect("id"),
                ThreadId::new("thread-1").expect("id"),
                MessageRole::User,
                "x".repeat(MAX_MESSAGE_BYTES + 1),
                10,
            )
            .is_err()
        );
    }

    #[test]
    fn artifact_names_and_automation_policies_are_explicit() {
        for name in [
            "../secret.txt",
            "folder/file.txt",
            "folder\\file.txt",
            "CON",
            "com1.txt",
            "trailing. ",
            "bad?.txt",
        ] {
            assert!(validate_imported_file_name(name).is_err(), "{name}");
        }
        validate_imported_file_name("Quarterly report (final).pdf").expect("portable name");
        let automation = Automation::new(
            AutomationId::new("automation-1").expect("id"),
            ProjectId::new("project-1").expect("id"),
            "Daily brief".into(),
            "Summarize the project".into(),
            "0 9 * * *".into(),
            "Europe/Paris".into(),
            MissedRunPolicy::RunOnce,
            OverlapPolicy::QueueOne,
            true,
            10,
        )
        .expect("automation");
        assert_eq!(automation.state, AutomationState::Enabled);
        assert_eq!(automation.schedule, "v1;daily;09:00");
        assert_eq!(Automation::restore(automation.clone()), Ok(automation));
        assert!(
            Automation::new(
                AutomationId::new("automation-2").expect("id"),
                ProjectId::new("project-1").expect("id"),
                "Daily brief".into(),
                "Summarize the project".into(),
                "0 9 * * *".into(),
                "Not/A-Timezone".into(),
                MissedRunPolicy::Skip,
                OverlapPolicy::Skip,
                false,
                10,
            )
            .is_err()
        );
    }

    #[test]
    fn automation_restore_requires_canonical_schedule_and_reachable_lifecycle() {
        let mut automation = Automation::new(
            AutomationId::new("automation-restore").expect("id"),
            ProjectId::new("project-1").expect("id"),
            "Weekly brief".into(),
            "Summarize the week".into(),
            r#"{"frequency":"weekly","localTime":"08:30","weekday":1,"timeZoneIana":"UTC"}"#.into(),
            "UTC".into(),
            MissedRunPolicy::Skip,
            OverlapPolicy::QueueOne,
            false,
            10,
        )
        .expect("normalized automation");
        assert_eq!(automation.schedule, "v1;weekly;1;08:30");
        automation
            .update(
                "Monthly brief".into(),
                "Summarize the month".into(),
                "0 9 31 * *".into(),
                "UTC".into(),
                MissedRunPolicy::RunOnce,
                OverlapPolicy::Skip,
                false,
                11,
            )
            .expect("normalized update");
        assert_eq!(automation.schedule, "v1;monthly;31;09:00");
        assert_eq!(
            Automation::restore(automation.clone()),
            Ok(automation.clone())
        );

        let mut corrupt = automation.clone();
        corrupt.schedule = "0 9 31 * *".into();
        assert!(Automation::restore(corrupt).is_err());
        let mut corrupt = automation.clone();
        corrupt.revision = 0;
        assert!(Automation::restore(corrupt).is_err());
        let mut corrupt = automation;
        corrupt.updated_at = corrupt.created_at.saturating_sub(1);
        assert!(Automation::restore(corrupt).is_err());
    }

    #[test]
    fn automation_history_restore_rejects_unsequenced_or_malformed_rows() {
        let mut entry = AutomationHistoryEntry::new(
            AutomationId::new("automation-history").expect("id"),
            100,
            101,
            AutomationHistoryStatus::SkippedMissed,
            "Missed while the scheduler lease was unavailable.".into(),
        )
        .expect("history entry");
        assert!(AutomationHistoryEntry::restore(entry.clone()).is_err());
        entry.sequence = 1;
        assert_eq!(
            AutomationHistoryEntry::restore(entry.clone()),
            Ok(entry.clone())
        );

        let mut corrupt = entry.clone();
        corrupt.recorded_at = 99;
        assert!(AutomationHistoryEntry::restore(corrupt).is_err());
        let mut corrupt = entry.clone();
        corrupt.summary = "bad\0summary".into();
        assert!(AutomationHistoryEntry::restore(corrupt).is_err());
        let mut corrupt = entry;
        corrupt.summary = "x".repeat(1_001);
        assert!(AutomationHistoryEntry::restore(corrupt).is_err());
    }

    #[test]
    fn artifact_content_lifecycle_is_exact_and_path_free() {
        let mut artifact = Artifact::new_unavailable(
            ArtifactId::new("artifact-1").expect("id"),
            ProjectId::new("project-1").expect("id"),
            None,
            "notes.txt".into(),
            10,
        )
        .expect("reservation");
        assert_eq!(Artifact::restore(artifact.clone()), Ok(artifact.clone()));
        assert_eq!(artifact.state, ArtifactState::Unavailable);
        assert!(artifact.content.is_none());

        let version =
            ArtifactVersion::new(artifact.id.clone(), 1, [7; 32], "text/plain".into(), 4, 11)
                .expect("version");
        artifact
            .record_content(version.summary(), 11)
            .expect("publish");
        assert_eq!(artifact.state, ArtifactState::Available);
        assert_eq!(artifact.revision, 1);
        assert_eq!(Artifact::restore(artifact.clone()), Ok(artifact.clone()));

        assert!(artifact.record_content(version.summary(), 12).is_err());
        let next = ArtifactContentSummary::new(2, "text/plain".into(), 8).expect("summary");
        artifact.record_content(next, 12).expect("next version");
        assert_eq!(artifact.revision, 2);
    }

    #[test]
    fn artifact_restore_rejects_forged_state_combinations() {
        let reserved = Artifact::new_unavailable(
            ArtifactId::new("artifact-forged").expect("id"),
            ProjectId::new("project-1").expect("id"),
            None,
            "safe.txt".into(),
            10,
        )
        .expect("reservation");
        let mut forged = reserved.clone();
        forged.state = ArtifactState::Available;
        assert!(Artifact::restore(forged).is_err());

        let mut forged = reserved.clone();
        forged.revision = 1;
        assert!(Artifact::restore(forged).is_err());

        let mut legacy = reserved;
        legacy.name = "legacy:name.txt".into();
        assert_eq!(Artifact::restore(legacy.clone()), Ok(legacy));
        assert!(validate_imported_file_name("legacy:name.txt").is_err());
    }

    #[test]
    fn artifact_removal_is_an_irreversible_revisioned_tombstone() {
        let mut unavailable = Artifact::new_unavailable(
            ArtifactId::new("artifact-remove").expect("id"),
            ProjectId::new("project-1").expect("id"),
            None,
            "remove.txt".into(),
            10,
        )
        .expect("reservation");
        assert_eq!(
            unavailable.remove(11),
            Err(WorkspaceError::InvalidLifecycle { entity: "artifact" })
        );
        let content = ArtifactContentSummary::new(1, "text/plain".into(), 4).expect("content");
        unavailable.record_content(content, 11).expect("available");
        assert_eq!(unavailable.remove(10), Err(WorkspaceError::ClockRegression));

        unavailable.remove(12).expect("remove");
        assert_eq!(unavailable.state, ArtifactState::Deleted);
        assert_eq!(unavailable.revision, 2);
        assert_eq!(unavailable.updated_at, 12);
        assert!(unavailable.content.is_none());
        assert_eq!(
            Artifact::restore(unavailable.clone()),
            Ok(unavailable.clone())
        );
        assert_eq!(
            unavailable.remove(13),
            Err(WorkspaceError::InvalidLifecycle { entity: "artifact" })
        );
        assert!(
            unavailable
                .record_content(
                    ArtifactContentSummary::new(2, "text/plain".into(), 5).expect("content"),
                    13,
                )
                .is_err()
        );
    }

    #[test]
    fn artifact_version_bounds_are_enforced() {
        let id = ArtifactId::new("artifact-version").expect("id");
        assert!(ArtifactVersion::new(id.clone(), 0, [1; 32], "text/plain".into(), 0, 10).is_err());
        assert!(
            ArtifactVersion::new(
                id,
                MAX_ARTIFACT_CONTENT_VERSION + 1,
                [1; 32],
                "text/plain".into(),
                0,
                10,
            )
            .is_err()
        );

        let mut exhausted = Artifact {
            id: ArtifactId::new("artifact-exhausted").expect("id"),
            project_id: ProjectId::new("project-1").expect("id"),
            thread_id: None,
            name: "full.bin".into(),
            content: Some(
                ArtifactContentSummary::new(
                    MAX_ARTIFACT_CONTENT_VERSION,
                    "application/octet-stream".into(),
                    1,
                )
                .expect("summary"),
            ),
            state: ArtifactState::Available,
            revision: u64::from(MAX_ARTIFACT_CONTENT_VERSION),
            created_at: 10,
            updated_at: 10,
        };
        assert_eq!(
            exhausted.record_content(
                ArtifactContentSummary::new(
                    MAX_ARTIFACT_CONTENT_VERSION,
                    "application/octet-stream".into(),
                    2,
                )
                .expect("summary"),
                11,
            ),
            Err(WorkspaceError::RevisionExhausted)
        );
        assert_eq!(exhausted.revision, u64::from(MAX_ARTIFACT_CONTENT_VERSION));
    }

    #[test]
    fn revision_exhaustion_never_reports_a_successful_update() {
        let mut project = Project::new(
            ProjectId::new("project-1").expect("id"),
            "Research".into(),
            String::new(),
            10,
        )
        .expect("project");
        project.revision = u64::MAX;
        assert_eq!(project.archive(11), Err(WorkspaceError::RevisionExhausted));
        assert_eq!(project.state, ProjectState::Active);
    }
}
