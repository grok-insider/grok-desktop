use std::{path::PathBuf, sync::Arc};

use futures_util::StreamExt;
use grok_application::{
    AgentEvent, AgentPrompt, AgentRuntime, AgentRuntimeError, AgentSessionRequest,
    ApplicationError, CreateMessage, CreateRun, ExecutionStore, HostExecutionPolicyStore,
    HostFilesystemReader, HostToolsMcpServer, RunService, WorkspaceService,
};
use grok_domain::{MessageRole, ProjectId, Run, RunState, ThreadId, WorkExecutionBackend};
use grok_host_tools::CapabilityHostFilesystem;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::{HostToolBridge, HostWorkRuntime};

const MAX_WORK_PROMPT_BYTES: usize = 256 * 1024;
const MAX_WORK_RESPONSE_BYTES: usize = 1024 * 1024;

/// Completed first-version Host Work turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostWorkOutcome {
    /// Durable Work run after terminal completion.
    pub run: Run,
    /// Bounded assistant text persisted into the owning thread.
    pub assistant_text: String,
}

/// Daemon composition for one-at-a-time Host Work ACP execution.
pub struct HostWorkService {
    runtime: Arc<HostWorkRuntime>,
    policies: Arc<dyn HostExecutionPolicyStore>,
    executions: Arc<dyn ExecutionStore>,
    runs: Arc<RunService>,
    workspace: Arc<WorkspaceService>,
    endpoint_base: PathBuf,
    active: Semaphore,
}

impl std::fmt::Debug for HostWorkService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostWorkService")
            .finish_non_exhaustive()
    }
}

impl HostWorkService {
    /// Creates a Host Work use case from daemon-owned ports and adapters.
    #[must_use]
    pub fn new(
        runtime: Arc<HostWorkRuntime>,
        policies: Arc<dyn HostExecutionPolicyStore>,
        executions: Arc<dyn ExecutionStore>,
        runs: Arc<RunService>,
        workspace: Arc<WorkspaceService>,
        endpoint_base: PathBuf,
    ) -> Self {
        Self {
            runtime,
            policies,
            executions,
            runs,
            workspace,
            endpoint_base,
            active: Semaphore::new(1),
        }
    }

    /// Runs one bounded prompt through authenticated `HostWorkTools` and its MCP bridge.
    ///
    /// # Errors
    ///
    /// Returns a stable application error when policy, run, helper, ACP, bridge,
    /// filesystem, or persistence readiness fails.
    #[allow(clippy::too_many_lines)]
    pub async fn execute(
        &self,
        project_id: &str,
        thread_id: &str,
        prompt: &str,
        idempotency_key: &str,
    ) -> Result<HostWorkOutcome, ApplicationError> {
        if prompt.trim().is_empty() || prompt.len() > MAX_WORK_PROMPT_BYTES {
            return Err(ApplicationError::InvalidInput(
                "Host Work prompt is invalid".into(),
            ));
        }
        let _permit = self
            .active
            .try_acquire()
            .map_err(|_| ApplicationError::Unavailable("Host Work runtime is busy".into()))?;
        if !self.runtime.is_ready().await {
            return Err(ApplicationError::Unavailable(
                "Host Work runtime is not prepared".into(),
            ));
        }
        let policy = self.policies.get_host_execution_policy().await?;
        if !policy.is_effectively_active() {
            return Err(ApplicationError::InvalidState(
                "Host Tools enrollment is not active".into(),
            ));
        }
        let helper = self.runtime.helper().ok_or_else(|| {
            ApplicationError::Unavailable("Host Tools helper is unavailable".into())
        })?;
        let thread_id = ThreadId::new(thread_id)?;
        let thread = self.workspace.get_thread(&thread_id).await?;
        if thread.project_id != ProjectId::new(project_id)? {
            return Err(ApplicationError::InvalidInput(
                "Host Work thread does not belong to the selected project".into(),
            ));
        }
        let filesystem: Arc<dyn HostFilesystemReader> = Arc::new(
            CapabilityHostFilesystem::open(&policy.canonical_roots)
                .map_err(|error| ApplicationError::Unavailable(error.message))?,
        );
        let run_key = derived_key(idempotency_key, "run");
        let mut run = self
            .runs
            .create_work(
                CreateRun {
                    project_id: project_id.into(),
                    thread_id: thread_id.to_string(),
                },
                WorkExecutionBackend::HostDirect,
                &run_key,
            )
            .await?;
        run = self
            .runs
            .transition(
                &run.id,
                run.revision,
                RunState::Planning,
                &derived_key(idempotency_key, "planning"),
            )
            .await?;
        self.workspace
            .create_message(
                CreateMessage {
                    thread_id: thread_id.to_string(),
                    role: MessageRole::User,
                    content: prompt.into(),
                },
                &derived_key(idempotency_key, "user-message"),
            )
            .await?;
        let bridge = HostToolBridge::start(
            &self.endpoint_base,
            run.id.clone(),
            policy.revision,
            helper.clone(),
            self.policies.clone(),
            self.executions.clone(),
            filesystem,
        )?;
        let roots = policy
            .canonical_roots
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let session = self
            .runtime
            .open_session(AgentSessionRequest {
                working_directory: roots[0].clone(),
                additional_directories: roots.iter().skip(1).cloned().collect(),
                host_tools_mcp: Some(HostToolsMcpServer {
                    executable: helper.path().to_path_buf(),
                    arguments: vec![
                        "--endpoint".into(),
                        bridge.endpoint().into(),
                        "--run-id".into(),
                        run.id.to_string(),
                        "--policy-revision".into(),
                        policy.revision.to_string(),
                    ],
                }),
                existing_session_id: None,
            })
            .await;
        let session = match session {
            Ok(session) => session,
            Err(error) => {
                bridge.shutdown().await;
                return Err(self.fail_run(run, idempotency_key, error).await);
            }
        };
        let running = self
            .runs
            .transition(
                &run.id,
                run.revision,
                RunState::Running,
                &derived_key(idempotency_key, "running"),
            )
            .await;
        run = match running {
            Ok(run) => run,
            Err(error) => {
                bridge.shutdown().await;
                return Err(error);
            }
        };
        let stream = self
            .runtime
            .prompt(AgentPrompt {
                session_id: session.id,
                text: prompt.into(),
            })
            .await;
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                bridge.shutdown().await;
                return Err(self.fail_run(run, idempotency_key, error).await);
            }
        };
        let mut assistant = String::new();
        let mut completed = false;
        while let Some(event) = stream.next().await {
            match event {
                Ok(AgentEvent::MessageDelta(delta)) => {
                    if assistant.len().saturating_add(delta.len()) > MAX_WORK_RESPONSE_BYTES {
                        bridge.shutdown().await;
                        return Err(self
                            .fail_run(
                                run,
                                idempotency_key,
                                unavailable("Host Work response exceeded its bound"),
                            )
                            .await);
                    }
                    assistant.push_str(&delta);
                }
                Ok(AgentEvent::Completed { .. }) => {
                    completed = true;
                    break;
                }
                Ok(
                    AgentEvent::ThoughtDelta(_)
                    | AgentEvent::ToolCall(_)
                    | AgentEvent::Plan(_)
                    | AgentEvent::Warning(_),
                ) => {}
                Err(error) => {
                    bridge.shutdown().await;
                    return Err(self.fail_run(run, idempotency_key, error).await);
                }
            }
        }
        bridge.shutdown().await;
        if !completed || assistant.trim().is_empty() {
            return Err(self
                .fail_run(
                    run,
                    idempotency_key,
                    unavailable("Host Work ended without a complete response"),
                )
                .await);
        }
        self.workspace
            .create_message(
                CreateMessage {
                    thread_id: thread_id.to_string(),
                    role: MessageRole::Assistant,
                    content: assistant.clone(),
                },
                &derived_key(idempotency_key, "assistant-message"),
            )
            .await?;
        run = self
            .runs
            .transition(
                &run.id,
                run.revision,
                RunState::Completed,
                &derived_key(idempotency_key, "completed"),
            )
            .await?;
        Ok(HostWorkOutcome {
            run,
            assistant_text: assistant,
        })
    }

    async fn fail_run(&self, run: Run, key: &str, error: AgentRuntimeError) -> ApplicationError {
        let _ = self
            .runs
            .transition(
                &run.id,
                run.revision,
                RunState::Failed,
                &derived_key(key, "failed"),
            )
            .await;
        ApplicationError::Unavailable(error.message)
    }
}

fn derived_key(key: &str, stage: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grok-host-work-stage-v1\0");
    hasher.update(stage.as_bytes());
    hasher.update([0]);
    hasher.update(key.as_bytes());
    format!("host-work-{}", hex::encode(hasher.finalize()))
}

fn unavailable(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: grok_application::AgentRuntimeErrorKind::Unavailable,
        message: message.into(),
        retryable: false,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use futures_util::stream;
    use grok_application::{
        AgentAuthMethod, AgentRuntimeCapabilities, AgentRuntimeProbe, AgentSession, CreateProject,
        CreateThread, HostExecutionPolicyStore, IdGenerator, MutationCommand,
    };
    use grok_domain::{HOST_ACKNOWLEDGMENT_VERSION, HostExecutionPolicy, HostToolClasses};
    use grok_memory::{FixedClock, InMemoryExecutionStore, SequentialIdGenerator};

    use crate::{HostWorkRoleFactory, VerifiedHostToolsHelper};

    use super::*;

    #[derive(Debug)]
    struct ScriptedRuntime {
        work: bool,
        saw_mcp: Arc<AtomicBool>,
    }

    #[async_trait]
    impl AgentRuntime for ScriptedRuntime {
        async fn probe(&self) -> Result<AgentRuntimeProbe, AgentRuntimeError> {
            Ok(AgentRuntimeProbe {
                protocol_version: 1,
                agent_name: Some("scripted".into()),
                agent_version: Some("1".into()),
                auth_methods: vec![AgentAuthMethod {
                    id: "grok.com".into(),
                    name: "Grok".into(),
                    description: None,
                }],
                capabilities: AgentRuntimeCapabilities::default(),
            })
        }

        async fn authenticate(&self, _method_id: &str) -> Result<(), AgentRuntimeError> {
            Ok(())
        }

        async fn open_session(
            &self,
            request: AgentSessionRequest,
        ) -> Result<AgentSession, AgentRuntimeError> {
            if !self.work || request.host_tools_mcp.is_none() {
                return Err(unavailable("session denied"));
            }
            self.saw_mcp.store(true, Ordering::SeqCst);
            Ok(AgentSession {
                id: "session-1".into(),
            })
        }

        async fn prompt(
            &self,
            _prompt: AgentPrompt,
        ) -> Result<grok_application::AgentEventStream, AgentRuntimeError> {
            Ok(Box::pin(stream::iter([
                Ok(AgentEvent::MessageDelta("Host Work reply".into())),
                Ok(AgentEvent::Completed {
                    stop_reason: "end_turn".into(),
                }),
            ])))
        }

        async fn cancel(&self, _session_id: &str) -> Result<(), AgentRuntimeError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), AgentRuntimeError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ScriptedFactory {
        saw_mcp: Arc<AtomicBool>,
    }

    #[async_trait]
    impl HostWorkRoleFactory for ScriptedFactory {
        async fn start_control(&self) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
            Ok(Arc::new(ScriptedRuntime {
                work: false,
                saw_mcp: self.saw_mcp.clone(),
            }))
        }

        async fn start_work(
            &self,
            _roots: Vec<PathBuf>,
        ) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
            Ok(Arc::new(ScriptedRuntime {
                work: true,
                saw_mcp: self.saw_mcp.clone(),
            }))
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn executes_and_persists_a_bound_host_work_turn() {
        let root = tempfile::tempdir().expect("root");
        let endpoints = tempfile::tempdir().expect("endpoints");
        let helper =
            VerifiedHostToolsHelper::verify(std::env::current_exe().expect("test executable"))
                .expect("helper");
        let saw_mcp = Arc::new(AtomicBool::new(false));
        let runtime = Arc::new(
            HostWorkRuntime::start(
                Arc::new(ScriptedFactory {
                    saw_mcp: saw_mcp.clone(),
                }),
                Some(helper),
            )
            .await
            .expect("runtime"),
        );
        runtime
            .authenticate("grok.com")
            .await
            .expect("authenticate");
        let policy = HostExecutionPolicy {
            revision: 1,
            active: true,
            acknowledgment_version: HOST_ACKNOWLEDGMENT_VERSION,
            acknowledged_at: 1,
            tool_classes: HostToolClasses {
                filesystem_read: true,
                filesystem_write: false,
                process_execute: false,
            },
            canonical_roots: vec![root.path().to_string_lossy().into_owned()],
            broad_scope_acknowledged: false,
            updated_at: 1,
        };
        runtime.prepare(&policy).await.expect("prepare");
        let store = Arc::new(InMemoryExecutionStore::new());
        store
            .replace_host_execution_policy(
                policy,
                0,
                &MutationCommand {
                    scope: "enroll_host_execution_v1".into(),
                    key: "service-policy".into(),
                    fingerprint: [3; 32],
                },
            )
            .await
            .expect("policy");
        let clock = Arc::new(FixedClock::new(10));
        let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Host".into(),
                    description: String::new(),
                },
                "host-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Host work".into(),
                },
                "host-thread",
            )
            .await
            .expect("thread");
        let execution: Arc<dyn ExecutionStore> = store.clone();
        let runs = Arc::new(RunService::new(execution.clone(), clock, ids));
        let service = HostWorkService::new(
            runtime,
            store,
            execution,
            runs,
            workspace.clone(),
            endpoints.path().to_path_buf(),
        );
        let outcome = service
            .execute(
                project.id.as_str(),
                thread.id.as_str(),
                "Read the project",
                "host-work-command",
            )
            .await
            .expect("execute");
        assert_eq!(outcome.run.state, RunState::Completed);
        assert_eq!(outcome.assistant_text, "Host Work reply");
        assert!(saw_mcp.load(Ordering::SeqCst));
        let messages = workspace
            .list_messages(&thread.id, None, 10)
            .await
            .expect("messages")
            .items;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[1].role, MessageRole::Assistant);
    }
}
