/// Product capabilities resolved from provider entitlements and local readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Local and remote conversational UI.
    Chat,
    /// Sandboxed agent workspace.
    Work,
    /// Project file access.
    Files,
    /// Shell execution within the utility VM.
    Shell,
    /// Model Context Protocol integrations.
    Mcp,
    /// Managed browser automation.
    BrowserAutomation,
    /// Native desktop computer use.
    ComputerUse,
    /// Web and X search.
    Search,
    /// App-owned multi-agent research workflow.
    Research,
    /// Image creation and editing.
    ImagineImage,
    /// Video creation and editing.
    ImagineVideo,
    /// Realtime speech conversation.
    RealtimeVoice,
    /// Local recurring work.
    Automations,
}

/// Official Grok surface providing a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySurface {
    /// Official Grok Build ACP process.
    SubscriptionAcp,
    /// Official xAI API.
    XaiApi,
    /// Fully local desktop behavior.
    Desktop,
    /// Separately installed and signed component.
    ManagedAddon,
    /// Unsupported account state opens an official web page.
    WebHandoff,
}

/// Authentication needed before a capability can become available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// No provider authentication is required.
    None,
    /// OAuth owned by the official Grok Build CLI.
    SubscriptionOAuth,
    /// User-owned xAI API key.
    XaiApiKey,
    /// Either official authentication path is sufficient.
    Either,
}

/// Static requirements for one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRequirement {
    /// Product feature being resolved.
    pub capability: Capability,
    /// Official surface expected to implement it.
    pub surface: CapabilitySurface,
    /// Authentication prerequisite.
    pub authentication: AuthMethod,
    /// Whether strong local isolation is mandatory.
    pub requires_strong_isolation: bool,
}

/// Availability presented to UI clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityAvailability {
    /// Ready for use.
    Available,
    /// A safe, reduced behavior is available.
    Limited,
    /// The feature cannot be invoked.
    Unavailable,
}

/// Resolved state and actionable explanation for a feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityStatus {
    /// Product feature.
    pub capability: Capability,
    /// Official or local implementation surface.
    pub surface: CapabilitySurface,
    /// Authentication prerequisite.
    pub authentication: AuthMethod,
    /// Current resolution.
    pub availability: CapabilityAvailability,
    /// Stable machine-readable reason for UI branching and diagnostics.
    pub reason_code: String,
    /// Safe user-facing explanation.
    pub reason: String,
}
