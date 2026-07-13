use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use grok_acp::{
    GrokAcpConfig, GrokAcpRuntime, GrokHomeSpec, VerifiedGrokComponent, permission_channel,
};
use grok_application::{
    AgentEventStream, AgentPermissionDecision, AgentPrompt, AgentRuntime, AgentRuntimeError,
    AgentRuntimeErrorKind, AgentRuntimeProbe, AgentSession, AgentSessionRequest,
};
use grok_domain::HostExecutionPolicy;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const MAX_HELPER_BYTES: u64 = 32 * 1024 * 1024;

/// Exact packaged helper identity revalidated before a Host Work role switch.
#[derive(Debug, Clone)]
pub struct VerifiedHostToolsHelper {
    path: PathBuf,
    sha256: [u8; 32],
}

impl VerifiedHostToolsHelper {
    /// Verifies and retains the current helper identity.
    ///
    /// # Errors
    ///
    /// Returns an integrity-safe runtime error when the helper is missing,
    /// non-absolute, empty, oversized, or unreadable.
    pub fn verify(path: PathBuf) -> Result<Self, AgentRuntimeError> {
        if !path.is_absolute() {
            return Err(invalid("Host Tools helper path must be absolute"));
        }
        let metadata = std::fs::metadata(&path)
            .map_err(|_| unavailable("Host Tools helper is unavailable"))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_HELPER_BYTES {
            return Err(invalid("Host Tools helper identity is invalid"));
        }
        let bytes =
            std::fs::read(&path).map_err(|_| unavailable("Host Tools helper is unavailable"))?;
        Ok(Self {
            path,
            sha256: Sha256::digest(bytes).into(),
        })
    }

    fn reverify(&self) -> Result<(), AgentRuntimeError> {
        let current = Self::verify(self.path.clone())?;
        if current.sha256 != self.sha256 {
            return Err(unavailable("Host Tools helper identity changed"));
        }
        Ok(())
    }

    /// Returns the verified absolute executable path.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Creates one exclusive official ACP home owner for a requested role.
#[async_trait]
pub trait HostWorkRoleFactory: Send + Sync {
    /// Starts authentication-only `HostControl`.
    async fn start_control(&self) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError>;

    /// Starts session-capable `HostWorkTools` for the enrolled roots.
    async fn start_work(
        &self,
        roots: Vec<PathBuf>,
    ) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError>;
}

/// Official ACP factory sharing one component and one exclusive `GROK_HOME`.
#[derive(Debug, Clone)]
pub struct GrokAcpRoleFactory {
    component: VerifiedGrokComponent,
    home: GrokHomeSpec,
}

impl GrokAcpRoleFactory {
    /// Creates a factory from an already verified official component.
    #[must_use]
    pub const fn new(component: VerifiedGrokComponent, home: GrokHomeSpec) -> Self {
        Self { component, home }
    }

    async fn start(
        &self,
        config: GrokAcpConfig,
    ) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
        let (mut host, broker) = permission_channel(
            std::num::NonZeroUsize::new(16).unwrap_or(std::num::NonZeroUsize::MIN),
            std::time::Duration::from_mins(1),
        );
        tokio::spawn(async move {
            while let Some(pending) = host.recv().await {
                // Residual agent-native tools never gain authority from ACP
                // permission options. Product tools use the daemon MCP gate.
                let _ = pending.respond(AgentPermissionDecision::Cancelled);
            }
        });
        Ok(Arc::new(GrokAcpRuntime::start(config, broker).await?))
    }
}

#[async_trait]
impl HostWorkRoleFactory for GrokAcpRoleFactory {
    async fn start_control(&self) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
        self.start(GrokAcpConfig::host_control(
            self.component.clone(),
            self.home.clone(),
        ))
        .await
    }

    async fn start_work(
        &self,
        roots: Vec<PathBuf>,
    ) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
        self.start(GrokAcpConfig::host_work_tools(
            self.component.clone(),
            roots,
            self.home.clone(),
        ))
        .await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Control,
    Work,
    Unavailable,
}

struct RuntimeState {
    runtime: Option<Arc<dyn AgentRuntime>>,
    role: Role,
    authenticated_method: Option<String>,
}

/// Delegating official runtime with an exclusive serialized home-role switch.
pub struct HostWorkRuntime {
    factory: Arc<dyn HostWorkRoleFactory>,
    helper: Option<VerifiedHostToolsHelper>,
    state: Mutex<RuntimeState>,
}

impl std::fmt::Debug for HostWorkRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostWorkRuntime")
            .finish_non_exhaustive()
    }
}

impl HostWorkRuntime {
    /// Starts the default authentication-only role.
    ///
    /// # Errors
    ///
    /// Returns the sanitized ACP startup failure when `HostControl` cannot be
    /// created.
    pub async fn start(
        factory: Arc<dyn HostWorkRoleFactory>,
        helper: Option<VerifiedHostToolsHelper>,
    ) -> Result<Self, AgentRuntimeError> {
        let runtime = factory.start_control().await?;
        Ok(Self {
            factory,
            helper,
            state: Mutex::new(RuntimeState {
                runtime: Some(runtime),
                role: Role::Control,
                authenticated_method: None,
            }),
        })
    }

    /// Switches to `HostWorkTools` and proves non-interactive auth resume.
    ///
    /// # Errors
    ///
    /// Returns a sanitized policy, helper, process, or authentication error and
    /// attempts to restore `HostControl` before returning.
    pub async fn prepare(&self, policy: &HostExecutionPolicy) -> Result<(), AgentRuntimeError> {
        if !policy.is_effectively_active() {
            return Err(invalid("Host Tools enrollment is not active"));
        }
        let helper = self
            .helper
            .as_ref()
            .ok_or_else(|| unavailable("Host Tools helper is unavailable"))?;
        helper.reverify()?;
        let roots = policy
            .canonical_roots
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let mut state = self.state.lock().await;
        if state.role == Role::Work {
            return Ok(());
        }
        let method = state
            .authenticated_method
            .clone()
            .ok_or_else(|| authentication("Grok Build authentication is required"))?;
        shutdown_current(&mut state).await;
        let work = match self.factory.start_work(roots).await {
            Ok(runtime) => runtime,
            Err(error) => {
                restore_control(&self.factory, &mut state, Some(&method)).await;
                return Err(error);
            }
        };
        if let Err(error) = work.authenticate(&method).await {
            let _ = work.shutdown().await;
            restore_control(&self.factory, &mut state, Some(&method)).await;
            return Err(error);
        }
        state.runtime = Some(work);
        state.role = Role::Work;
        Ok(())
    }

    /// Restores authentication-only `HostControl`.
    ///
    /// # Errors
    ///
    /// Returns a sanitized startup or authentication error when the control
    /// role cannot be restored.
    pub async fn deactivate(&self) -> Result<(), AgentRuntimeError> {
        let mut state = self.state.lock().await;
        if state.role == Role::Control {
            return Ok(());
        }
        let method = state.authenticated_method.clone();
        shutdown_current(&mut state).await;
        let control = self.factory.start_control().await?;
        if let Some(method) = &method {
            control.authenticate(method).await?;
        }
        state.runtime = Some(control);
        state.role = Role::Control;
        Ok(())
    }

    /// Reports whether the resident role is authenticated `HostWorkTools`.
    pub async fn is_ready(&self) -> bool {
        self.state.lock().await.role == Role::Work
    }

    async fn current(&self) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
        self.state
            .lock()
            .await
            .runtime
            .clone()
            .ok_or_else(|| unavailable("official Grok runtime is unavailable"))
    }
}

async fn shutdown_current(state: &mut RuntimeState) {
    state.role = Role::Unavailable;
    if let Some(runtime) = state.runtime.take() {
        let _ = runtime.shutdown().await;
    }
}

async fn restore_control(
    factory: &Arc<dyn HostWorkRoleFactory>,
    state: &mut RuntimeState,
    method: Option<&str>,
) {
    let Ok(control) = factory.start_control().await else {
        return;
    };
    if let Some(method) = method
        && control.authenticate(method).await.is_err()
    {
        let _ = control.shutdown().await;
        return;
    }
    state.runtime = Some(control);
    state.role = Role::Control;
}

#[async_trait]
impl AgentRuntime for HostWorkRuntime {
    async fn probe(&self) -> Result<AgentRuntimeProbe, AgentRuntimeError> {
        self.current().await?.probe().await
    }

    async fn authenticate(&self, method_id: &str) -> Result<(), AgentRuntimeError> {
        let runtime = self.current().await?;
        runtime.authenticate(method_id).await?;
        self.state.lock().await.authenticated_method = Some(method_id.into());
        Ok(())
    }

    async fn open_session(
        &self,
        request: AgentSessionRequest,
    ) -> Result<AgentSession, AgentRuntimeError> {
        self.current().await?.open_session(request).await
    }

    async fn prompt(&self, prompt: AgentPrompt) -> Result<AgentEventStream, AgentRuntimeError> {
        self.current().await?.prompt(prompt).await
    }

    async fn cancel(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        self.current().await?.cancel(session_id).await
    }

    async fn shutdown(&self) -> Result<(), AgentRuntimeError> {
        let mut state = self.state.lock().await;
        shutdown_current(&mut state).await;
        Ok(())
    }
}

fn invalid(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::InvalidRequest,
        message: message.into(),
        retryable: false,
    }
}

fn unavailable(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::Unavailable,
        message: message.into(),
        retryable: true,
    }
}

fn authentication(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::Authentication,
        message: message.into(),
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use grok_application::{AgentAuthMethod, AgentRuntimeCapabilities};
    use grok_domain::{HOST_ACKNOWLEDGMENT_VERSION, HostToolClasses};

    use super::*;

    #[derive(Debug)]
    struct FakeRuntime {
        fail_auth: bool,
    }

    #[async_trait]
    impl AgentRuntime for FakeRuntime {
        async fn probe(&self) -> Result<AgentRuntimeProbe, AgentRuntimeError> {
            Ok(AgentRuntimeProbe {
                protocol_version: 1,
                agent_name: Some("fake".into()),
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
            if self.fail_auth {
                Err(authentication("resume failed"))
            } else {
                Ok(())
            }
        }

        async fn open_session(
            &self,
            _request: AgentSessionRequest,
        ) -> Result<AgentSession, AgentRuntimeError> {
            Err(unavailable("unused"))
        }

        async fn prompt(
            &self,
            _prompt: AgentPrompt,
        ) -> Result<AgentEventStream, AgentRuntimeError> {
            Err(unavailable("unused"))
        }

        async fn cancel(&self, _session_id: &str) -> Result<(), AgentRuntimeError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), AgentRuntimeError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeFactory {
        control_starts: AtomicUsize,
        work_starts: AtomicUsize,
        fail_next_work_auth: AtomicBool,
    }

    #[async_trait]
    impl HostWorkRoleFactory for FakeFactory {
        async fn start_control(&self) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
            self.control_starts.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(FakeRuntime { fail_auth: false }))
        }

        async fn start_work(
            &self,
            _roots: Vec<PathBuf>,
        ) -> Result<Arc<dyn AgentRuntime>, AgentRuntimeError> {
            self.work_starts.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(FakeRuntime {
                fail_auth: self.fail_next_work_auth.swap(false, Ordering::SeqCst),
            }))
        }
    }

    fn policy(root: &std::path::Path) -> HostExecutionPolicy {
        HostExecutionPolicy {
            revision: 1,
            active: true,
            acknowledgment_version: HOST_ACKNOWLEDGMENT_VERSION,
            acknowledged_at: 1,
            tool_classes: HostToolClasses {
                filesystem_read: true,
                filesystem_write: true,
                process_execute: true,
            },
            canonical_roots: vec![root.to_string_lossy().into_owned()],
            broad_scope_acknowledged: false,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn prepare_requires_auth_and_role_switch_is_serial_and_reversible() {
        let directory = tempfile::tempdir().expect("directory");
        let helper_path = directory.path().join("helper");
        std::fs::write(&helper_path, b"verified helper").expect("helper");
        let helper = VerifiedHostToolsHelper::verify(helper_path).expect("verify helper");
        let factory = Arc::new(FakeFactory::default());
        let runtime = HostWorkRuntime::start(factory.clone(), Some(helper))
            .await
            .expect("control");
        assert!(runtime.prepare(&policy(directory.path())).await.is_err());
        runtime
            .authenticate("grok.com")
            .await
            .expect("authenticate");
        runtime
            .prepare(&policy(directory.path()))
            .await
            .expect("prepare");
        assert!(runtime.is_ready().await);
        runtime
            .prepare(&policy(directory.path()))
            .await
            .expect("idempotent prepare");
        assert_eq!(factory.work_starts.load(Ordering::SeqCst), 1);
        runtime.deactivate().await.expect("deactivate");
        assert!(!runtime.is_ready().await);
        assert_eq!(factory.control_starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_resume_restores_control_and_never_reports_ready() {
        let directory = tempfile::tempdir().expect("directory");
        let helper_path = directory.path().join("helper");
        std::fs::write(&helper_path, b"verified helper").expect("helper");
        let helper = VerifiedHostToolsHelper::verify(helper_path).expect("verify helper");
        let factory = Arc::new(FakeFactory::default());
        factory.fail_next_work_auth.store(true, Ordering::SeqCst);
        let runtime = HostWorkRuntime::start(factory.clone(), Some(helper))
            .await
            .expect("control");
        runtime
            .authenticate("grok.com")
            .await
            .expect("authenticate");
        assert!(runtime.prepare(&policy(directory.path())).await.is_err());
        assert!(!runtime.is_ready().await);
        assert_eq!(factory.control_starts.load(Ordering::SeqCst), 2);
        runtime.probe().await.expect("restored control");
    }
}
