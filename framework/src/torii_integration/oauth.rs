//! OAuth 2.0 authentication facade for Suprnova.
//!
//! Suprnova owns the OAuth2 protocol (authorization URL generation, code
//! exchange, userinfo fetching, and PKCE per RFC 7636). Torii provides
//! persistence: the `get_or_create_user` + `create_session` primitives.
//!
//! # Architecture
//!
//! Torii 0.5.2's `oauth` feature does **not** generate authorization URLs; it
//! only offers account-linking primitives. This module fills that gap by:
//!
//! 1. Storing provider config (client ID, secret, redirect URL, scopes,
//!    optional endpoint overrides) in a process-global
//!    [`OAuthProviderConfig`] registry.
//! 2. Generating the authorization URL from well-known endpoint tables (or
//!    the per-config override, used for self-hosted providers and tests).
//! 3. Generating a CSRF state token and an RFC 7636 PKCE `code_verifier`
//!    during [`OAuthAuth::begin`]; storing both in the caller's session.
//! 4. Sending `code_challenge` + `code_challenge_method=S256` on the
//!    authorization URL, and `code_verifier` on the token-exchange POST.
//! 5. Exchanging the code via `reqwest`, fetching user info, then
//!    delegating user persistence and session creation to torii.
//!
//! # Supported providers
//!
//! Hardcoded well-known endpoints: `github`, `google`. Custom providers (or
//! self-hosted GitHub Enterprise / Google for Workspaces tenants) can supply
//! their own endpoints via `OAuthProviderConfig::endpoints_override`.
//!
//! # Error mapping
//!
//! Protocol failures (state missing/mismatched, PKCE verifier missing,
//! provider returning 4xx) surface as `FrameworkError::Domain { 400, .. }`
//! — they are caller errors, not server errors. Network failures and
//! provider 5xx surface as `FrameworkError::Domain { 502, .. }` — bad
//! upstream. We never use `FrameworkError::internal` here, because that
//! would map to 500 and conflate caller-facing protocol issues with real
//! server faults.

use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    error::FrameworkError,
    lock,
    session::{session, session_mut},
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
    /// Optional endpoint override.
    ///
    /// `None` → use the well-known endpoints for the provider name (e.g.
    /// `"github"` resolves to `https://github.com/login/oauth/...`).
    ///
    /// `Some(_)` → use these endpoints instead. Required for self-hosted
    /// providers (GitHub Enterprise, custom OIDC, etc.) and used by the
    /// framework's integration tests to point the OAuth flow at an
    /// in-process mock server.
    pub endpoints_override: Option<EndpointOverrides>,
}

impl OAuthProviderConfig {
    /// Builder-style convenience to attach endpoint overrides.
    pub fn with_endpoints_override(mut self, endpoints: EndpointOverrides) -> Self {
        self.endpoints_override = Some(endpoints);
        self
    }
}

/// Custom endpoint URLs for an OAuth provider. Optional escape hatch for
/// self-hosted providers and tests.
#[derive(Clone)]
pub struct EndpointOverrides {
    /// Authorization endpoint (redirect user here).
    pub authorize: String,
    /// Token endpoint (exchange code here).
    pub token: String,
    /// Userinfo endpoint (fetch user profile here).
    pub userinfo: String,
}

/// Process-global registry mapping provider name → config.
static PROVIDER_CONFIGS: OnceLock<RwLock<HashMap<String, OAuthProviderConfig>>> = OnceLock::new();

fn configs() -> &'static RwLock<HashMap<String, OAuthProviderConfig>> {
    PROVIDER_CONFIGS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Resolved (owned) endpoint URLs for a given provider+config pair. Owned
/// strings rather than `&'static str` so per-config overrides slot in.
struct ProviderEndpoints {
    /// Authorization endpoint (redirect user here).
    authorize: String,
    /// Token endpoint (exchange code here).
    token: String,
    /// Userinfo endpoint (fetch user profile here).
    userinfo: String,
}

/// Return hardcoded well-known endpoints for supported providers.
///
/// Returns `None` for unknown providers without an override.
fn provider_endpoints(provider: &str) -> Option<ProviderEndpoints> {
    match provider {
        "github" => Some(ProviderEndpoints {
            authorize: "https://github.com/login/oauth/authorize".into(),
            token: "https://github.com/login/oauth/access_token".into(),
            userinfo: "https://api.github.com/user".into(),
        }),
        "google" => Some(ProviderEndpoints {
            authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            userinfo: "https://www.googleapis.com/oauth2/v3/userinfo".into(),
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
///     endpoints_override: None, // use the well-known GitHub endpoints
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
    ///
    /// **Poison policy** (Domain 10 audit D10-B): if the registry lock
    /// is poisoned, the config is NOT applied — a `tracing::error!` is
    /// emitted instead. Next OAuth-flow attempt for this provider will
    /// return "provider not configured" via the read path's normal
    /// error propagation. Production: an app whose lock is poisoned at
    /// boot has bigger problems than a missing OAuth config.
    pub fn configure(&self, config: OAuthProviderConfig) {
        match lock::write(configs()) {
            Ok(mut map) => {
                map.insert(self.provider.clone(), config);
            }
            Err(_) => {
                tracing::error!(
                    provider = %self.provider,
                    "OAuth provider config lock poisoned; skipping configure. \
                     OAuth flows for this provider will report 'not configured'."
                );
            }
        }
    }

    /// Begin the OAuth flow.
    ///
    /// Generates a CSRF state token and an RFC 7636 PKCE `code_verifier`,
    /// then stores both in the **current user's session** under
    /// provider-scoped keys (`oauth_state_<provider>` and
    /// `oauth_pkce_verifier_<provider>`). Returns the provider
    /// authorization URL with `state`, `code_challenge=S256(verifier)`,
    /// and `code_challenge_method=S256` query parameters.
    ///
    /// Storing state and the PKCE verifier per-session (rather than in a
    /// global store) means an attacker cannot complete an OAuth flow
    /// initiated by a different user — each session only accepts the
    /// state and verifier it generated.
    ///
    /// # Errors
    ///
    /// - `FrameworkError::Domain { status_code: 400 }` if the provider is
    ///   unknown or not configured.
    pub async fn begin(&self) -> Result<OAuthKickoff, FrameworkError> {
        let config = self.config()?;
        let endpoints = self.endpoints_for(&config)?;

        // Generate a cryptographically random CSRF state token.
        let state = uuid::Uuid::new_v4().to_string();

        // Generate the PKCE code_verifier + S256 challenge per RFC 7636.
        let verifier = generate_pkce_verifier();
        let challenge = pkce_s256_challenge(&verifier);

        // Store both in THIS session, scoped to the provider. No global
        // store — binding to the session prevents cross-session CSRF and
        // ensures the verifier cannot be replayed by an attacker who
        // intercepts the authorization code but never had the session.
        let state_key = format!("oauth_state_{}", self.provider);
        let verifier_key = format!("oauth_pkce_verifier_{}", self.provider);
        session_mut(|s| {
            s.put(&state_key, state.clone());
            s.put(&verifier_key, verifier);
        });

        // Build the authorization URL. PKCE params are required by
        // RFC 7636 for `code_challenge_method=S256` flows; sending them
        // for providers that don't enforce PKCE is harmless (they ignore
        // unknown params) and provides defense-in-depth for those that do.
        let scope = config.scopes.join(" ");
        let authorization_url = format!(
            "{}?client_id={}&redirect_uri={}&scope={}&state={}&response_type=code\
             &code_challenge={}&code_challenge_method=S256",
            endpoints.authorize,
            urlencoding::encode(&config.client_id),
            urlencoding::encode(&config.redirect_url),
            urlencoding::encode(&scope),
            urlencoding::encode(&state),
            urlencoding::encode(&challenge),
        );

        Ok(OAuthKickoff {
            authorization_url,
            state,
        })
    }

    /// Complete the OAuth callback flow.
    ///
    /// Validates the CSRF state against THIS session's stored value
    /// (one-time use: the session key is deleted after reading). Reads
    /// the PKCE `code_verifier` from the session (also one-time use) and
    /// includes it in the token-exchange POST. Fetches the user's
    /// profile from the provider and returns the (User, Session) pair.
    ///
    /// # Arguments
    ///
    /// * `code`  - The authorization code from the provider callback.
    /// * `state` - The CSRF state from the provider callback (must match session).
    ///
    /// # Errors
    ///
    /// All caller/protocol errors map to `Domain { status_code: 400 }` —
    /// they're caller-facing, not server faults. Upstream provider
    /// failures (network errors, parse errors, provider 5xx) map to
    /// `Domain { status_code: 502 }`. We never use
    /// `FrameworkError::internal` here, because that would 500 a
    /// caller-facing OAuth protocol problem.
    ///
    /// - 400: state missing/mismatched, PKCE verifier missing, provider
    ///   returning a 4xx (e.g. bad client creds, invalid code), provider
    ///   profile lookup returning a 4xx, payload parse failures.
    /// - 502: HTTP client build failure, network transport errors,
    ///   provider returning a 5xx, token-endpoint JSON parse failures
    ///   we can't attribute to the caller.
    /// - 500: only `instance()` failing (torii not initialised) and
    ///   torii's persistence calls (`get_or_create_user`,
    ///   `create_session`) — both real server faults the operator must fix.
    pub async fn complete(
        &self,
        code: &str,
        state: &str,
    ) -> Result<(User, Session), FrameworkError> {
        let config = self.config()?;
        let endpoints = self.endpoints_for(&config)?;
        let torii = instance()?;

        // Read and consume the expected state from THIS session (one-time use).
        let state_key = format!("oauth_state_{}", self.provider);
        let verifier_key = format!("oauth_pkce_verifier_{}", self.provider);
        let expected_state: Option<String> = session().and_then(|s| s.get(&state_key));
        let pkce_verifier: Option<String> = session().and_then(|s| s.get(&verifier_key));
        // Delete from session immediately — one-time use for both.
        session_mut(|s| {
            s.forget(&state_key);
            s.forget(&verifier_key);
        });

        match expected_state {
            None => {
                return Err(FrameworkError::Domain {
                    message: "OAuth state missing from session — flow not initiated or session expired".to_string(),
                    status_code: 400,
                });
            }
            Some(ref expected) if expected != state => {
                return Err(FrameworkError::Domain {
                    message: "OAuth state mismatch — possible CSRF attack or expired flow".to_string(),
                    status_code: 400,
                });
            }
            Some(_) => {} // state matches, proceed
        }

        let verifier = pkce_verifier.ok_or_else(|| FrameworkError::Domain {
            message: "OAuth PKCE verifier missing from session — flow not initiated or session expired".to_string(),
            status_code: 400,
        })?;

        let client = Client::builder()
            .user_agent("suprnova-oauth/0.1")
            .build()
            .map_err(|e| FrameworkError::Domain {
                message: format!("oauth http client build failed: {e}"),
                status_code: 502,
            })?;

        // Exchange the authorization code for an access token.
        let token_resp = client
            .post(&endpoints.token)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", config.redirect_url.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|e| FrameworkError::Domain {
                message: format!("oauth token exchange network error: {e}"),
                status_code: 502,
            })?;

        if !token_resp.status().is_success() {
            let provider_status = token_resp.status();
            let body = token_resp.text().await.unwrap_or_default();
            // Provider 4xx → caller error (400). Provider 5xx → bad upstream (502).
            let outbound_status = if provider_status.is_client_error() {
                400
            } else {
                502
            };
            return Err(FrameworkError::Domain {
                message: format!(
                    "oauth token endpoint returned {provider_status}: {body}"
                ),
                status_code: outbound_status,
            });
        }

        let token_data: TokenResponse = token_resp
            .json()
            .await
            .map_err(|e| FrameworkError::Domain {
                message: format!("oauth token response parse failed: {e}"),
                status_code: 502,
            })?;

        // Fetch the user's profile from the provider.
        let userinfo_resp = client
            .get(&endpoints.userinfo)
            .bearer_auth(&token_data.access_token)
            .header("User-Agent", "suprnova-oauth/0.1")
            .send()
            .await
            .map_err(|e| FrameworkError::Domain {
                message: format!("oauth userinfo fetch network error: {e}"),
                status_code: 502,
            })?;

        if !userinfo_resp.status().is_success() {
            let provider_status = userinfo_resp.status();
            let body = userinfo_resp.text().await.unwrap_or_default();
            let outbound_status = if provider_status.is_client_error() {
                400
            } else {
                502
            };
            return Err(FrameworkError::Domain {
                message: format!(
                    "oauth userinfo endpoint returned {provider_status}: {body}"
                ),
                status_code: outbound_status,
            });
        }

        let profile: ProviderProfile = userinfo_resp
            .json()
            .await
            .map_err(|e| FrameworkError::Domain {
                message: format!("oauth userinfo response parse failed: {e}"),
                status_code: 502,
            })?;

        // Derive the email (required by torii).
        let email = profile
            .email
            .clone()
            .or_else(|| profile.login.clone())
            .unwrap_or_else(|| profile.id_str());

        // Upsert the user in torii's store. Failures here are genuine
        // server faults (DB unreachable, schema drift, etc.) so the 500
        // status code from `FrameworkError::internal` is correct.
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
        lock::read(configs())?
            .get(&self.provider)
            .cloned()
            .ok_or_else(|| FrameworkError::Domain {
                message: format!(
                    "OAuth provider '{}' is not configured. Call Auth::oauth(\"{}\").configure(...) first.",
                    self.provider, self.provider,
                ),
                status_code: 400,
            })
    }

    /// Resolve endpoints for this provider given an already-fetched
    /// config. If the config supplies `endpoints_override`, those win;
    /// otherwise we look up the well-known endpoints table.
    fn endpoints_for(&self, config: &OAuthProviderConfig) -> Result<ProviderEndpoints, FrameworkError> {
        if let Some(override_) = &config.endpoints_override {
            return Ok(ProviderEndpoints {
                authorize: override_.authorize.clone(),
                token: override_.token.clone(),
                userinfo: override_.userinfo.clone(),
            });
        }
        provider_endpoints(&self.provider).ok_or_else(|| FrameworkError::Domain {
            message: format!(
                "Unknown OAuth provider '{}' and no endpoints_override supplied. Supported providers: github, google.",
                self.provider,
            ),
            status_code: 400,
        })
    }
}

// ── PKCE helpers (RFC 7636) ───────────────────────────────────────────────────

/// Generate a PKCE `code_verifier` per RFC 7636 §4.1.
///
/// The spec requires the verifier to be 43–128 characters from the
/// unreserved set `[A-Za-z0-9-._~]`. We use 64 bytes of OS randomness
/// from `getrandom::fill`, base64-url-no-pad encoded, which produces an
/// 86-character string in the strict subset `[A-Za-z0-9_-]`. 86 chars
/// gives 512 bits of entropy — comfortably above the 256-bit floor that
/// would defeat brute-force, while still inside the spec maximum.
fn generate_pkce_verifier() -> String {
    let mut bytes = [0u8; 64];
    getrandom::fill(&mut bytes).expect("OS RNG must be available to mint PKCE verifier");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute the S256 PKCE `code_challenge` from a verifier per RFC 7636 §4.2.
///
/// `code_challenge = BASE64URL-NO-PAD(SHA256(ASCII(code_verifier)))`.
///
/// The challenge is what the client sends on the authorize URL; the
/// verifier is what it sends on the token-exchange POST. The provider
/// validates the relationship server-side, proving the same client
/// drove both halves of the flow.
fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
mod pkce_tests {
    use super::*;

    #[test]
    fn verifier_meets_rfc7636_length_and_charset() {
        let v = generate_pkce_verifier();
        // RFC 7636 §4.1: 43..=128 chars.
        assert!(
            (43..=128).contains(&v.len()),
            "verifier length {} not in 43..=128",
            v.len()
        );
        // Strict subset of the unreserved set.
        assert!(
            v.chars().all(|c| matches!(
                c,
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_'
            )),
            "verifier contains chars outside [A-Za-z0-9_-]: {v}"
        );
    }

    #[test]
    fn verifier_is_high_entropy_and_random() {
        // Two consecutive verifiers must differ — guards against a
        // regression where `generate_pkce_verifier` accidentally returns
        // a constant (e.g. someone seeds the RNG with 0).
        let a = generate_pkce_verifier();
        let b = generate_pkce_verifier();
        assert_ne!(a, b);
    }

    #[test]
    fn s256_challenge_matches_rfc7636_test_vector() {
        // RFC 7636 Appendix B test vector:
        //   verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        //   challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = pkce_s256_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
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
