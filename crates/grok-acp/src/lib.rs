//! Official Grok Build ACP process adapter.

mod catalog;
mod component;
mod isolation;
mod permission;
mod pinned;
mod runtime;

pub use catalog::{
    CatalogVerificationError, MAX_SIGNED_CATALOG_ENVELOPE_BYTES, OfficialGrokCatalogVerifier,
    TrustedCatalogKey, VerifiedCatalogComponent,
};
pub use component::{ComponentVerificationError, ExternalGrokComponent, VerifiedGrokComponent};
pub use isolation::{GrokHomeError, GrokHomeSpec};
pub use permission::{
    HostPermissionChannel, PendingPermission, PermissionBroker, permission_channel,
};
pub use pinned::{
    MAX_PINNED_COMPONENT_MANIFEST_BYTES, OfficialGrokPinnedComponentVerifier,
    PinnedComponentVerificationError,
};
pub use runtime::{GrokAcpConfig, GrokAcpRuntime};
