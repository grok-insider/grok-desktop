use thiserror::Error;

use crate::UnixMillis;

/// Current disclosure contract accepted by a Host Tools enrollment.
pub const HOST_ACKNOWLEDGMENT_VERSION: u32 = 1;
/// Exact v1 phrase required after surrounding-whitespace removal.
pub const HOST_ACKNOWLEDGMENT_PHRASE: &str = "I UNDERSTAND HOST TOOLS CAN CONTROL THIS COMPUTER";
/// Maximum number of separately enrolled filesystem roots.
pub const MAX_HOST_EXECUTION_ROOTS: usize = 8;

/// Closed set of daemon-owned Host Tools classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostToolClasses {
    /// Directory listing and bounded file reads.
    pub filesystem_read: bool,
    /// Exact-target file creation or replacement.
    pub filesystem_write: bool,
    /// Exact-command process execution with full desktop-user authority.
    pub process_execute: bool,
}

impl HostToolClasses {
    /// Returns whether at least one class was deliberately selected.
    #[must_use]
    pub const fn any(self) -> bool {
        self.filesystem_read || self.filesystem_write || self.process_execute
    }
}

/// Versioned, revocable daemon-owned Host Tools enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecutionPolicy {
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Whether the enrollment has not been revoked.
    pub active: bool,
    /// Disclosure contract version accepted by the user.
    pub acknowledgment_version: u32,
    /// Time at which the current disclosure and scope were accepted.
    pub acknowledged_at: UnixMillis,
    /// Enabled daemon-owned tool classes.
    pub tool_classes: HostToolClasses,
    /// Canonical absolute filesystem roots, stored as opaque platform strings.
    pub canonical_roots: Vec<String>,
    /// Whether broad home/drive scope received the additional acknowledgment.
    pub broad_scope_acknowledged: bool,
    /// Last durable mutation time.
    pub updated_at: UnixMillis,
}

impl HostExecutionPolicy {
    /// Returns whether this policy can authorize a newly created Host Work run.
    #[must_use]
    pub fn is_effectively_active(&self) -> bool {
        self.active
            && self.acknowledgment_version == HOST_ACKNOWLEDGMENT_VERSION
            && self.tool_classes.any()
            && !self.canonical_roots.is_empty()
    }

    /// Validates a complete replacement enrollment before persistence.
    ///
    /// # Errors
    ///
    /// Returns [`HostExecutionPolicyError`] when the disclosure, selected tool
    /// classes, or bounded canonical roots violate the current contract.
    pub fn validate_enrollment(
        acknowledgment_version: u32,
        typed_acknowledgment: &str,
        tool_classes: HostToolClasses,
        canonical_roots: &[String],
    ) -> Result<(), HostExecutionPolicyError> {
        if acknowledgment_version != HOST_ACKNOWLEDGMENT_VERSION
            || typed_acknowledgment.trim() != HOST_ACKNOWLEDGMENT_PHRASE
        {
            return Err(HostExecutionPolicyError::AcknowledgmentMismatch);
        }
        if !tool_classes.any() {
            return Err(HostExecutionPolicyError::NoToolClasses);
        }
        if canonical_roots.is_empty() || canonical_roots.len() > MAX_HOST_EXECUTION_ROOTS {
            return Err(HostExecutionPolicyError::InvalidRoots);
        }
        if canonical_roots.iter().any(|root| {
            root.is_empty() || root.len() > 4096 || root.chars().any(|character| character == '\0')
        }) {
            return Err(HostExecutionPolicyError::InvalidRoots);
        }
        Ok(())
    }
}

/// Invalid Host Tools enrollment input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HostExecutionPolicyError {
    /// Disclosure version or exact phrase was not accepted.
    #[error("Host Tools acknowledgment does not match the current contract")]
    AcknowledgmentMismatch,
    /// No useful tool class was selected.
    #[error("at least one Host Tools class is required")]
    NoToolClasses,
    /// Root count or representation violates the bounded contract.
    #[error("Host Tools roots are invalid")]
    InvalidRoots,
}

#[cfg(test)]
mod tests {
    use super::*;

    const READ_ONLY: HostToolClasses = HostToolClasses {
        filesystem_read: true,
        filesystem_write: false,
        process_execute: false,
    };

    #[test]
    fn enrollment_requires_current_exact_acknowledgment_and_bounded_scope() {
        assert!(
            HostExecutionPolicy::validate_enrollment(
                HOST_ACKNOWLEDGMENT_VERSION,
                &format!("  {HOST_ACKNOWLEDGMENT_PHRASE}\n"),
                READ_ONLY,
                &["/workspace".into()],
            )
            .is_ok()
        );
        assert_eq!(
            HostExecutionPolicy::validate_enrollment(
                HOST_ACKNOWLEDGMENT_VERSION,
                "I accept",
                READ_ONLY,
                &["/workspace".into()],
            ),
            Err(HostExecutionPolicyError::AcknowledgmentMismatch)
        );
        assert_eq!(
            HostExecutionPolicy::validate_enrollment(
                HOST_ACKNOWLEDGMENT_VERSION,
                HOST_ACKNOWLEDGMENT_PHRASE,
                READ_ONLY,
                &[],
            ),
            Err(HostExecutionPolicyError::InvalidRoots)
        );
    }
}
