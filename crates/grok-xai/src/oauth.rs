//! Fixed-origin xAI OAuth primitives for `SuperGrok` API access.
//!
//! This adapter performs a fresh authorization grant. It never imports Grok
//! CLI credentials and never sends subscription tokens to the CLI chat proxy.

use std::{fmt, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use grok_application::{
    DeviceAuthorization as ApplicationDeviceAuthorization, OAuthFailure, OAuthTokenGrant,
    SecretValue, SuperGrokOAuth,
};
use reqwest::StatusCode;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::{Zeroize, Zeroizing};

/// Public xAI client shipped for generic desktop/CLI authorization.
pub const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
/// Scopes required for identity, refresh, Grok entitlement, and xAI API use.
pub const XAI_OAUTH_SCOPES: &str = "openid profile email offline_access grok-cli:access api:access";
/// Registered loopback callback used by the public client.
pub const XAI_OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";

const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const DEVICE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_TOKEN_BYTES: usize = 64 * 1024;

/// PKCE/state material retained only while one browser enrollment is pending.
pub struct BrowserAuthorization {
    /// Fixed-origin authorization URL to open in the user's browser.
    pub url: Url,
    verifier: Zeroizing<String>,
    state: Zeroizing<String>,
}

impl fmt::Debug for BrowserAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserAuthorization")
            .field("url", &self.url)
            .field("verifier", &"[REDACTED]")
            .field("state", &"[REDACTED]")
            .finish()
    }
}

impl BrowserAuthorization {
    /// Validates the callback state and returns the verifier for immediate exchange.
    ///
    /// # Errors
    ///
    /// Returns an invalid-callback error when state does not match, or a
    /// protocol error when the verifier cannot cross the secret boundary.
    pub fn complete(mut self, returned_state: &str) -> Result<SecretValue, OAuthError> {
        if !constant_time_equal(self.state.as_bytes(), returned_state.as_bytes()) {
            return Err(OAuthError::InvalidCallback);
        }
        let bytes = self.verifier.as_bytes().to_vec();
        self.state.zeroize();
        SecretValue::new(bytes).map_err(|_| OAuthError::Protocol)
    }
}

/// Non-secret device authorization projection safe for local IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorization {
    /// Verification page the user can open on another device.
    pub verification_uri: Url,
    /// Verification page with the user code prefilled, when supplied by xAI.
    pub verification_uri_complete: Option<Url>,
    /// Short non-secret code displayed to the user.
    pub user_code: String,
    /// Maximum lifetime of the device authorization.
    pub expires_in: Duration,
    /// Initial server-requested polling interval.
    pub interval: Duration,
    device_code: SecretValue,
}

impl DeviceAuthorization {
    /// Borrows the device code only for the trusted token poller.
    #[must_use]
    pub fn device_code(&self) -> &SecretValue {
        &self.device_code
    }
}

/// OAuth tokens retained only by the daemon vault boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthTokenSet {
    /// Short-lived access token.
    pub access: SecretValue,
    /// Rotating refresh token.
    pub refresh: SecretValue,
    /// Lifetime reported for the access token.
    pub expires_in: Duration,
    /// Granted scope string, when returned by xAI.
    pub scopes: Option<String>,
}

impl fmt::Debug for OAuthTokenSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthTokenSet")
            .field("access", &"[REDACTED]")
            .field("refresh", &"[REDACTED]")
            .field("expires_in", &self.expires_in)
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// Stable, response-body-free OAuth failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    /// The user or authorization server rejected the grant.
    #[error("xAI authorization was rejected")]
    Rejected,
    /// Device authorization has not completed yet.
    #[error("xAI authorization is still pending")]
    Pending,
    /// Device polling must slow down before the next attempt.
    #[error("xAI requested a slower authorization poll interval")]
    SlowDown,
    /// The pending authorization is no longer usable.
    #[error("xAI authorization expired")]
    Expired,
    /// The loopback callback did not match the pending authorization.
    #[error("xAI authorization callback is invalid")]
    InvalidCallback,
    /// The server returned an invalid or unsafe shape.
    #[error("xAI authorization response is invalid")]
    Protocol,
    /// The authorization service could not be reached safely.
    #[error("xAI authorization is unavailable")]
    Unavailable,
}

/// Fixed-origin OAuth HTTP adapter. Alternate endpoints exist only in tests.
#[derive(Clone)]
pub struct XaiOAuthClient {
    http: reqwest::Client,
    authorize_url: Arc<str>,
    token_url: Arc<str>,
    device_url: Arc<str>,
}

impl fmt::Debug for XaiOAuthClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XaiOAuthClient")
            .finish_non_exhaustive()
    }
}

impl XaiOAuthClient {
    /// Constructs the production fixed-origin client.
    ///
    /// # Errors
    ///
    /// Returns unavailable when the hardened HTTP client cannot be built.
    pub fn new() -> Result<Self, OAuthError> {
        Self::with_endpoints(AUTHORIZE_URL, TOKEN_URL, DEVICE_URL)
    }

    fn with_endpoints(authorize: &str, token: &str, device: &str) -> Result<Self, OAuthError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("grok-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| OAuthError::Unavailable)?;
        Ok(Self {
            http,
            authorize_url: Arc::from(authorize),
            token_url: Arc::from(token),
            device_url: Arc::from(device),
        })
    }

    /// Creates a PKCE authorization URL with fresh state and nonce.
    ///
    /// # Errors
    ///
    /// Returns unavailable when secure randomness fails and protocol when the
    /// fixed authorization URL cannot be represented safely.
    pub fn browser_authorization(&self) -> Result<BrowserAuthorization, OAuthError> {
        let verifier = random_urlsafe(64)?;
        let state = random_urlsafe(32)?;
        let nonce = random_urlsafe(32)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse(&self.authorize_url).map_err(|_| OAuthError::Protocol)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", XAI_OAUTH_CLIENT_ID)
            .append_pair("redirect_uri", XAI_OAUTH_REDIRECT_URI)
            .append_pair("scope", XAI_OAUTH_SCOPES)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("plan", "generic")
            .append_pair("referrer", "grok-desktop");
        Ok(BrowserAuthorization {
            url,
            verifier: Zeroizing::new(verifier),
            state: Zeroizing::new(state),
        })
    }

    /// Starts an RFC 8628 device authorization.
    ///
    /// # Errors
    ///
    /// Returns a stable OAuth error for network, rejection, or malformed data.
    pub async fn request_device_code(&self) -> Result<DeviceAuthorization, OAuthError> {
        let response = self
            .http
            .post(self.device_url.as_ref())
            .form(&[
                ("client_id", XAI_OAUTH_CLIENT_ID),
                ("scope", XAI_OAUTH_SCOPES),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Unavailable)?;
        let value: DeviceResponse = bounded_json(response).await?;
        let expires = positive_seconds(value.expires_in, 300)?;
        let interval = positive_seconds(value.interval, 5)?.max(Duration::from_secs(1));
        Ok(DeviceAuthorization {
            verification_uri: verified_https_url(&value.verification_uri)?,
            verification_uri_complete: value
                .verification_uri_complete
                .as_deref()
                .map(verified_https_url)
                .transpose()?,
            user_code: bounded_visible(value.user_code, 128)?,
            expires_in: expires,
            interval,
            device_code: secret(value.device_code)?,
        })
    }

    /// Exchanges one device code. The application owns polling/backoff/deadline policy.
    ///
    /// # Errors
    ///
    /// Returns pending/slow-down/expired/rejected as reported by the fixed
    /// authorization service, or a stable transport/protocol error.
    pub async fn poll_device_token(&self, code: &SecretValue) -> Result<OAuthTokenSet, OAuthError> {
        let code = std::str::from_utf8(code.expose_secret()).map_err(|_| OAuthError::Protocol)?;
        let response = self
            .http
            .post(self.token_url.as_ref())
            .form(&[
                ("grant_type", DEVICE_GRANT),
                ("client_id", XAI_OAUTH_CLIENT_ID),
                ("device_code", code),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Unavailable)?;
        token_response(response).await
    }

    /// Exchanges an authorization code after callback state validation.
    ///
    /// # Errors
    ///
    /// Returns a stable OAuth error for an invalid secret, rejected grant,
    /// unavailable service, or malformed token response.
    pub async fn exchange_code(
        &self,
        code: &SecretValue,
        verifier: &SecretValue,
    ) -> Result<OAuthTokenSet, OAuthError> {
        let code = secret_text(code)?;
        let verifier = secret_text(verifier)?;
        let response = self
            .http
            .post(self.token_url.as_ref())
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", XAI_OAUTH_REDIRECT_URI),
                ("client_id", XAI_OAUTH_CLIENT_ID),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Unavailable)?;
        token_response(response).await
    }

    /// Refreshes a rotating token pair.
    ///
    /// # Errors
    ///
    /// Returns rejected when the grant is no longer usable, otherwise a stable
    /// unavailable/protocol error without provider response text.
    pub async fn refresh(&self, refresh: &SecretValue) -> Result<OAuthTokenSet, OAuthError> {
        let refresh = secret_text(refresh)?;
        let response = self
            .http
            .post(self.token_url.as_ref())
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", XAI_OAUTH_CLIENT_ID),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Unavailable)?;
        token_response(response).await
    }
}

#[async_trait::async_trait]
impl SuperGrokOAuth for XaiOAuthClient {
    async fn begin_device_authorization(
        &self,
        now_ms: i64,
    ) -> Result<ApplicationDeviceAuthorization, OAuthFailure> {
        let authorization = self.request_device_code().await.map_err(map_oauth_error)?;
        let device_code = secret_text(authorization.device_code())
            .map_err(map_oauth_error)?
            .to_owned();
        ApplicationDeviceAuthorization::new(
            authorization.verification_uri.to_string(),
            authorization.user_code,
            device_code,
            expiry_millis(now_ms, authorization.expires_in)?,
            authorization.interval.as_secs(),
        )
    }

    async fn poll_device_token(
        &self,
        device_code: &str,
        now_ms: i64,
    ) -> Result<OAuthTokenGrant, OAuthFailure> {
        let code = secret(device_code.to_owned()).map_err(map_oauth_error)?;
        let tokens = XaiOAuthClient::poll_device_token(self, &code)
            .await
            .map_err(map_oauth_error)?;
        token_grant(tokens, now_ms)
    }

    async fn refresh_token(
        &self,
        refresh_token: &str,
        now_ms: i64,
    ) -> Result<OAuthTokenGrant, OAuthFailure> {
        let refresh = secret(refresh_token.to_owned()).map_err(map_oauth_error)?;
        let tokens = self.refresh(&refresh).await.map_err(map_oauth_error)?;
        token_grant(tokens, now_ms)
    }
}

fn token_grant(tokens: OAuthTokenSet, now_ms: i64) -> Result<OAuthTokenGrant, OAuthFailure> {
    let access_token = Zeroizing::new(
        secret_text(&tokens.access)
            .map_err(map_oauth_error)?
            .to_owned(),
    );
    let refresh_token = Zeroizing::new(
        secret_text(&tokens.refresh)
            .map_err(map_oauth_error)?
            .to_owned(),
    );
    Ok(OAuthTokenGrant {
        access_token,
        refresh_token,
        expires_at_ms: expiry_millis(now_ms, tokens.expires_in)?,
        scopes: tokens
            .scopes
            .map(|scopes| scopes.split_ascii_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
    })
}

fn expiry_millis(now_ms: i64, lifetime: Duration) -> Result<i64, OAuthFailure> {
    let lifetime_ms =
        i64::try_from(lifetime.as_millis()).map_err(|_| OAuthFailure::InvalidResponse)?;
    now_ms
        .checked_add(lifetime_ms)
        .ok_or(OAuthFailure::InvalidResponse)
}

fn map_oauth_error(error: OAuthError) -> OAuthFailure {
    match error {
        OAuthError::Pending => OAuthFailure::Pending,
        OAuthError::SlowDown => OAuthFailure::SlowDown,
        OAuthError::Expired => OAuthFailure::Expired,
        OAuthError::Rejected => OAuthFailure::Denied,
        OAuthError::InvalidCallback | OAuthError::Protocol => OAuthFailure::InvalidResponse,
        OAuthError::Unavailable => OAuthFailure::Unavailable,
    }
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: Option<String>,
}

async fn token_response(response: reqwest::Response) -> Result<OAuthTokenSet, OAuthError> {
    if !response.status().is_success() {
        return Err(error_status(response).await);
    }
    let token: TokenResponse = bounded_json(response).await?;
    Ok(OAuthTokenSet {
        access: secret(token.access_token)?,
        refresh: secret(token.refresh_token.ok_or(OAuthError::Protocol)?)?,
        expires_in: positive_seconds(token.expires_in, 3600)?,
        scopes: token
            .scope
            .map(|value| bounded_visible(value, 4096))
            .transpose()?,
    })
}

async fn error_status(response: reqwest::Response) -> OAuthError {
    let status = response.status();
    let error = bounded_json::<ErrorResponse>(response)
        .await
        .ok()
        .and_then(|body| body.error);
    match error.as_deref() {
        Some("authorization_pending") => OAuthError::Pending,
        Some("slow_down") => OAuthError::SlowDown,
        Some("expired_token") => OAuthError::Expired,
        Some("access_denied" | "authorization_denied" | "invalid_grant") => OAuthError::Rejected,
        _ if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
            OAuthError::Rejected
        }
        _ => OAuthError::Unavailable,
    }
}

async fn bounded_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, OAuthError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_TOKEN_BYTES as u64)
    {
        return Err(OAuthError::Protocol);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| OAuthError::Unavailable)?;
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err(OAuthError::Protocol);
    }
    serde_json::from_slice(&bytes).map_err(|_| OAuthError::Protocol)
}

fn random_urlsafe(bytes: usize) -> Result<String, OAuthError> {
    let mut value = vec![0_u8; bytes];
    getrandom::fill(&mut value).map_err(|_| OAuthError::Unavailable)?;
    let encoded = URL_SAFE_NO_PAD.encode(&value);
    value.zeroize();
    Ok(encoded)
}

fn secret(value: String) -> Result<SecretValue, OAuthError> {
    if value.len() > MAX_TOKEN_BYTES || value.chars().any(char::is_control) {
        return Err(OAuthError::Protocol);
    }
    SecretValue::new(value.into_bytes()).map_err(|_| OAuthError::Protocol)
}

fn secret_text(value: &SecretValue) -> Result<&str, OAuthError> {
    std::str::from_utf8(value.expose_secret()).map_err(|_| OAuthError::Protocol)
}

fn positive_seconds(value: Option<u64>, default: u64) -> Result<Duration, OAuthError> {
    let seconds = value.unwrap_or(default);
    if seconds == 0 || seconds > 86_400 {
        return Err(OAuthError::Protocol);
    }
    Ok(Duration::from_secs(seconds))
}

fn bounded_visible(value: String, maximum: usize) -> Result<String, OAuthError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(OAuthError::Protocol);
    }
    Ok(value)
}

fn verified_https_url(value: &str) -> Result<Url, OAuthError> {
    let url = Url::parse(value).map_err(|_| OAuthError::Protocol)?;
    let approved_host = matches!(url.host_str(), Some("x.ai" | "accounts.x.ai" | "auth.x.ai"));
    if url.scheme() != "https" || !approved_host || url.username() != "" || url.password().is_some()
    {
        return Err(OAuthError::Protocol);
    }
    Ok(url)
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_authorization_uses_pkce_and_fixed_contract() {
        let client = XaiOAuthClient::new().expect("client");
        let pending = client.browser_authorization().expect("authorization");
        let query = pending
            .url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(pending.url.as_str().split('?').next(), Some(AUTHORIZE_URL));
        assert_eq!(
            query.get("client_id").map(AsRef::as_ref),
            Some(XAI_OAUTH_CLIENT_ID)
        );
        assert_eq!(
            query.get("scope").map(AsRef::as_ref),
            Some(XAI_OAUTH_SCOPES)
        );
        assert_eq!(
            query.get("code_challenge_method").map(AsRef::as_ref),
            Some("S256")
        );
        assert_eq!(
            query.get("referrer").map(AsRef::as_ref),
            Some("grok-desktop")
        );
        assert!(!format!("{pending:?}").contains(pending.verifier.as_str()));
    }

    #[test]
    fn state_comparison_rejects_mismatch_and_redacts_tokens() {
        let client = XaiOAuthClient::new().expect("client");
        let pending = client.browser_authorization().expect("authorization");
        assert_eq!(pending.complete("wrong"), Err(OAuthError::InvalidCallback));
        let tokens = OAuthTokenSet {
            access: SecretValue::new(b"access-secret".to_vec()).expect("access"),
            refresh: SecretValue::new(b"refresh-secret".to_vec()).expect("refresh"),
            expires_in: Duration::from_hours(1),
            scopes: Some(XAI_OAUTH_SCOPES.into()),
        };
        let debug = format!("{tokens:?}");
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("refresh-secret"));
    }

    #[test]
    fn verification_url_must_be_https() {
        assert!(verified_https_url("https://x.ai/device").is_ok());
        assert_eq!(
            verified_https_url("http://x.ai/device"),
            Err(OAuthError::Protocol)
        );
        assert_eq!(
            verified_https_url("https://user@x.ai/device"),
            Err(OAuthError::Protocol)
        );
        assert_eq!(
            verified_https_url("https://example.com/device"),
            Err(OAuthError::Protocol)
        );
    }
}
