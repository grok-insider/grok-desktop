use std::sync::Arc;

use crate::{
    ApplicationError, Clock, IdGenerator, WorkspaceSearchHit, WorkspaceStore,
    mutations::mutation_command,
};
use grok_domain::{
    Automation, AutomationHistoryEntry, AutomationHistoryStatus, AutomationId, AutomationState,
    Message, MessageId, MessageRole, MissedRunPolicy, OverlapPolicy, Project, ProjectId,
    ProjectState, Thread, ThreadId, ThreadState,
};

const MAX_PAGE_SIZE: usize = 200;
const MAX_SEARCH_PAGE_SIZE: usize = 100;
const MAX_SEARCH_OFFSET: usize = 10_000;

/// One bounded keyset page of canonical entities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// Ordered page contents.
    pub items: Vec<T>,
    /// Last returned entity ID when another page exists.
    pub next_cursor: Option<String>,
}

/// Project creation input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProject {
    /// User-visible name.
    pub name: String,
    /// Optional description.
    pub description: String,
}

/// Project metadata update input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProject {
    /// Project identifier.
    pub id: String,
    /// Revision observed by the caller.
    pub expected_revision: u64,
    /// User-visible name.
    pub name: String,
    /// Optional description.
    pub description: String,
}

/// Thread creation input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateThread {
    /// Owning project.
    pub project_id: String,
    /// User-visible title.
    pub title: String,
}

/// Thread title update input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateThread {
    /// Thread identifier.
    pub id: String,
    /// Revision observed by the caller.
    pub expected_revision: u64,
    /// User-visible title.
    pub title: String,
}

/// Canonical message creation input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateMessage {
    /// Owning thread.
    pub thread_id: String,
    /// Canonical author role.
    pub role: MessageRole,
    /// Canonical UTF-8 content.
    pub content: String,
}

/// Canonical message edit input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateMessage {
    /// Message identifier.
    pub id: String,
    /// Revision observed by the caller.
    pub expected_revision: u64,
    /// Replacement content.
    pub content: String,
}

/// Automation creation input. It stores policy only and never starts execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAutomation {
    /// Owning project.
    pub project_id: String,
    /// User-visible title.
    pub title: String,
    /// Future-run prompt.
    pub prompt: String,
    /// Opaque schedule expression.
    pub schedule: String,
    /// IANA-style timezone identifier.
    pub timezone: String,
    /// Missed occurrence behavior.
    pub missed_run_policy: MissedRunPolicy,
    /// Overlap behavior.
    pub overlap_policy: OverlapPolicy,
    /// Requested enabled state. The daemon arms this only when the scheduler
    /// kernel is live (`schedule_active` + execution-enabled health).
    pub enabled: bool,
}

/// Automation definition update input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAutomation {
    /// Automation identifier.
    pub id: String,
    /// Revision observed by the caller.
    pub expected_revision: u64,
    /// User-visible title.
    pub title: String,
    /// Future-run prompt.
    pub prompt: String,
    /// Opaque schedule expression.
    pub schedule: String,
    /// IANA-style timezone identifier.
    pub timezone: String,
    /// Missed occurrence behavior.
    pub missed_run_policy: MissedRunPolicy,
    /// Overlap behavior.
    pub overlap_policy: OverlapPolicy,
    /// Requested enabled state. The daemon arms this only when the scheduler
    /// kernel is live (`schedule_active` + execution-enabled health).
    pub enabled: bool,
}

/// Transport-independent workspace use cases.
pub struct WorkspaceService {
    store: Arc<dyn WorkspaceStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

#[allow(clippy::missing_errors_doc)]
impl WorkspaceService {
    /// Creates a workspace service.
    #[must_use]
    pub fn new(
        store: Arc<dyn WorkspaceStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { store, clock, ids }
    }

    /// Creates a project idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for invalid input or persistence failure.
    pub async fn create_project(
        &self,
        input: CreateProject,
        idempotency_key: &str,
    ) -> Result<Project, ApplicationError> {
        let command = mutation_command(
            "create_project",
            idempotency_key,
            &[input.name.clone(), input.description.clone()],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("create_project", &command)
            .await?
        {
            return self.get_project(&ProjectId::new(id)?).await;
        }
        let project = Project::new(
            ProjectId::new(self.ids.generate("project"))?,
            input.name,
            input.description,
            self.clock.now(),
        )?;
        Ok(self.store.create_project(project, &command).await?)
    }

    /// Loads one project.
    pub async fn get_project(&self, id: &ProjectId) -> Result<Project, ApplicationError> {
        Ok(self.store.get_project(id).await?)
    }

    /// Updates project metadata using optimistic concurrency.
    pub async fn update_project(
        &self,
        input: UpdateProject,
        idempotency_key: &str,
    ) -> Result<Project, ApplicationError> {
        let command = mutation_command(
            "update_project",
            idempotency_key,
            &[
                input.id.clone(),
                input.expected_revision.to_string(),
                input.name.clone(),
                input.description.clone(),
            ],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("update_project", &command)
            .await?
        {
            return self.get_project(&ProjectId::new(id)?).await;
        }
        let id = ProjectId::new(input.id)?;
        let mut project = self.store.get_project(&id).await?;
        ensure_revision(project.revision, input.expected_revision)?;
        project.update(input.name, input.description, self.clock.now())?;
        self.store
            .save_project(project.clone(), input.expected_revision, &command)
            .await?;
        Ok(project)
    }

    /// Archives a project using optimistic concurrency.
    pub async fn archive_project(
        &self,
        id: &ProjectId,
        expected_revision: u64,
        idempotency_key: &str,
    ) -> Result<Project, ApplicationError> {
        let command = mutation_command(
            "archive_project",
            idempotency_key,
            &[id.as_str().into(), expected_revision.to_string()],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("archive_project", &command)
            .await?
        {
            return self.get_project(&ProjectId::new(id)?).await;
        }
        let mut project = self.store.get_project(id).await?;
        ensure_revision(project.revision, expected_revision)?;
        project.archive(self.clock.now())?;
        self.store
            .save_project(project.clone(), expected_revision, &command)
            .await?;
        Ok(project)
    }

    /// Lists projects using a bounded keyset page.
    pub async fn list_projects(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Project>, ApplicationError> {
        validate_page_limit(limit)?;
        let after = cursor.map(ProjectId::new).transpose()?;
        let items = self.store.list_projects(after.as_ref(), limit + 1).await?;
        Ok(entity_page(items, limit, |project| project.id.as_str()))
    }

    /// Creates a thread beneath an active project, idempotently.
    pub async fn create_thread(
        &self,
        input: CreateThread,
        idempotency_key: &str,
    ) -> Result<Thread, ApplicationError> {
        let command = mutation_command(
            "create_thread",
            idempotency_key,
            &[input.project_id.clone(), input.title.clone()],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("create_thread", &command)
            .await?
        {
            return self.get_thread(&ThreadId::new(id)?).await;
        }
        let project_id = ProjectId::new(input.project_id)?;
        ensure_project_active(&self.store.get_project(&project_id).await?)?;
        let thread = Thread::new(
            ThreadId::new(self.ids.generate("thread"))?,
            project_id,
            input.title,
            self.clock.now(),
        )?;
        Ok(self.store.create_thread(thread, &command).await?)
    }

    /// Loads one thread.
    pub async fn get_thread(&self, id: &ThreadId) -> Result<Thread, ApplicationError> {
        Ok(self.store.get_thread(id).await?)
    }

    /// Updates a thread title using optimistic concurrency.
    pub async fn update_thread(
        &self,
        input: UpdateThread,
        idempotency_key: &str,
    ) -> Result<Thread, ApplicationError> {
        let command = mutation_command(
            "update_thread",
            idempotency_key,
            &[
                input.id.clone(),
                input.expected_revision.to_string(),
                input.title.clone(),
            ],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("update_thread", &command)
            .await?
        {
            return self.get_thread(&ThreadId::new(id)?).await;
        }
        let id = ThreadId::new(input.id)?;
        let mut thread = self.store.get_thread(&id).await?;
        ensure_revision(thread.revision, input.expected_revision)?;
        thread.update(input.title, self.clock.now())?;
        self.store
            .save_thread(thread.clone(), input.expected_revision, &command)
            .await?;
        Ok(thread)
    }

    /// Archives a thread using optimistic concurrency.
    pub async fn archive_thread(
        &self,
        id: &ThreadId,
        expected_revision: u64,
        idempotency_key: &str,
    ) -> Result<Thread, ApplicationError> {
        let command = mutation_command(
            "archive_thread",
            idempotency_key,
            &[id.as_str().into(), expected_revision.to_string()],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("archive_thread", &command)
            .await?
        {
            return self.get_thread(&ThreadId::new(id)?).await;
        }
        let mut thread = self.store.get_thread(id).await?;
        ensure_revision(thread.revision, expected_revision)?;
        thread.archive(self.clock.now())?;
        self.store
            .save_thread(thread.clone(), expected_revision, &command)
            .await?;
        Ok(thread)
    }

    /// Lists threads under one project.
    pub async fn list_threads(
        &self,
        project_id: &ProjectId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Thread>, ApplicationError> {
        validate_page_limit(limit)?;
        let after = cursor.map(ThreadId::new).transpose()?;
        let items = self
            .store
            .list_threads(project_id, after.as_ref(), limit + 1)
            .await?;
        Ok(entity_page(items, limit, |thread| thread.id.as_str()))
    }

    /// Appends a canonical message with an atomically assigned sequence.
    pub async fn create_message(
        &self,
        input: CreateMessage,
        idempotency_key: &str,
    ) -> Result<Message, ApplicationError> {
        let command = mutation_command(
            "create_message",
            idempotency_key,
            &[
                input.thread_id.clone(),
                message_role_key(input.role).into(),
                input.content.clone(),
            ],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("create_message", &command)
            .await?
        {
            return self.get_message(&MessageId::new(id)?).await;
        }
        let thread_id = ThreadId::new(input.thread_id)?;
        let thread = self.store.get_thread(&thread_id).await?;
        ensure_thread_open(&thread)?;
        ensure_project_active(&self.store.get_project(&thread.project_id).await?)?;
        let message = Message::new(
            MessageId::new(self.ids.generate("message"))?,
            thread_id,
            input.role,
            input.content,
            self.clock.now(),
        )?;
        Ok(self.store.create_message(message, &command).await?)
    }

    /// Loads one canonical message.
    pub async fn get_message(&self, id: &MessageId) -> Result<Message, ApplicationError> {
        Ok(self.store.get_message(id).await?)
    }

    /// Edits message content using optimistic concurrency.
    pub async fn update_message(
        &self,
        input: UpdateMessage,
        idempotency_key: &str,
    ) -> Result<Message, ApplicationError> {
        let command = mutation_command(
            "update_message",
            idempotency_key,
            &[
                input.id.clone(),
                input.expected_revision.to_string(),
                input.content.clone(),
            ],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("update_message", &command)
            .await?
        {
            return self.get_message(&MessageId::new(id)?).await;
        }
        let id = MessageId::new(input.id)?;
        let mut message = self.store.get_message(&id).await?;
        ensure_revision(message.revision, input.expected_revision)?;
        message.update(input.content, self.clock.now())?;
        self.store
            .save_message(message.clone(), input.expected_revision, &command)
            .await?;
        Ok(message)
    }

    /// Replaces message content with a durable tombstone.
    pub async fn delete_message(
        &self,
        id: &MessageId,
        expected_revision: u64,
        idempotency_key: &str,
    ) -> Result<Message, ApplicationError> {
        let command = mutation_command(
            "delete_message",
            idempotency_key,
            &[id.as_str().into(), expected_revision.to_string()],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("delete_message", &command)
            .await?
        {
            return self.get_message(&MessageId::new(id)?).await;
        }
        let mut message = self.store.get_message(id).await?;
        ensure_revision(message.revision, expected_revision)?;
        message.delete(self.clock.now())?;
        self.store
            .save_message(message.clone(), expected_revision, &command)
            .await?;
        Ok(message)
    }

    /// Lists messages in canonical thread order.
    pub async fn list_messages(
        &self,
        thread_id: &ThreadId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Message>, ApplicationError> {
        validate_page_limit(limit)?;
        let after = cursor.map(MessageId::new).transpose()?;
        let items = self
            .store
            .list_messages(thread_id, after.as_ref(), limit + 1)
            .await?;
        Ok(entity_page(items, limit, |message| message.id.as_str()))
    }

    /// Creates an automation definition without starting execution.
    pub async fn create_automation(
        &self,
        input: CreateAutomation,
        idempotency_key: &str,
    ) -> Result<Automation, ApplicationError> {
        let command = mutation_command(
            "create_automation",
            idempotency_key,
            &[
                input.project_id.clone(),
                input.title.clone(),
                input.prompt.clone(),
                input.schedule.clone(),
                input.timezone.clone(),
                missed_run_key(input.missed_run_policy).into(),
                overlap_key(input.overlap_policy).into(),
                input.enabled.to_string(),
            ],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("create_automation", &command)
            .await?
        {
            return self.get_automation(&AutomationId::new(id)?).await;
        }
        let project_id = ProjectId::new(input.project_id)?;
        ensure_project_active(&self.store.get_project(&project_id).await?)?;
        let automation = Automation::new(
            AutomationId::new(self.ids.generate("automation"))?,
            project_id,
            input.title,
            input.prompt,
            input.schedule,
            input.timezone,
            input.missed_run_policy,
            input.overlap_policy,
            input.enabled,
            self.clock.now(),
        )?;
        Ok(self.store.create_automation(automation, &command).await?)
    }

    /// Loads one automation definition.
    pub async fn get_automation(&self, id: &AutomationId) -> Result<Automation, ApplicationError> {
        Ok(self.store.get_automation(id).await?)
    }

    /// Updates an automation definition using optimistic concurrency.
    pub async fn update_automation(
        &self,
        input: UpdateAutomation,
        idempotency_key: &str,
    ) -> Result<Automation, ApplicationError> {
        let command = mutation_command(
            "update_automation",
            idempotency_key,
            &[
                input.id.clone(),
                input.expected_revision.to_string(),
                input.title.clone(),
                input.prompt.clone(),
                input.schedule.clone(),
                input.timezone.clone(),
                missed_run_key(input.missed_run_policy).into(),
                overlap_key(input.overlap_policy).into(),
                input.enabled.to_string(),
            ],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("update_automation", &command)
            .await?
        {
            return self.get_automation(&AutomationId::new(id)?).await;
        }
        let id = AutomationId::new(input.id)?;
        let mut automation = self.store.get_automation(&id).await?;
        ensure_revision(automation.revision, input.expected_revision)?;
        automation.update(
            input.title,
            input.prompt,
            input.schedule,
            input.timezone,
            input.missed_run_policy,
            input.overlap_policy,
            input.enabled,
            self.clock.now(),
        )?;
        self.store
            .save_automation(automation.clone(), input.expected_revision, &command)
            .await?;
        Ok(automation)
    }

    /// Archives an automation definition.
    pub async fn archive_automation(
        &self,
        id: &AutomationId,
        expected_revision: u64,
        idempotency_key: &str,
    ) -> Result<Automation, ApplicationError> {
        let command = mutation_command(
            "archive_automation",
            idempotency_key,
            &[id.as_str().into(), expected_revision.to_string()],
        )?;
        if let Some(id) = self
            .store
            .resolve_mutation("archive_automation", &command)
            .await?
        {
            return self.get_automation(&AutomationId::new(id)?).await;
        }
        let mut automation = self.store.get_automation(id).await?;
        ensure_revision(automation.revision, expected_revision)?;
        automation.archive(self.clock.now())?;
        self.store
            .save_automation(automation.clone(), expected_revision, &command)
            .await?;
        Ok(automation)
    }

    /// Lists project automation definitions.
    pub async fn list_automations(
        &self,
        project_id: &ProjectId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Automation>, ApplicationError> {
        validate_page_limit(limit)?;
        let after = cursor.map(AutomationId::new).transpose()?;
        let items = self
            .store
            .list_automations(project_id, after.as_ref(), limit + 1)
            .await?;
        Ok(entity_page(items, limit, |automation| {
            automation.id.as_str()
        }))
    }

    /// Records one future scheduler result idempotently by scheduled timestamp.
    pub async fn record_automation_history(
        &self,
        automation_id: AutomationId,
        scheduled_for: u64,
        status: AutomationHistoryStatus,
        summary: String,
    ) -> Result<AutomationHistoryEntry, ApplicationError> {
        let automation = self.store.get_automation(&automation_id).await?;
        if automation.state == AutomationState::Archived {
            return Err(ApplicationError::InvalidState(
                "automation is archived".into(),
            ));
        }
        let entry = AutomationHistoryEntry::new(
            automation_id,
            scheduled_for,
            self.clock.now(),
            status,
            summary,
        )?;
        Ok(self.store.record_automation_history(entry).await?)
    }

    /// Lists ordered automation history.
    pub async fn automation_history(
        &self,
        automation_id: &AutomationId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<AutomationHistoryEntry>, ApplicationError> {
        validate_page_limit(limit)?;
        Ok(self
            .store
            .automation_history(automation_id, after_sequence, limit)
            .await?)
    }

    /// Searches canonical workspace content.
    pub async fn search(
        &self,
        project_id: Option<&ProjectId>,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Page<WorkspaceSearchHit>, ApplicationError> {
        if query.trim().is_empty() || query.len() > 256 || query.chars().any(char::is_control) {
            return Err(ApplicationError::InvalidInput(
                "search query must be printable and between 1 and 256 bytes".into(),
            ));
        }
        if !(1..=MAX_SEARCH_PAGE_SIZE).contains(&limit) || offset > MAX_SEARCH_OFFSET {
            return Err(ApplicationError::InvalidInput(
                "search pagination is outside the supported bounds".into(),
            ));
        }
        let mut items = self
            .store
            .search(project_id, query, offset, limit + 1)
            .await?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_offset = offset.saturating_add(limit);
        Ok(Page {
            items,
            next_cursor: (has_more && next_offset <= MAX_SEARCH_OFFSET)
                .then(|| next_offset.to_string()),
        })
    }
}

const fn message_role_key(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

const fn missed_run_key(policy: MissedRunPolicy) -> &'static str {
    match policy {
        MissedRunPolicy::RunOnce => "run_once",
        MissedRunPolicy::Skip => "skip",
    }
}

const fn overlap_key(policy: OverlapPolicy) -> &'static str {
    match policy {
        OverlapPolicy::QueueOne => "queue_one",
        OverlapPolicy::Skip => "skip",
    }
}

fn validate_page_limit(limit: usize) -> Result<(), ApplicationError> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ApplicationError::InvalidInput(
            "page limit must be between 1 and 200".into(),
        ));
    }
    Ok(())
}

fn ensure_revision(actual: u64, expected: u64) -> Result<(), ApplicationError> {
    if actual != expected {
        return Err(ApplicationError::Conflict);
    }
    Ok(())
}

fn ensure_project_active(project: &Project) -> Result<(), ApplicationError> {
    if project.state != ProjectState::Active {
        return Err(ApplicationError::InvalidState("project is archived".into()));
    }
    Ok(())
}

fn ensure_thread_open(thread: &Thread) -> Result<(), ApplicationError> {
    if thread.state != ThreadState::Open {
        return Err(ApplicationError::InvalidState("thread is archived".into()));
    }
    Ok(())
}

fn entity_page<T>(mut items: Vec<T>, limit: usize, id: impl Fn(&T) -> &str) -> Page<T> {
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = has_more
        .then(|| items.last().map(|item| id(item).to_owned()))
        .flatten();
    Page { items, next_cursor }
}
