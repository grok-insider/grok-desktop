use async_trait::async_trait;
use thiserror::Error;

/// Version of the narrow privileged VM-service contract accepted by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolationContractVersion {
    /// Contract major version.
    pub major: u16,
    /// Contract minor version.
    pub minor: u16,
    /// Contract patch version.
    pub patch: u16,
}

/// Qualified strong-isolation implementation discovered by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationBackend {
    /// Windows Host Compute Service with `VirtualMachinePlatform` utility VMs.
    HcsVirtualMachinePlatform,
    /// Linux QEMU/KVM utility VMs behind the privileged linux-vm-service broker.
    QemuKvm,
}

/// Host-workspace exposure enforced by the isolation broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationWorkspaceMode {
    /// HCS Plan9 sharing configured read-only by the privileged broker.
    ReadOnlyPlan9,
    /// Linux virtio-9p (or virtio-fs) read-only share from the privileged broker.
    ReadOnlyVirtio9p,
}

/// Closed lifecycle surface advertised by a qualified isolation broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IsolationBrokerOperation {
    /// Read the broker's bounded capability document.
    GetCapabilities,
    /// Install an image selected by the signed product catalog.
    EnsureImage,
    /// Create a utility VM from an installed image.
    CreateVm,
    /// Start a known utility VM.
    StartVm,
    /// Stop a known utility VM.
    StopVm,
    /// Delete a known utility VM.
    DeleteVm,
    /// Attach a validated workspace read-only.
    AttachWorkspace,
}

/// Static, non-authorizing facts returned by a qualified VM-service probe.
///
/// A successful probe does not grant guest control and does not make Work ready.
/// Guest health, caller proof, durable replay, and policy checks are separate gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationBrokerCapabilities {
    /// Accepted service contract version.
    pub contract_version: IsolationContractVersion,
    /// Qualified platform backend.
    pub backend: IsolationBackend,
    /// HCS schema version enforced by the broker (empty for non-HCS backends).
    pub hcs_schema: String,
    /// Enforced host-workspace exposure.
    pub workspace_mode: IsolationWorkspaceMode,
    /// Sorted closed operation set. Guest control is intentionally absent from
    /// the static probe document; grants are a separate PoP gate.
    pub operations: Vec<IsolationBrokerOperation>,
}

/// Stable failure classes for static isolation-broker discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IsolationProbeError {
    /// The local broker transport is absent or temporarily unreachable.
    #[error("isolation broker unavailable")]
    Unavailable,
    /// The service is present but is simulated or does not enforce product policy.
    #[error("isolation broker is not qualified")]
    Unqualified,
    /// The broker uses a contract this daemon does not explicitly support.
    #[error("isolation broker contract is incompatible")]
    Incompatible,
    /// The broker returned malformed, ambiguous, or uncorrelated data.
    #[error("isolation broker protocol failure")]
    Protocol,
}

/// Read-only discovery port for the platform isolation broker.
#[async_trait]
pub trait IsolationProbe: Send + Sync {
    /// Reads and validates static broker capabilities without granting authority.
    ///
    /// # Errors
    ///
    /// Returns [`IsolationProbeError`] when transport, compatibility, qualification,
    /// or response validation fails.
    async fn probe(&self) -> Result<IsolationBrokerCapabilities, IsolationProbeError>;
}
