use std::fmt::{self, Display};

use thiserror::Error;

/// Validation error for an entity identifier.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdError {
    /// An identifier must contain at least one character.
    #[error("identifier cannot be empty")]
    Empty,
    /// Identifier lengths are bounded before they enter storage or protocols.
    #[error("identifier exceeds 128 bytes")]
    TooLong,
    /// Identifiers are printable, single-line values.
    #[error("identifier contains control characters")]
    ControlCharacter,
}

macro_rules! entity_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier after validating its transport-safe form.
            ///
            /// # Errors
            ///
            /// Returns [`IdError`] when the value is empty, too long, or contains
            /// control characters.
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                if value.is_empty() {
                    return Err(IdError::Empty);
                }
                if value.len() > 128 {
                    return Err(IdError::TooLong);
                }
                if value.chars().any(char::is_control) {
                    return Err(IdError::ControlCharacter);
                }
                Ok(Self(value))
            }

            /// Returns the stable external representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes this identifier into its external representation.
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

entity_id!(ProjectId, "A locally owned project identifier.");
entity_id!(ThreadId, "A conversation thread identifier.");
entity_id!(RunId, "An agent run identifier.");
entity_id!(ApprovalId, "An approval request identifier.");
entity_id!(EffectId, "A durable side-effect intent identifier.");
entity_id!(
    PrivilegedOperationId,
    "A durable privileged-operation journal identifier."
);
entity_id!(
    ConversationTurnId,
    "A durable direct-model conversation turn identifier."
);
entity_id!(MessageId, "A canonical conversation message identifier.");
entity_id!(ArtifactId, "A durable project artifact identifier.");
entity_id!(AutomationId, "A durable automation definition identifier.");
entity_id!(
    AutomationOccurrenceId,
    "A durable scheduled automation occurrence identifier."
);
entity_id!(
    AutomationSchedulerOwnerId,
    "A daemon process identity used for fenced scheduler ownership."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_reject_values_unsafe_for_protocols() {
        assert_eq!(RunId::new(""), Err(IdError::Empty));
        assert_eq!(RunId::new("a\nb"), Err(IdError::ControlCharacter));
        assert_eq!(RunId::new("a".repeat(129)), Err(IdError::TooLong));
        assert_eq!(RunId::new("run-1").expect("valid").as_str(), "run-1");
    }
}
