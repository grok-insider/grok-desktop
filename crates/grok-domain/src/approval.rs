use thiserror::Error;

use crate::{ApprovalId, RunId, UnixMillis};

/// User-visible impact classification for a requested action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApprovalRisk {
    /// Read-only or readily reversible action.
    Low,
    /// Action crosses a new trust boundary.
    Elevated,
    /// Destructive, external, or sensitive action.
    High,
    /// Purchase, credential, account, or similarly exceptional action.
    Critical,
}

/// Maximum reuse permitted for an approval decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalScope {
    /// Authorizes exactly one invocation.
    Once,
    /// Authorizes matching actions for the current run.
    Run,
    /// Authorizes matching actions for one stable resource identity.
    Resource(String),
}

/// Exact action presented to the user for an informed decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedAction {
    /// Stable tool or operation name.
    pub action: String,
    /// Human-readable target, excluding secret values.
    pub target: String,
    /// Concise description of the data crossing the boundary.
    pub data_summary: String,
    /// Impact classification.
    pub risk: ApprovalRisk,
}

/// Approval request lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalStatus {
    /// Awaiting a user decision.
    Pending,
    /// User granted the requested scope.
    Granted,
    /// User denied the request.
    Denied,
    /// Deadline elapsed before a decision.
    Expired,
    /// Owning run was cancelled.
    Cancelled,
}

/// Decision accepted by the application boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Grant the declared scope.
    Grant,
    /// Deny the action.
    Deny,
}

/// Invalid approval lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApprovalError {
    /// Only pending approvals can be decided.
    #[error("approval is already {0:?}")]
    AlreadyDecided(ApprovalStatus),
    /// A decision cannot grant an expired request.
    #[error("approval expired at {expires_at}")]
    Expired {
        /// Expiration timestamp.
        expires_at: UnixMillis,
    },
}

/// Durable, scoped request for user authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// Stable approval identifier.
    pub id: ApprovalId,
    /// Run blocked by this request.
    pub run_id: RunId,
    /// Exact requested operation.
    pub request: RequestedAction,
    /// Maximum authorized reuse.
    pub scope: ApprovalScope,
    /// Current lifecycle status.
    pub status: ApprovalStatus,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Creation time.
    pub created_at: UnixMillis,
    /// Decision deadline.
    pub expires_at: UnixMillis,
    /// Time of decision or expiration.
    pub decided_at: Option<UnixMillis>,
}

impl Approval {
    /// Creates a pending approval request.
    #[must_use]
    pub const fn pending(
        id: ApprovalId,
        run_id: RunId,
        request: RequestedAction,
        scope: ApprovalScope,
        now: UnixMillis,
        expires_at: UnixMillis,
    ) -> Self {
        Self {
            id,
            run_id,
            request,
            scope,
            status: ApprovalStatus::Pending,
            revision: 0,
            created_at: now,
            expires_at,
            decided_at: None,
        }
    }

    /// Applies a user decision if the request remains pending and unexpired.
    ///
    /// # Errors
    ///
    /// Returns [`ApprovalError`] when the request was already decided or its
    /// deadline has elapsed.
    pub fn decide(
        &mut self,
        decision: ApprovalDecision,
        now: UnixMillis,
    ) -> Result<(), ApprovalError> {
        if self.status != ApprovalStatus::Pending {
            return Err(ApprovalError::AlreadyDecided(self.status));
        }
        if now > self.expires_at {
            self.status = ApprovalStatus::Expired;
            self.decided_at = Some(now);
            self.revision = self.revision.saturating_add(1);
            return Err(ApprovalError::Expired {
                expires_at: self.expires_at,
            });
        }
        self.status = match decision {
            ApprovalDecision::Grant => ApprovalStatus::Granted,
            ApprovalDecision::Deny => ApprovalStatus::Denied,
        };
        self.decided_at = Some(now);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval() -> Approval {
        Approval::pending(
            ApprovalId::new("approval-1").expect("id"),
            RunId::new("run-1").expect("id"),
            RequestedAction {
                action: "filesystem.write".into(),
                target: "report.md".into(),
                data_summary: "generated report".into(),
                risk: ApprovalRisk::Elevated,
            },
            ApprovalScope::Once,
            10,
            20,
        )
    }

    #[test]
    fn decision_is_single_use_and_revisioned() {
        let mut approval = approval();
        approval.decide(ApprovalDecision::Grant, 15).expect("grant");
        assert_eq!(approval.status, ApprovalStatus::Granted);
        assert_eq!(approval.revision, 1);
        assert!(matches!(
            approval.decide(ApprovalDecision::Deny, 16),
            Err(ApprovalError::AlreadyDecided(ApprovalStatus::Granted))
        ));
    }

    #[test]
    fn expired_request_fails_closed() {
        let mut approval = approval();
        assert!(matches!(
            approval.decide(ApprovalDecision::Grant, 21),
            Err(ApprovalError::Expired { .. })
        ));
        assert_eq!(approval.status, ApprovalStatus::Expired);
    }
}
