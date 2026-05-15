//! OAuth 2.0 authentication facade for Suprnova.
//!
//! Suprnova owns the OAuth2 protocol (authorization URL generation, code exchange,
//! userinfo fetching). Torii provides persistence: PKCE/state storage, and the
//! `get_or_create_user` + `create_session` primitives.
//!
//! # Architecture
//!
//! Torii 0.5.2's `oauth` feature does **not** generate authorization URLs; it only
//! offers account-linking primitives and a PKCE verifier store. This module fills
//! that gap by:
//!
//! 1. Storing provider config (client ID, secret, redirect URL, scopes) in a
//!    process-global [`OAuthProviderConfig`] registry.
//! 2. Generating the authorization URL from well-known endpoint tables.
//! 3. Using torii's `store_pkce_verifier` / `get_pkce_verifier` for CSRF state.
//! 4. Exchanging the code via `reqwest` and fetching user info.
//! 5. Delegating user persistence and session creation to torii.
//!
//! # Supported providers
//!
//! Currently hardcoded: `"github"`. Adding a new provider requires a row in
//! [`provider_endpoints`].

use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
};

use chrono::Duration;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{
    error::FrameworkError,
    torii_integration::{Session, User, instance},
};

// ── Provider registry ─────────────────────────────────────────────────────────

/// Configuration for a single OAuth 2.0 provider.
#[derive(Clone)]
pub struct OAuthProviderConfig {
    /// The OAuth application's client ID.
    pub client_id: String,
    /// The OAuth application's client secret.
    pub client_secret: String,
    /// The URL the provider redirects to after authorization.
    pub redirect_url: String,
    /// Scopes to request from the provider.
    pub scopes: Vec<String>,
}

/// Process-global registry mapping provider name → config.
static PROVIDER_CONFIGS: OnceLock<RwLock<HashMap<String, OAuthProviderConfig>>> = OnceLock::new();

fn configs() -> &'static RwLock<HashMap<String, OAuthProviderConfig>> {
    PROVIDER_CONFIGS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Well-known endpoint URLs for a provider.
struct ProviderEndpoints {
    /// Authorization endpoint (redirect user here).
    authorize: &'static str,
    /// Token endpoint (exchange code here).
    token: &'static str,
    /// Userinfo endpoint (fetch user profile here).
    userinfo: &'static str,
}

/// Return hardcoded well-known endpoints for supported providers.
///
/// Returns `None` for unknown providers.
fn provider_endpoints(provider: &str) -> Option<ProviderEndpoints> {
    match provider {
        "github" => Some(ProviderEndpoints {
            authorize: "https://github.com/login/oauth/authorize",
            token: "https://github.com/login/oauth/access_token",
            userinfo: "https://api.github.com/user",
        }),
        "google" => Some(ProviderEndpoints {
            authorize: "https://accounts.google.com/o/oauth2/v2/auth",
            token: "https://oauth2.googleapis.com/token",
            userinfo: "https://www.googleapis.com/oauth2/v3/userinfo",
        }),
        _ => None,
    }
}

// ── Public API types ───────────────────────────────────────────────────────────

/// Result of initiating an OAuth flow.
///
/// Redirect the user to [`authorization_url`] and store [`state`] in their
/// session so it can be verified on the callback.
pub struct OAuthKickoff {
    /// The provider's authorization URL with all required query parameters.
    pub authorization_url: String,
    /// A random CSRF state token. Store in the user's session and verify in
    /// the callback handler against the `state` query parameter.
    pub state: String,
}

// ── Facade ────────────────────────────────────────────────────────────────────

/// Facade for OAuth-based authentication operations.
///
/// Obtained via [`crate::Auth::oauth(provider)`].
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::Auth;
/// use suprnova::torii_integration::oauth::OAuthProviderConfig;
///
/// // Configure once at startup (idempotent):
/// Auth::oauth("github").configure(OAuthProviderConfig {
///     client_id: std::env::var("GITHUB_CLIENT_ID").unwrap(),
///     client_secret: std::env::var("GITHUB_CLIENT_SECRET").unwrap(),
///     redirect_url: "https://example.com/auth/oauth/github/callback".into(),
///     scopes: vec!["user:email".into()],
/// });
///
/// // Begin flow:
/// let kickoff = Auth::oauth("github").begin().await?;
/// // Store kickoff.state in session, redirect user to kickoff.authorization_url.
///
/// // Complete on callback:
/// let (user, session) = Auth::oauth("github").complete(&code, &state).await?;
/// ```
pub struct OAuthAuth {
    provider: String,
}

impl OAuthAuth {
    /// Create a new `OAuthAuth` for the given provider.
    pub(crate) fn new(provider: String) -> Self {
        Self { provider }
    }

    /// Register (or overwrite) the provider config for this provider.
    ///
    /// Idempotent: calling again replaces the existing config.
    pub fn configure(&self, config: OAuthProviderConfig) {
        configs()
            .write()
            .expect("OAuthProviderConfig lock poisoned")
            .insert(self.provider.clone(), config);
    }

    /// Begin the OAuth flow.
    ///
    /// Generates a CSRF state token, stores it in torii's PKCE verifier store
    /// (10-minute TTL), and returns the provider authorization URL with all
    /// required query parameters appended.
    ///
    /// # Errors
    ///
    /// - `FrameworkError` if torii is not initialised.
    /// - `FrameworkError` if the provider is unknown or not configured.
    pub async fn begin(&self) -> Result<OAuthKickoff, FrameworkError> {
        let config = self.config()?;
        let endpoints = self.endpoints()?;

        // Generate a cryptographically random CSRF state token.
        let state = uuid::Uuid::new_v4().to_string();

        // Persist the state so we can validate it on callback.
        // We repurpose torii's PKCE verifier store: the "verifier" value is the
        // state itself (we use a simple CSRF token, not full PKCE here).
        let torii = instance()?;
        torii
            .oauth()
            .store_pkce_verifier(&state, &state, Duration::minutes(10))
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth store_pkce_verifier: {e}")))?;

        // Build the authorization URL.
        let scope = config.scopes.join(" ");
        let authorization_url = format!(
            "{}?client_id={}&redirect_uri={}&scope={}&state={}&response_type=code",
            endpoints.authorize,
            urlencoding::encode(&config.client_id),
            urlencoding::encode(&config.redirect_url),
            urlencoding::encode(&scope),
            urlencoding::encode(&state),
        );

        Ok(OAuthKickoff {
            authorization_url,
            state,
        })
    }

    /// Complete the OAuth callback flow.
    ///
    /// Validates the CSRF state, exchanges the authorization code for an access
    /// token, fetches the user's profile from the provider, and returns the
    /// (User, Session) pair.
    ///
    /// # Arguments
    ///
    /// * `code`  - The authorization code from the provider callback.
    /// * `state` - The CSRF state from the provider callback (must match stored).
    ///
    /// # Errors
    ///
    /// - `FrameworkError` if torii is not initialised.
    /// - `FrameworkError` if the state is invalid or expired.
    /// - `FrameworkError` if the token exchange or userinfo fetch fails.
    pub async fn complete(
        &self,
        code: &str,
        state: &str,
    ) -> Result<(User, Session), FrameworkError> {
        let config = self.config()?;
        let endpoints = self.endpoints()?;
        let torii = instance()?;

        // Validate CSRF state (one-time use: get consumes it).
        let stored = torii
            .oauth()
            .get_pkce_verifier(state)
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth get_pkce_verifier: {e}")))?;

        if stored.is_none() {
            return Err(FrameworkError::internal(
                "OAuth state is invalid or expired".to_string(),
            ));
        }

        let client = Client::builder()
            .user_agent("suprnova-oauth/0.1")
            .build()
            .map_err(|e| FrameworkError::internal(format!("http client build: {e}")))?;

        // Exchange the authorization code for an access token.
        let token_resp = client
            .post(endpoints.token)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", config.redirect_url.as_str()),
            ])
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth token exchange: {e}")))?;

        if !token_resp.status().is_success() {
            let status = token_resp.status();
            let body = token_resp.text().await.unwrap_or_default();
            return Err(FrameworkError::internal(format!(
                "oauth token endpoint returned {status}: {body}"
            )));
        }

        let token_data: TokenResponse = token_resp
            .json()
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth token parse: {e}")))?;

        // Fetch the user's profile from the provider.
        let userinfo_resp = client
            .get(endpoints.userinfo)
            .bearer_auth(&token_data.access_token)
            .header("User-Agent", "suprnova-oauth/0.1")
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth userinfo fetch: {e}")))?;

        if !userinfo_resp.status().is_success() {
            let status = userinfo_resp.status();
            let body = userinfo_resp.text().await.unwrap_or_default();
            return Err(FrameworkError::internal(format!(
                "oauth userinfo endpoint returned {status}: {body}"
            )));
        }

        let profile: ProviderProfile = userinfo_resp
            .json()
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth userinfo parse: {e}")))?;

        // Derive the email (required by torii).
        let email = profile
            .email
            .clone()
            .or_else(|| profile.login.clone())
            .unwrap_or_else(|| profile.id_str());

        // Upsert the user in torii's store.
        let user = torii
            .oauth()
            .get_or_create_user(
                &self.provider,
                &profile.id_str(),
                &email,
                profile.name.clone(),
            )
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth get_or_create_user: {e}")))?;

        // Create a session.
        let session = torii
            .create_session(&user.id, None, None)
            .await
            .map_err(|e| FrameworkError::internal(format!("oauth create_session: {e}")))?;

        Ok((user, session))
    }

    // ── Private helpers ────────────────────────────────────────────────────────

    fn config(&self) -> Result<OAuthProviderConfig, FrameworkError> {
        configs()
            .read()
            .expect("OAuthProviderConfig lock poisoned")
            .get(&self.provider)
            .cloned()
            .ok_or_else(|| {
                FrameworkError::internal(format!(
                    "OAuth provider '{}' is not configured. Call Auth::oauth(\"{}\").configure(...) first.",
                    self.provider, self.provider,
                ))
            })
    }

    fn endpoints(&self) -> Result<ProviderEndpoints, FrameworkError> {
        provider_endpoints(&self.provider).ok_or_else(|| {
            FrameworkError::internal(format!(
                "Unknown OAuth provider '{}'. Supported providers: github, google.",
                self.provider,
            ))
        })
    }
}

// ── Deserialisation helpers ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    token_type: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
}

/// Minimal cross-provider user profile. Fields are optional since providers
/// differ in what they include.
#[derive(Deserialize, Serialize)]
struct ProviderProfile {
    /// Numeric user ID (GitHub, etc.)
    #[serde(rename = "id")]
    id_num: Option<serde_json::Value>,
    /// String user ID (Google `sub`, etc.)
    sub: Option<String>,
    email: Option<String>,
    name: Option<String>,
    /// GitHub username.
    login: Option<String>,
}

impl ProviderProfile {
    fn id_str(&self) -> String {
        if let Some(sub) = &self.sub {
            return sub.clone();
        }
        if let Some(id) = &self.id_num {
            return id.to_string();
        }
        // Fallback: shouldn't happen with well-known providers.
        "unknown".to_string()
    }
}

// ── URL encoding helper (inline — avoids a new dep) ──────────────────────────

mod urlencoding {
    /// Percent-encode a string for use in a query parameter value.
    pub fn encode(s: &str) -> String {
        let mut encoded = String::with_capacity(s.len() * 2);
        for byte in s.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    encoded.push(byte as char);
                }
                b' ' => encoded.push('+'),
                b => {
                    encoded.push('%');
                    encoded.push_str(&format!("{b:02X}"));
                }
            }
        }
        encoded
    }
}
