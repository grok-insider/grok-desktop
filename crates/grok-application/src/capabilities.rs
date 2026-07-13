use grok_domain::{
    AuthMethod, Capability, CapabilityAvailability, CapabilityStatus, CapabilitySurface,
    WorkExecutionBackend,
};

/// Current authentication, connectivity, and local-runtime facts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct CapabilityFacts {
    /// Official Grok Build CLI has an active subscription session.
    pub subscription_authenticated: bool,
    /// A user-owned xAI API key is configured.
    pub xai_api_key_configured: bool,
    /// A daemon-owned `SuperGrok` `api:access` grant is connected.
    pub supergrok_api_connected: bool,
    /// The daemon validated its selected xAI model for the active API rail.
    pub xai_capabilities_resolved: bool,
    /// Provider network calls may be attempted; each call still enforces live reachability.
    pub online: bool,
    /// Static packaged VM broker identity and contract checks passed.
    ///
    /// This does not prove guest health, caller authorization, or execution readiness.
    pub isolation_broker_qualified: bool,
    /// HCS utility-VM readiness checks passed.
    pub strong_isolation_ready: bool,
    /// Dedicated managed browser is installed and healthy.
    pub managed_browser_ready: bool,
    /// Native platform computer-use broker is healthy.
    pub computer_use_ready: bool,
    /// Private immutable artifact storage and native exact-version open are qualified.
    pub artifact_content_ready: bool,
    /// Automation scheduler journal is live and occurrence dispatch is armed.
    pub automation_scheduler_ready: bool,
    /// A current versioned Host Tools enrollment exists.
    pub host_policy_effective: bool,
    /// The daemon has prepared and authenticated the constrained Host Work runtime.
    pub host_work_runtime_ready: bool,
    /// Current Host Tools enrollment includes exact-command process execution.
    pub host_process_execute_enabled: bool,
}

/// Deterministic product capability policy shared by UI and daemon workflows.
#[derive(Debug, Default, Clone, Copy)]
pub struct CapabilityResolver;

impl CapabilityResolver {
    /// Resolves all product capabilities with stable reason codes.
    #[must_use]
    pub fn resolve(facts: CapabilityFacts) -> Vec<CapabilityStatus> {
        let mut statuses = Vec::with_capacity(13);
        statuses.extend(local_definition_capabilities(facts));
        statuses.push(chat_capability(facts));
        statuses.extend(execution_capabilities(facts));
        statuses.extend(unavailable_provider_capabilities(facts));
        statuses
    }

    /// Selects the backend for a new Work run without creating authority.
    #[must_use]
    pub fn resolve_work_backend(facts: CapabilityFacts) -> Option<WorkExecutionBackend> {
        if facts.subscription_authenticated && facts.strong_isolation_ready {
            Some(WorkExecutionBackend::IsolatedGuest)
        } else if facts.subscription_authenticated
            && facts.host_policy_effective
            && facts.host_work_runtime_ready
        {
            Some(WorkExecutionBackend::HostDirect)
        } else {
            None
        }
    }
}

fn local_definition_capabilities(facts: CapabilityFacts) -> [CapabilityStatus; 2] {
    [
        status(
            Capability::Files,
            CapabilitySurface::Desktop,
            AuthMethod::None,
            facts.artifact_content_ready,
            CapabilityAvailability::Limited,
            "artifact_content_unavailable",
            "Artifact metadata is available, but private import and local open are not qualified on this runtime.",
        ),
        status(
            Capability::Automations,
            CapabilitySurface::Desktop,
            AuthMethod::None,
            facts.automation_scheduler_ready,
            CapabilityAvailability::Limited,
            "automation_scheduler_unavailable",
            "Automation definitions are available, but scheduled execution is not yet qualified.",
        ),
    ]
}

fn chat_capability(facts: CapabilityFacts) -> CapabilityStatus {
    status(
        Capability::Chat,
        CapabilitySurface::XaiApi,
        AuthMethod::Either,
        (facts.xai_api_key_configured || facts.supergrok_api_connected)
            && facts.xai_capabilities_resolved
            && facts.online,
        CapabilityAvailability::Limited,
        "xai_conversation_unavailable",
        "Chat requires a connected SuperGrok API grant or validated xAI API key, selected model, and network access.",
    )
}

fn execution_capabilities(facts: CapabilityFacts) -> [CapabilityStatus; 5] {
    let backend = CapabilityResolver::resolve_work_backend(facts);
    let work_ready = backend.is_some();
    let host_ready = backend == Some(WorkExecutionBackend::HostDirect);
    let isolation_ready = facts.strong_isolation_ready;
    [
        status(
            Capability::Work,
            CapabilitySurface::SubscriptionAcp,
            AuthMethod::SubscriptionOAuth,
            work_ready,
            CapabilityAvailability::Unavailable,
            if facts.strong_isolation_ready {
                "subscription_session_unavailable"
            } else if facts.host_policy_effective && !facts.host_work_runtime_ready {
                "host_tools_runtime_not_prepared"
            } else if !facts.host_policy_effective {
                "host_tools_not_enrolled"
            } else {
                "work_execution_unavailable"
            },
            if facts.strong_isolation_ready {
                "Work requires an authenticated official Grok Build subscription session in the guest."
            } else if facts.host_policy_effective {
                "Prepare the authenticated Host Tools runtime before starting Work."
            } else {
                "Work requires Isolated Guest readiness or explicit Host Tools enrollment."
            },
        ),
        status(
            Capability::Shell,
            CapabilitySurface::Desktop,
            AuthMethod::SubscriptionOAuth,
            work_ready && (!host_ready || facts.host_process_execute_enabled),
            CapabilityAvailability::Unavailable,
            if host_ready {
                "host_process_execution_not_enrolled"
            } else {
                "work_execution_unavailable"
            },
            "Shell requires Isolated Work or the separately acknowledged Host process-execution class.",
        ),
        status(
            Capability::Mcp,
            CapabilitySurface::Desktop,
            AuthMethod::None,
            isolation_ready,
            CapabilityAvailability::Unavailable,
            "mcp_sandbox_unavailable",
            "MCP servers require the qualified guest execution backend.",
        ),
        status(
            Capability::BrowserAutomation,
            CapabilitySurface::ManagedAddon,
            AuthMethod::None,
            isolation_ready && facts.managed_browser_ready,
            CapabilityAvailability::Unavailable,
            "managed_browser_unavailable",
            "Browser automation requires the qualified managed browser broker.",
        ),
        status(
            Capability::ComputerUse,
            CapabilitySurface::Desktop,
            AuthMethod::None,
            isolation_ready && facts.computer_use_ready,
            CapabilityAvailability::Unavailable,
            "computer_use_broker_unavailable",
            "Computer use requires the qualified native broker and guest proxy.",
        ),
    ]
}

fn unavailable_provider_capabilities(facts: CapabilityFacts) -> [CapabilityStatus; 5] {
    let setup_reason = if facts.xai_api_key_configured {
        "The xAI credential is configured, but this provider workflow is not yet exposed."
    } else {
        "This workflow requires native xAI credential enrollment and a qualified daemon command."
    };
    [
        unavailable(
            Capability::Search,
            CapabilitySurface::XaiApi,
            AuthMethod::XaiApiKey,
            "search_operation_unavailable",
            setup_reason,
        ),
        unavailable(
            Capability::Research,
            CapabilitySurface::XaiApi,
            AuthMethod::XaiApiKey,
            "research_operation_unavailable",
            setup_reason,
        ),
        unavailable(
            Capability::ImagineImage,
            CapabilitySurface::XaiApi,
            AuthMethod::XaiApiKey,
            "image_operation_unavailable",
            setup_reason,
        ),
        unavailable(
            Capability::ImagineVideo,
            CapabilitySurface::XaiApi,
            AuthMethod::XaiApiKey,
            "video_operation_unavailable",
            setup_reason,
        ),
        unavailable(
            Capability::RealtimeVoice,
            CapabilitySurface::XaiApi,
            AuthMethod::XaiApiKey,
            "voice_operation_unavailable",
            setup_reason,
        ),
    ]
}

fn unavailable(
    capability: Capability,
    surface: CapabilitySurface,
    authentication: AuthMethod,
    reason_code: &str,
    reason: &str,
) -> CapabilityStatus {
    status(
        capability,
        surface,
        authentication,
        false,
        CapabilityAvailability::Unavailable,
        reason_code,
        reason,
    )
}

#[allow(clippy::too_many_arguments)]
fn status(
    capability: Capability,
    surface: CapabilitySurface,
    authentication: AuthMethod,
    ready: bool,
    fallback: CapabilityAvailability,
    reason_code: &str,
    reason: &str,
) -> CapabilityStatus {
    CapabilityStatus {
        capability,
        surface,
        authentication,
        availability: if ready {
            CapabilityAvailability::Available
        } else {
            fallback
        },
        reason_code: if ready { "ready" } else { reason_code }.into(),
        reason: if ready { "Available." } else { reason }.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_enrollment_never_enables_work_until_runtime_is_ready() {
        let facts = CapabilityFacts {
            subscription_authenticated: true,
            host_policy_effective: true,
            host_process_execute_enabled: true,
            ..CapabilityFacts::default()
        };
        assert_eq!(CapabilityResolver::resolve_work_backend(facts), None);
        let work = CapabilityResolver::resolve(facts)
            .into_iter()
            .find(|status| status.capability == Capability::Work)
            .expect("Work status");
        assert_eq!(work.reason_code, "host_tools_runtime_not_prepared");
    }

    #[test]
    fn ready_host_is_explicit_and_qualified_guest_preempts_it() {
        let host = CapabilityFacts {
            subscription_authenticated: true,
            host_policy_effective: true,
            host_work_runtime_ready: true,
            host_process_execute_enabled: true,
            ..CapabilityFacts::default()
        };
        assert_eq!(
            CapabilityResolver::resolve_work_backend(host),
            Some(WorkExecutionBackend::HostDirect)
        );
        assert_eq!(
            CapabilityResolver::resolve_work_backend(CapabilityFacts {
                strong_isolation_ready: true,
                ..host
            }),
            Some(WorkExecutionBackend::IsolatedGuest)
        );
    }

    #[test]
    fn limited_mode_never_enables_execution() {
        let statuses = CapabilityResolver::resolve(CapabilityFacts::default());
        let chat = statuses
            .iter()
            .find(|item| item.capability == Capability::Chat)
            .expect("chat");
        let work = statuses
            .iter()
            .find(|item| item.capability == Capability::Work)
            .expect("work");
        assert_eq!(chat.availability, CapabilityAvailability::Limited);
        assert_eq!(work.availability, CapabilityAvailability::Unavailable);
    }

    #[test]
    fn api_key_enables_only_the_implemented_direct_chat_path() {
        let statuses = CapabilityResolver::resolve(CapabilityFacts {
            xai_api_key_configured: true,
            xai_capabilities_resolved: true,
            online: true,
            strong_isolation_ready: true,
            ..CapabilityFacts::default()
        });
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Chat)
                .expect("chat")
                .availability,
            CapabilityAvailability::Available
        );
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Work)
                .expect("work")
                .availability,
            CapabilityAvailability::Unavailable
        );
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Research)
                .expect("research")
                .availability,
            CapabilityAvailability::Unavailable
        );
    }

    #[test]
    fn supergrok_api_grant_enables_only_the_implemented_chat_path() {
        let statuses = CapabilityResolver::resolve(CapabilityFacts {
            supergrok_api_connected: true,
            xai_capabilities_resolved: true,
            online: true,
            ..CapabilityFacts::default()
        });
        let chat = statuses
            .iter()
            .find(|item| item.capability == Capability::Chat)
            .expect("chat");
        assert_eq!(chat.authentication, AuthMethod::Either);
        assert_eq!(chat.availability, CapabilityAvailability::Available);
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::ImagineImage)
                .expect("image")
                .availability,
            CapabilityAvailability::Unavailable
        );
    }

    #[test]
    fn qualified_artifact_content_enables_only_the_files_capability() {
        let statuses = CapabilityResolver::resolve(CapabilityFacts {
            artifact_content_ready: true,
            ..CapabilityFacts::default()
        });
        let files = statuses
            .iter()
            .find(|item| item.capability == Capability::Files)
            .expect("files");
        assert_eq!(files.availability, CapabilityAvailability::Available);
        assert_eq!(files.reason_code, "ready");
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Automations)
                .expect("automations")
                .availability,
            CapabilityAvailability::Limited
        );
    }

    #[test]
    fn static_broker_qualification_never_enables_work() {
        let statuses = CapabilityResolver::resolve(CapabilityFacts {
            subscription_authenticated: true,
            isolation_broker_qualified: true,
            strong_isolation_ready: false,
            ..CapabilityFacts::default()
        });
        for capability in [
            Capability::Work,
            Capability::Shell,
            Capability::Mcp,
            Capability::ComputerUse,
        ] {
            assert_eq!(
                statuses
                    .iter()
                    .find(|status| status.capability == capability)
                    .expect("execution capability")
                    .availability,
                CapabilityAvailability::Unavailable
            );
        }
    }

    #[test]
    fn work_requires_subscription_and_strong_isolation_facts() {
        let statuses = CapabilityResolver::resolve(CapabilityFacts {
            subscription_authenticated: true,
            strong_isolation_ready: true,
            isolation_broker_qualified: true,
            ..CapabilityFacts::default()
        });
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Work)
                .expect("work")
                .availability,
            CapabilityAvailability::Available
        );
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Shell)
                .expect("shell")
                .availability,
            CapabilityAvailability::Available
        );
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Mcp)
                .expect("mcp")
                .availability,
            CapabilityAvailability::Available
        );
        // Computer use still needs computer_use_ready.
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::ComputerUse)
                .expect("computer use")
                .availability,
            CapabilityAvailability::Unavailable
        );
    }

    #[test]
    fn isolation_without_subscription_does_not_enable_work() {
        let statuses = CapabilityResolver::resolve(CapabilityFacts {
            strong_isolation_ready: true,
            isolation_broker_qualified: true,
            subscription_authenticated: false,
            ..CapabilityFacts::default()
        });
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Work)
                .expect("work")
                .availability,
            CapabilityAvailability::Unavailable
        );
        assert_eq!(
            statuses
                .iter()
                .find(|item| item.capability == Capability::Work)
                .expect("work")
                .reason_code,
            "subscription_session_unavailable"
        );
    }
}
