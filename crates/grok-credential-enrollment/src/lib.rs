#![deny(unsafe_code)]
#![warn(missing_docs)]

//! Native, renderer-free credential entry for the trusted daemon.
//!
//! Windows collects through the audited Win32 credential UI; unix daemons
//! collect through a local pinentry dialog. Both paths keep the entered key
//! inside the daemon process.

#[cfg(any(windows, test))]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use grok_application::{
    CredentialEnrollment, CredentialEnrollmentError, CredentialEnrollmentRequest, SecretValue,
};
#[cfg(windows)]
use tokio::sync::oneshot;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MIN_ATTEMPT_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(unix)]
mod unix;

#[cfg(all(test, unix))]
mod linux_ga_qualification;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

/// Serializes native credential UI and keeps prompt authority inside the daemon.
#[derive(Debug)]
pub struct NativeCredentialEnrollment {
    coordinator: PromptCoordinator,
    #[cfg(windows)]
    integrity_failed: AtomicBool,
}

impl NativeCredentialEnrollment {
    /// Creates the native enrollment adapter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            coordinator: PromptCoordinator::new(MIN_ATTEMPT_INTERVAL),
            #[cfg(windows)]
            integrity_failed: AtomicBool::new(false),
        }
    }

    #[cfg(windows)]
    async fn collect(
        &self,
        parent_window_token: u64,
    ) -> Result<SecretValue, CredentialEnrollmentError> {
        if self.integrity_failed.load(Ordering::Acquire) {
            return Err(CredentialEnrollmentError::Integrity);
        }
        let permit = self.coordinator.reserve()?;

        let (sender, receiver) = oneshot::channel();
        std::thread::Builder::new()
            .name("grok-native-credential-ui".into())
            .spawn(move || {
                let _permit = permit;
                let result = windows::prompt_xai_api_key(parent_window_token);
                // A cancelled RPC drops the receiver; the returned SecretValue then
                // drops here and zeroizes without crossing another process boundary.
                let _ = sender.send(result);
            })
            .map_err(|_| CredentialEnrollmentError::Unavailable)?;
        let result = receiver
            .await
            .map_err(|_| CredentialEnrollmentError::Unavailable)?;
        if matches!(result, Err(CredentialEnrollmentError::Integrity)) {
            self.integrity_failed.store(true, Ordering::Release);
        }
        result
    }
}

#[derive(Debug)]
struct PromptCoordinator {
    gate: Arc<Semaphore>,
    last_attempt: Mutex<Option<Instant>>,
    minimum_interval: Duration,
}

impl PromptCoordinator {
    fn new(minimum_interval: Duration) -> Self {
        Self {
            gate: Arc::new(Semaphore::new(1)),
            last_attempt: Mutex::new(None),
            minimum_interval,
        }
    }

    fn reserve(&self) -> Result<OwnedSemaphorePermit, CredentialEnrollmentError> {
        let permit = Arc::clone(&self.gate)
            .try_acquire_owned()
            .map_err(|_| CredentialEnrollmentError::Unavailable)?;
        let mut last_attempt = self
            .last_attempt
            .lock()
            .map_err(|_| CredentialEnrollmentError::Unavailable)?;
        let now = Instant::now();
        if last_attempt.is_some_and(|last| now.duration_since(last) < self.minimum_interval) {
            return Err(CredentialEnrollmentError::Unavailable);
        }
        // Reserve before native dispatch so cancellation cannot bypass cooldown.
        *last_attempt = Some(now);
        Ok(permit)
    }
}

impl Default for NativeCredentialEnrollment {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CredentialEnrollment for NativeCredentialEnrollment {
    async fn collect_xai_api_key(
        &self,
        request: CredentialEnrollmentRequest,
    ) -> Result<SecretValue, CredentialEnrollmentError> {
        #[cfg(windows)]
        {
            if request.parent_window_token == 0 {
                return Err(CredentialEnrollmentError::Unavailable);
            }
            return self.collect(request.parent_window_token).await;
        }
        #[cfg(unix)]
        {
            // Wayland/X11 window parenting is not portable; pinentry renders
            // its own dialog, so the token is intentionally unused here.
            let _ = request;
            let _permit = self.coordinator.reserve()?;
            return unix::prompt_xai_api_key().await;
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = request;
            Err(CredentialEnrollmentError::Unavailable)
        }
    }
}

#[cfg(any(windows, test))]
fn expected_owner_executable(daemon_executable: &Path) -> Option<PathBuf> {
    if !daemon_executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("grok-daemon.exe"))
    {
        return None;
    }
    let bin = daemon_executable.parent()?;
    if !bin
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        return None;
    }
    let resources = bin.parent()?;
    if !resources
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("resources"))
    {
        return None;
    }
    Some(resources.parent()?.join("Grok Desktop.exe"))
}

#[cfg(any(windows, test))]
fn ascii_secret_length(source: &[u16]) -> Option<usize> {
    let length = source.iter().position(|value| *value == 0)?;
    if length == 0
        || !source[..length]
            .iter()
            .all(|value| (0x21..=0x7e).contains(value))
    {
        return None;
    }
    Some(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_layout_has_one_exact_window_owner() {
        assert_eq!(
            expected_owner_executable(Path::new("/package/resources/bin/grok-daemon.exe")),
            Some(PathBuf::from("/package/Grok Desktop.exe"))
        );
        assert!(expected_owner_executable(Path::new("/tmp/grok-daemon.exe")).is_none());
        assert!(
            expected_owner_executable(Path::new("/package/resources/bin/renamed.exe")).is_none()
        );
    }

    #[test]
    fn credential_text_is_bounded_printable_ascii() {
        let mut valid = vec![0_u16; 32];
        for (target, source) in valid.iter_mut().zip("xai-key_123".encode_utf16()) {
            *target = source;
        }
        assert_eq!(ascii_secret_length(&valid), Some(11));

        valid[0] = u16::from(b' ');
        assert_eq!(ascii_secret_length(&valid), None);
        valid[0] = 0xd800;
        assert_eq!(ascii_secret_length(&valid), None);
        assert_eq!(ascii_secret_length(&[0; 4]), None);
        assert_eq!(ascii_secret_length(&[u16::from(b'x'); 4]), None);
    }

    /// Unix enrollment is exercised hermetically in `unix::tests` through fake
    /// pinentry scripts; invoking `collect_xai_api_key` here would open a real
    /// dialog on developer machines.
    #[cfg(windows)]
    #[tokio::test]
    async fn empty_owner_window_fails_closed() {
        let enrollment = NativeCredentialEnrollment::new();
        assert_eq!(
            enrollment
                .collect_xai_api_key(CredentialEnrollmentRequest {
                    parent_window_token: 0,
                })
                .await,
            Err(CredentialEnrollmentError::Unavailable)
        );
    }

    #[test]
    fn prompt_reservation_survives_caller_cancellation_and_enforces_cooldown() {
        let coordinator = PromptCoordinator::new(Duration::ZERO);
        let worker_permit = coordinator.reserve().expect("first reservation");
        assert_eq!(
            coordinator.reserve().expect_err("parallel prompt denied"),
            CredentialEnrollmentError::Unavailable
        );
        drop(worker_permit);
        drop(coordinator.reserve().expect("worker released reservation"));

        let cooldown = PromptCoordinator::new(Duration::from_secs(5));
        drop(cooldown.reserve().expect("first cooldown reservation"));
        assert_eq!(
            cooldown.reserve().expect_err("cooldown enforced"),
            CredentialEnrollmentError::Unavailable
        );
    }
}
