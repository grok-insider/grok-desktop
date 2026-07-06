use sha2::{Digest, Sha256};

use crate::{ApplicationError, MutationCommand};

pub(crate) fn mutation_command(
    scope: &str,
    key: &str,
    parts: &[String],
) -> Result<MutationCommand, ApplicationError> {
    let parts = parts.iter().map(String::as_bytes).collect::<Vec<_>>();
    mutation_command_bytes(scope, key, &parts)
}

pub(crate) fn mutation_command_bytes(
    scope: &str,
    key: &str,
    parts: &[&[u8]],
) -> Result<MutationCommand, ApplicationError> {
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        return Err(ApplicationError::InvalidInput(
            "a bounded idempotency key is required".into(),
        ));
    }
    let mut hasher = Sha256::new();
    hash_part(&mut hasher, scope.as_bytes());
    for part in parts {
        hash_part(&mut hasher, part);
    }
    Ok(MutationCommand {
        scope: scope.into(),
        key: key.into(),
        fingerprint: hasher.finalize().into(),
    })
}

fn hash_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}
