//! Daemon-owned `SuperGrok` OAuth enrollment and token rotation use cases.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

use crate::{ApplicationError, SecretName, SecretValue, SecretVault, VaultError};

/// Stable vault entry used for the daemon-owned `SuperGrok` grant.
pub const SUPERGROK_OAUTH_VAULT_NAME: &str = "supergrok.oauth.primary";
const TOKEN_FORMAT_VERSION: u8 = 1;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_SCOPE_BYTES: usize = 1024;
const MAX_SCOPE_COUNT: usize = 16;
const MAX_INDIVIDUAL_SCOPE_BYTES: usize = 128;
const MAX_DEVICE_CODE_BYTES: usize = 4096;
const MIN_POLL_INTERVAL_SECS: u64 = 1;
const MAX_POLL_INTERVAL_SECS: u64 = 60;
const SLOW_DOWN_INCREMENT_SECS: u64 = 5;

/// Device authorization data. The device code is secret and never printed.
pub struct DeviceAuthorization {
    /// Official verification page the user should open.
    pub verification_uri: String,
    /// Short code the user enters on the verification page.
    pub user_code: String,
    /// Absolute Unix-millisecond deadline.
    pub expires_at_ms: i64,
    /// Server-directed polling interval after applying the safe minimum.
    pub interval_secs: u64,
    device_code: Zeroizing<String>,
}

impl DeviceAuthorization {
    /// Creates bounded device authorization state returned by a trusted adapter.
    ///
    /// # Errors
    ///
    /// Returns [`OAuthFailure::InvalidResponse`] for empty or oversized fields,
    /// or for a non-positive expiry.
    pub fn new(
        verification_uri: String,
        user_code: String,
        device_code: String,
        expires_at_ms: i64,
        interval_secs: u64,
    ) -> Result<Self, OAuthFailure> {
        if verification_uri.is_empty()
            || verification_uri.len() > 2048
            || user_code.is_empty()
            || user_code.len() > 128
            || device_code.is_empty()
            || device_code.len() > MAX_DEVICE_CODE_BYTES
            || expires_at_ms <= 0
        {
            return Err(OAuthFailure::InvalidResponse);
        }
        Ok(Self {
            verification_uri,
            user_code,
            expires_at_ms,
            interval_secs: interval_secs.clamp(MIN_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS),
            device_code: Zeroizing::new(device_code),
        })
    }
}

impl fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceAuthorization")
            .field("verification_uri", &self.verification_uri)
            .field("user_code", &self.user_code)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("interval_secs", &self.interval_secs)
            .field("device_code", &"[REDACTED]")
            .finish()
    }
}

/// A token grant returned by the trusted OAuth adapter.
pub struct OAuthTokenGrant {
    /// Short-lived bearer token. Never crosses IPC.
    pub access_token: Zeroizing<String>,
    /// Rotating refresh token. Never crosses IPC.
    pub refresh_token: Zeroizing<String>,
    /// Absolute Unix-millisecond access-token expiry.
    pub expires_at_ms: i64,
    /// Granted OAuth scopes.
    pub scopes: Vec<String>,
}

impl fmt::Debug for OAuthTokenGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthTokenGrant([REDACTED])")
    }
}

/// Sanitized OAuth boundary failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFailure {
    /// Device authorization has not completed.
    Pending,
    /// Provider requested a five-second polling slowdown.
    SlowDown,
    /// User or provider denied the authorization.
    Denied,
    /// Authorization state expired.
    Expired,
    /// Caller cancelled the operation.
    Cancelled,
    /// Credential was rejected.
    Unauthorized,
    /// Provider transport is temporarily unavailable.
    Unavailable,
    /// Provider returned malformed or out-of-contract data.
    InvalidResponse,
}

/// OAuth operations implemented by the official xAI transport adapter.
#[async_trait]
pub trait SuperGrokOAuth: Send + Sync {
    /// Starts an RFC 8628 device authorization.
    async fn begin_device_authorization(
        &self,
        now_ms: i64,
    ) -> Result<DeviceAuthorization, OAuthFailure>;
    /// Polls the device grant once using the secret device code.
    async fn poll_device_token(
        &self,
        device_code: &str,
        now_ms: i64,
    ) -> Result<OAuthTokenGrant, OAuthFailure>;
    /// Exchanges the current rotating refresh token for a new grant.
    async fn refresh_token(
        &self,
        refresh_token: &str,
        now_ms: i64,
    ) -> Result<OAuthTokenGrant, OAuthFailure>;
}

/// Cooperative cancellation owned by the daemon request lifecycle.
#[derive(Default)]
struct OAuthCancellationState {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}

/// Cooperative cancellation owned by the daemon request lifecycle.
#[derive(Clone, Default)]
pub struct OAuthCancellation(Arc<OAuthCancellationState>);
impl OAuthCancellation {
    /// Requests cancellation. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }
    /// Reports whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.0.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// Non-secret enrollment progress suitable for projection across IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuperGrokEnrollmentStatus {
    /// Enrollment is waiting for the user to authorize the device.
    AwaitingUser {
        /// Official verification page.
        verification_uri: String,
        /// Short code entered by the user.
        user_code: String,
        /// Absolute Unix-millisecond deadline.
        expires_at_ms: i64,
    },
    /// A grant was committed to the daemon vault.
    Connected {
        /// Absolute Unix-millisecond access-token expiry.
        expires_at_ms: i64,
        /// Monotonic local credential generation.
        generation: u64,
    },
}

/// Short-lived daemon credential. Debug output is always redacted.
pub struct SuperGrokCredential {
    /// Short-lived bearer token for immediate trusted-adapter use.
    pub access_token: Zeroizing<String>,
    /// Absolute Unix-millisecond expiry.
    pub expires_at_ms: i64,
    /// Monotonic local credential generation.
    pub generation: u64,
}
impl fmt::Debug for SuperGrokCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SuperGrokCredential([REDACTED])")
    }
}

#[derive(Serialize, Deserialize)]
struct StoredToken {
    version: u8,
    access_token: String,
    refresh_token: String,
    expires_at_ms: i64,
    scopes: Vec<String>,
    generation: u64,
}
impl Drop for StoredToken {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
        for scope in &mut self.scopes {
            scope.zeroize();
        }
    }
}

/// Coordinates enrollment, vault persistence, refresh serialization, and disconnect.
pub struct SuperGrokEnrollmentService {
    oauth: Arc<dyn SuperGrokOAuth>,
    vault: Arc<dyn SecretVault>,
    vault_name: SecretName,
    refresh_gate: Mutex<()>,
    sleeper: Arc<dyn PollSleeper>,
}

#[async_trait]
trait PollSleeper: Send + Sync {
    async fn sleep(&self, duration: std::time::Duration);
}

struct TokioPollSleeper;
#[async_trait]
impl PollSleeper for TokioPollSleeper {
    async fn sleep(&self, duration: std::time::Duration) {
        tokio::time::sleep(duration).await;
    }
}

impl SuperGrokEnrollmentService {
    /// Creates a service over an official OAuth adapter and daemon-owned vault.
    ///
    /// # Errors
    ///
    /// Returns an integrity error if the compile-time vault name violates the
    /// application vault-name contract.
    pub fn new(
        oauth: Arc<dyn SuperGrokOAuth>,
        vault: Arc<dyn SecretVault>,
    ) -> Result<Self, ApplicationError> {
        Ok(Self {
            oauth,
            vault,
            vault_name: SecretName::new(SUPERGROK_OAUTH_VAULT_NAME)
                .map_err(|_| ApplicationError::Integrity("OAuth vault name is invalid".into()))?,
            refresh_gate: Mutex::new(()),
            sleeper: Arc::new(TokioPollSleeper),
        })
    }

    /// Starts a bounded device authorization.
    ///
    /// # Errors
    ///
    /// Returns a sanitized application error when the provider is unavailable
    /// or returns invalid/expired authorization state.
    pub async fn begin_device(&self, now_ms: i64) -> Result<DeviceAuthorization, ApplicationError> {
        let authorization = self
            .oauth
            .begin_device_authorization(now_ms)
            .await
            .map_err(map_oauth)?;
        if authorization.expires_at_ms <= now_ms {
            return Err(ApplicationError::Integrity(
                "OAuth provider returned expired device authorization".into(),
            ));
        }
        Ok(authorization)
    }

    /// Polls until success, terminal failure, cancellation, or the authorization deadline.
    ///
    /// # Errors
    ///
    /// Returns a sanitized cancellation, deadline, provider, validation, or
    /// vault error. A failed vault write never reports a connected status.
    pub async fn complete_device(
        &self,
        authorization: &DeviceAuthorization,
        cancellation: &OAuthCancellation,
    ) -> Result<SuperGrokEnrollmentStatus, ApplicationError> {
        let mut interval = authorization
            .interval_secs
            .clamp(MIN_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS);
        loop {
            if cancellation.is_cancelled() {
                return Err(ApplicationError::Cancelled);
            }
            let now_ms = unix_time_ms()?;
            if now_ms >= authorization.expires_at_ms {
                return Err(ApplicationError::DeadlineExceeded);
            }
            match self
                .oauth
                .poll_device_token(&authorization.device_code, now_ms)
                .await
            {
                Ok(grant) => return self.persist_grant(grant, 1, now_ms),
                Err(OAuthFailure::Pending) => {}
                Err(OAuthFailure::SlowDown) => {
                    interval = interval
                        .saturating_add(SLOW_DOWN_INCREMENT_SECS)
                        .min(MAX_POLL_INTERVAL_SECS);
                }
                Err(error) => return Err(map_oauth(error)),
            }
            let remaining_ms = authorization.expires_at_ms.saturating_sub(unix_time_ms()?);
            let sleep_ms = i64::try_from(interval.saturating_mul(1000))
                .unwrap_or(i64::MAX)
                .min(remaining_ms);
            let sleep = self.sleeper.sleep(std::time::Duration::from_millis(
                u64::try_from(sleep_ms).unwrap_or(0),
            ));
            tokio::select! {
                () = sleep => {}
                () = cancellation.cancelled() => return Err(ApplicationError::Cancelled),
            }
        }
    }

    /// Loads a credential and refreshes it once, under a single-flight gate, when requested.
    ///
    /// # Errors
    ///
    /// Returns a sanitized vault, provider, integrity, or generation error.
    pub async fn credential(
        &self,
        now_ms: i64,
        refresh_before_ms: i64,
    ) -> Result<SuperGrokCredential, ApplicationError> {
        let token = self.load()?;
        if token.expires_at_ms.saturating_sub(now_ms) > refresh_before_ms {
            return Ok(to_credential(&token));
        }
        drop(token);
        let _guard = self.refresh_gate.lock().await;
        let token = self.load()?;
        if token.expires_at_ms.saturating_sub(now_ms) > refresh_before_ms {
            return Ok(to_credential(&token));
        }
        let next_generation = token.generation.checked_add(1).ok_or_else(|| {
            ApplicationError::InvalidState("OAuth credential generation exhausted".into())
        })?;
        let grant = self
            .oauth
            .refresh_token(&token.refresh_token, now_ms)
            .await
            .map_err(map_oauth)?;
        let SuperGrokEnrollmentStatus::Connected { .. } =
            self.persist_grant(grant, next_generation, now_ms)?
        else {
            unreachable!()
        };
        Ok(to_credential(&self.load()?))
    }

    /// Deletes the daemon-owned grant. Missing state is treated as disconnected.
    ///
    /// # Errors
    ///
    /// Returns a sanitized vault error when deletion cannot be completed.
    pub fn disconnect(&self) -> Result<(), ApplicationError> {
        match self.vault.delete(&self.vault_name) {
            Ok(()) | Err(VaultError::NotFound) => Ok(()),
            Err(error) => Err(map_vault(error)),
        }
    }

    fn persist_grant(
        &self,
        grant: OAuthTokenGrant,
        generation: u64,
        now_ms: i64,
    ) -> Result<SuperGrokEnrollmentStatus, ApplicationError> {
        validate_grant(&grant, now_ms)?;
        let stored = StoredToken {
            version: TOKEN_FORMAT_VERSION,
            access_token: grant.access_token.to_string(),
            refresh_token: grant.refresh_token.to_string(),
            expires_at_ms: grant.expires_at_ms,
            scopes: grant.scopes,
            generation,
        };
        let mut bytes = serde_json::to_vec(&stored).map_err(|_| {
            ApplicationError::Integrity("OAuth credential serialization failed".into())
        })?;
        if bytes.len() > MAX_TOKEN_BYTES {
            bytes.zeroize();
            return Err(ApplicationError::Integrity(
                "OAuth credential exceeds storage bound".into(),
            ));
        }
        let secret = SecretValue::new(bytes).map_err(map_vault)?;
        self.vault
            .set(&self.vault_name, &secret)
            .map_err(map_vault)?;
        Ok(SuperGrokEnrollmentStatus::Connected {
            expires_at_ms: stored.expires_at_ms,
            generation,
        })
    }

    fn load(&self) -> Result<StoredToken, ApplicationError> {
        let secret = self.vault.get(&self.vault_name).map_err(map_vault)?;
        if secret.expose_secret().len() > MAX_TOKEN_BYTES {
            return Err(ApplicationError::Integrity(
                "OAuth credential exceeds storage bound".into(),
            ));
        }
        let token: StoredToken = serde_json::from_slice(secret.expose_secret())
            .map_err(|_| ApplicationError::Integrity("OAuth credential is malformed".into()))?;
        if token.version != TOKEN_FORMAT_VERSION || token.generation == 0 {
            return Err(ApplicationError::Integrity(
                "OAuth credential version is invalid".into(),
            ));
        }
        validate_stored(&token)?;
        Ok(token)
    }
}

fn validate_grant(grant: &OAuthTokenGrant, now_ms: i64) -> Result<(), ApplicationError> {
    if !valid_token(&grant.access_token)
        || !valid_token(&grant.refresh_token)
        || grant.expires_at_ms <= now_ms
        || !valid_scopes(&grant.scopes)
    {
        return Err(ApplicationError::Integrity(
            "OAuth provider returned an invalid token grant".into(),
        ));
    }
    Ok(())
}
fn validate_stored(token: &StoredToken) -> Result<(), ApplicationError> {
    if !valid_token(&token.access_token)
        || !valid_token(&token.refresh_token)
        || token.expires_at_ms <= 0
        || !valid_scopes(&token.scopes)
    {
        return Err(ApplicationError::Integrity(
            "OAuth credential is invalid".into(),
        ));
    }
    Ok(())
}
fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}
fn valid_scopes(scopes: &[String]) -> bool {
    !scopes.is_empty()
        && scopes.len() <= MAX_SCOPE_COUNT
        && scopes.iter().map(String::len).sum::<usize>() <= MAX_SCOPE_BYTES
        && scopes.iter().all(|scope| {
            !scope.is_empty()
                && scope.len() <= MAX_INDIVIDUAL_SCOPE_BYTES
                && scope.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-')
                })
        })
        && scopes.iter().any(|scope| scope == "api:access")
}
fn to_credential(token: &StoredToken) -> SuperGrokCredential {
    SuperGrokCredential {
        access_token: Zeroizing::new(token.access_token.clone()),
        expires_at_ms: token.expires_at_ms,
        generation: token.generation,
    }
}
fn unix_time_ms() -> Result<i64, ApplicationError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .map_err(|_| ApplicationError::Integrity("system clock precedes Unix epoch".into()))
}
fn map_vault(error: VaultError) -> ApplicationError {
    match error {
        VaultError::NotFound => ApplicationError::NotFound,
        VaultError::Unavailable => ApplicationError::Unavailable("secure vault unavailable".into()),
        VaultError::InvalidName | VaultError::InvalidValue | VaultError::Internal => {
            ApplicationError::Storage("secure vault operation failed".into())
        }
    }
}
fn map_oauth(error: OAuthFailure) -> ApplicationError {
    match error {
        OAuthFailure::Cancelled => ApplicationError::Cancelled,
        OAuthFailure::Expired => ApplicationError::DeadlineExceeded,
        OAuthFailure::Denied | OAuthFailure::Unauthorized => {
            ApplicationError::Unauthorized("SuperGrok authorization rejected".into())
        }
        OAuthFailure::InvalidResponse => {
            ApplicationError::Integrity("OAuth provider returned an invalid response".into())
        }
        OAuthFailure::Unavailable => {
            ApplicationError::Unavailable("OAuth provider unavailable".into())
        }
        OAuthFailure::Pending | OAuthFailure::SlowDown => {
            ApplicationError::InvalidState("OAuth authorization is still pending".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{HashMap, VecDeque},
        sync::Mutex as StdMutex,
    };

    #[derive(Default)]
    struct MemoryVault(StdMutex<HashMap<String, SecretValue>>);
    impl SecretVault for MemoryVault {
        fn get(&self, n: &SecretName) -> Result<SecretValue, VaultError> {
            self.0
                .lock()
                .unwrap()
                .get(n.as_str())
                .cloned()
                .ok_or(VaultError::NotFound)
        }
        fn set(&self, n: &SecretName, v: &SecretValue) -> Result<(), VaultError> {
            self.0.lock().unwrap().insert(n.as_str().into(), v.clone());
            Ok(())
        }
        fn delete(&self, n: &SecretName) -> Result<(), VaultError> {
            self.0.lock().unwrap().remove(n.as_str());
            Ok(())
        }
    }
    struct FakeOAuth {
        polls: StdMutex<VecDeque<Result<OAuthTokenGrant, OAuthFailure>>>,
        refreshes: StdMutex<u32>,
    }
    fn grant(access: &str, refresh: &str, expiry: i64) -> OAuthTokenGrant {
        OAuthTokenGrant {
            access_token: Zeroizing::new(access.into()),
            refresh_token: Zeroizing::new(refresh.into()),
            expires_at_ms: expiry,
            scopes: vec!["api:access".into()],
        }
    }
    #[async_trait]
    impl SuperGrokOAuth for FakeOAuth {
        async fn begin_device_authorization(
            &self,
            _now: i64,
        ) -> Result<DeviceAuthorization, OAuthFailure> {
            DeviceAuthorization::new(
                "https://auth.x.ai/device".into(),
                "ABCD".into(),
                "secret-device".into(),
                i64::MAX,
                1,
            )
        }
        async fn poll_device_token(
            &self,
            _: &str,
            _: i64,
        ) -> Result<OAuthTokenGrant, OAuthFailure> {
            self.polls
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(OAuthFailure::Pending))
        }
        async fn refresh_token(&self, _: &str, now: i64) -> Result<OAuthTokenGrant, OAuthFailure> {
            *self.refreshes.lock().unwrap() += 1;
            Ok(grant(
                "new-access",
                "new-refresh",
                now.saturating_add(60_000),
            ))
        }
    }
    fn service(oauth: Arc<FakeOAuth>, vault: Arc<MemoryVault>) -> SuperGrokEnrollmentService {
        SuperGrokEnrollmentService::new(oauth, vault).unwrap()
    }

    #[derive(Default)]
    struct RecordingSleeper(StdMutex<Vec<std::time::Duration>>);
    #[async_trait]
    impl PollSleeper for RecordingSleeper {
        async fn sleep(&self, duration: std::time::Duration) {
            self.0.lock().unwrap().push(duration);
        }
    }

    #[derive(Default)]
    struct BlockingSleeper(tokio::sync::Notify);
    #[async_trait]
    impl PollSleeper for BlockingSleeper {
        async fn sleep(&self, _duration: std::time::Duration) {
            self.0.notified().await;
        }
    }

    #[test]
    fn secrets_are_redacted_and_bounded() {
        let device = DeviceAuthorization::new(
            "https://auth.x.ai/device".into(),
            "ABCD".into(),
            "secret-device".into(),
            10,
            0,
        )
        .unwrap();
        assert!(!format!("{device:?}").contains("secret-device"));
        assert_eq!(device.interval_secs, 1);
        let capped = DeviceAuthorization::new(
            "https://auth.x.ai/device".into(),
            "ABCD".into(),
            "secret-device".into(),
            10,
            86_400,
        )
        .unwrap();
        assert_eq!(capped.interval_secs, MAX_POLL_INTERVAL_SECS);
        assert_eq!(
            format!("{:?}", grant("access", "refresh", 1)),
            "OAuthTokenGrant([REDACTED])"
        );
        assert!(DeviceAuthorization::new("x".into(), "x".into(), "x".into(), 0, 1).is_err());
    }

    #[test]
    fn rejects_malformed_grants_and_stored_credentials() {
        let oauth = Arc::new(FakeOAuth {
            polls: StdMutex::new(VecDeque::new()),
            refreshes: StdMutex::new(0),
        });
        let vault = Arc::new(MemoryVault::default());
        let service = service(oauth, vault.clone());

        let mut missing_scope = grant("access", "refresh", 100);
        missing_scope.scopes = vec!["openid".into()];
        assert!(service.persist_grant(missing_scope, 1, 1).is_err());
        assert!(
            service
                .persist_grant(grant("bad token", "refresh", 100), 1, 1)
                .is_err()
        );
        assert!(
            service
                .persist_grant(grant("access", "refresh", 1), 1, 1)
                .is_err()
        );

        let malformed = SecretValue::new(
            br#"{"version":1,"access_token":"access","refresh_token":"refresh","expires_at_ms":100,"scopes":["openid"],"generation":1}"#.to_vec(),
        )
        .unwrap();
        vault
            .set(
                &SecretName::new(SUPERGROK_OAUTH_VAULT_NAME).unwrap(),
                &malformed,
            )
            .unwrap();
        assert!(matches!(
            service.load(),
            Err(ApplicationError::Integrity(_))
        ));
    }

    #[tokio::test]
    async fn slow_down_increases_the_next_poll_delay_without_waiting() {
        let oauth = Arc::new(FakeOAuth {
            polls: StdMutex::new(VecDeque::from([
                Err(OAuthFailure::SlowDown),
                Ok(grant("access", "refresh", i64::MAX)),
            ])),
            refreshes: StdMutex::new(0),
        });
        let sleeper = Arc::new(RecordingSleeper::default());
        let mut service = service(oauth, Arc::new(MemoryVault::default()));
        service.sleeper = sleeper.clone();
        let authorization = service.begin_device(1).await.unwrap();
        service
            .complete_device(&authorization, &OAuthCancellation::default())
            .await
            .unwrap();
        assert_eq!(
            *sleeper.0.lock().unwrap(),
            vec![std::time::Duration::from_secs(6)]
        );
    }

    #[tokio::test]
    async fn cancellation_wakes_a_poll_sleep_promptly() {
        let oauth = Arc::new(FakeOAuth {
            polls: StdMutex::new(VecDeque::from([Err(OAuthFailure::Pending)])),
            refreshes: StdMutex::new(0),
        });
        let mut service = service(oauth, Arc::new(MemoryVault::default()));
        service.sleeper = Arc::new(BlockingSleeper::default());
        let service = Arc::new(service);
        let authorization = Arc::new(service.begin_device(1).await.unwrap());
        let cancellation = OAuthCancellation::default();
        let task = tokio::spawn({
            let service = service.clone();
            let authorization = authorization.clone();
            let cancellation = cancellation.clone();
            async move { service.complete_device(&authorization, &cancellation).await }
        });
        tokio::task::yield_now().await;
        cancellation.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_millis(100), task)
            .await
            .expect("cancellation must wake the sleep")
            .expect("poll task");
        assert!(matches!(result, Err(ApplicationError::Cancelled)));
    }

    #[tokio::test]
    async fn persists_loads_rotates_and_disconnects() {
        let oauth = Arc::new(FakeOAuth {
            polls: StdMutex::new(VecDeque::from([Ok(grant("access", "refresh", i64::MAX))])),
            refreshes: StdMutex::new(0),
        });
        let vault = Arc::new(MemoryVault::default());
        let service = service(oauth.clone(), vault);
        let auth = service.begin_device(1).await.unwrap();
        let status = service
            .complete_device(&auth, &OAuthCancellation::default())
            .await
            .unwrap();
        assert!(matches!(
            status,
            SuperGrokEnrollmentStatus::Connected { generation: 1, .. }
        ));
        let credential = service.credential(10, 0).await.unwrap();
        assert_eq!(&*credential.access_token, "access");
        assert_eq!(credential.generation, 1);
        let refreshed = service.credential(i64::MAX - 1, i64::MAX).await.unwrap();
        assert_eq!(&*refreshed.access_token, "new-access");
        assert_eq!(refreshed.generation, 2);
        assert_eq!(*oauth.refreshes.lock().unwrap(), 1);
        service.disconnect().unwrap();
        assert!(matches!(
            service.credential(0, 0).await,
            Err(ApplicationError::NotFound)
        ));
    }

    #[tokio::test]
    async fn cancellation_fails_closed() {
        let oauth = Arc::new(FakeOAuth {
            polls: StdMutex::new(VecDeque::new()),
            refreshes: StdMutex::new(0),
        });
        let service = service(oauth, Arc::new(MemoryVault::default()));
        let auth = service.begin_device(unix_time_ms().unwrap()).await.unwrap();
        let cancel = OAuthCancellation::default();
        cancel.cancel();
        assert!(matches!(
            service.complete_device(&auth, &cancel).await,
            Err(ApplicationError::Cancelled)
        ));
    }
}
