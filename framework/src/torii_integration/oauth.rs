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
    time::Duration,
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
    /// Optional provider-specific endpoint that returns the user's
    /// verified email addresses. GitHub's `/user/emails` returns
    /// `[{ email, primary, verified, visibility }, ...]`; when the
    /// `/user` userinfo response omits `email` (common for accounts
    /// that keep their primary email private), the OAuth flow falls
    /// back to this endpoint and picks the primary verified address.
    ///
    /// Only consulted when the userinfo payload's `email` field is
    /// missing or empty; never used to override a value the userinfo
    /// payload supplied.
    pub emails: Option<String>,
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
    /// Optional provider-specific emails endpoint (GitHub's
    /// `/user/emails`). See [`EndpointOverrides::emails`].
    emails: Option<String>,
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
            // GitHub's `/user` endpoint omits `email` when the user
            // has set their primary address to private. The verified
            // primary email is still available via `/user/emails`
            // when the `user:email` scope is granted; we fetch it
            // there before refusing the flow.
            emails: Some("https://api.github.com/user/emails".into()),
        }),
        "google" => Some(ProviderEndpoints {
            authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            userinfo: "https://www.googleapis.com/oauth2/v3/userinfo".into(),
            // Google's OIDC userinfo response carries `email` and
            // `email_verified` directly — no separate emails endpoint.
            emails: None,
        }),
        _ => None,
    }
}

// ── Public API types ───────────────────────────────────────────────────────────

/// Result of initiating an OAuth flow.
///
/// Redirect the user to [`authorization_url`] and store [`state`] in their
/// session so it can be verified on the callback.
#[derive(Debug)]
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
        match lock::write(configs(), "oauth provider configs") {
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
    /// - `FrameworkError::Internal` if `begin()` runs outside a request
    ///   handled by `SessionMiddleware`. The session-binding check in
    ///   [`Self::complete`] cannot pass without a session pointer, so a
    ///   sessionless `begin` would otherwise mint an unusable ceremony
    ///   and a state value the caller can never spend — a server
    ///   misconfiguration (route missing the middleware) rather than a
    ///   caller error.
    pub async fn begin(&self) -> Result<OAuthKickoff, FrameworkError> {
        let config = self.config()?;
        let endpoints = self.endpoints_for(&config)?;

        // Establish the session pointer BEFORE issuing the ceremony row
        // so a sessionless caller cannot orphan a row in
        // `auth_ceremony_tokens`. `session_mut` returns `None` when the
        // calling task is not inside `SessionMiddleware`, which is a
        // server wiring fault — surface it as an internal error with
        // an actionable message instead of silently succeeding with a
        // state the caller can never complete().
        let session_state_key = format!("oauth_state_{}", self.provider);
        if session_mut(|_| ()).is_none() {
            return Err(FrameworkError::internal(format!(
                "OAuth::begin('{}') invoked outside a session — \
                 mount SessionMiddleware on the route group that handles \
                 OAuth start endpoints so the session-binding check in \
                 OAuth::complete can pass.",
                self.provider,
            )));
        }
        // (Identical wording shape to the passkey facade's
        // `require_session_present` helper — both errors describe the
        // same wiring fault and surface as 500 internal so the
        // operator's log carries an actionable message.)

        // Generate a cryptographically random CSRF state token. The
        // state IS the ceremony selector — it's echoed in the
        // authorize URL and the provider sends it back on the callback,
        // giving us O(1) lookup of the matching ceremony payload.
        let state = uuid::Uuid::new_v4().to_string();

        // Generate the PKCE code_verifier + S256 challenge per RFC 7636.
        let verifier = generate_pkce_verifier();
        let challenge = pkce_s256_challenge(&verifier);

        // Store the (state, verifier, provider) ceremony in the
        // single-use `auth_ceremony_tokens` table. ChatGPT audit
        // `torii_integration` HIGH #3: the previous design stored
        // these in the session and relied on a non-atomic
        // get-and-forget — two concurrent callbacks with the same
        // session cookie could both consume the same ceremony.
        // The ceremony-tokens table provides atomic single-use via a
        // conditional DELETE keyed on the UNIQUE selector.
        //
        // TTL: 10 minutes — generous for slow networks while keeping
        // unused ceremonies pruned.
        super::ceremony::issue(
            &state,
            super::ceremony::kind::OAUTH,
            &OAuthCeremonyPayload {
                provider: self.provider.clone(),
                pkce_verifier: verifier,
            },
            10,
        )
        .await?;

        // Session binding: write the state under a provider-scoped
        // session key. `complete` requires THIS session to hold the
        // exact state before consuming the ceremony — preserves the
        // codex finding #7 property that an attacker who steals a
        // state value but not the session cookie cannot complete the
        // flow. The atomic ceremony consume gives single-use on top
        // of the session check.
        session_mut(|s| {
            s.put(&session_state_key, state.clone());
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

        // Session binding (codex finding #7): the session that called
        // `begin` is the only session that can complete the flow. An
        // attacker who steals a state value but not the session cookie
        // sees an empty session here and is rejected.
        let session_state_key = format!("oauth_state_{}", self.provider);
        let expected_state: Option<String> = session().and_then(|s| s.get(&session_state_key));

        match expected_state.as_deref() {
            None => {
                return Err(FrameworkError::Domain {
                    message:
                        "OAuth state missing from session — flow not initiated or session expired"
                            .to_string(),
                    status_code: 400,
                });
            }
            Some(expected) if expected != state => {
                return Err(FrameworkError::Domain {
                    message: "OAuth state mismatch — possible CSRF attack or expired flow"
                        .to_string(),
                    status_code: 400,
                });
            }
            _ => {} // state matches the session's stored value
        }

        // Atomically consume the ceremony keyed by the state echoed
        // back from the provider. Single-use under concurrency: two
        // concurrent callbacks with the same `state` both pass the
        // session check above, but only one wins the atomic DELETE
        // (rows_affected == 1) and gets the payload; the other gets
        // `None` and rejects. ChatGPT audit `torii_integration` HIGH
        // #3 — replay race impossible by construction.
        let payload: OAuthCeremonyPayload =
            super::ceremony::consume(state, super::ceremony::kind::OAUTH)
                .await?
                .ok_or_else(|| FrameworkError::Domain {
                    message:
                        "OAuth state already consumed or expired — replay attempt or stale flow"
                            .to_string(),
                    status_code: 400,
                })?;

        // Best-effort clear the session pointer. The atomic consume
        // above is the single-use authority; this is janitorial.
        session_mut(|s| {
            s.forget(&session_state_key);
        });

        // Verify the provider in the ceremony matches the facade
        // instance handling this callback. Prevents a state token
        // generated for provider A from being consumed by provider B
        // (defence-in-depth, since the OAuth controller routes
        // per-provider anyway).
        if payload.provider != self.provider {
            return Err(FrameworkError::Domain {
                message: format!(
                    "OAuth state was issued for provider '{}' but consumed against '{}'",
                    payload.provider, self.provider
                ),
                status_code: 400,
            });
        }

        let verifier = payload.pkce_verifier;

        // Both timeouts cap how long a slow or blackholed provider can
        // tie up the calling task. `connect_timeout` covers DNS +
        // TCP/TLS handshake; `timeout` is the total per-request budget
        // (token exchange and userinfo fetch each have their own
        // `.send()`, so each gets the full budget independently). 30s
        // matches the framework's outbound HTTP default in
        // [`crate::http_client`]; OAuth providers are expected to
        // respond in well under a second, so this is generous but
        // still bounded.
        let client = Client::builder()
            .user_agent("suprnova-oauth/0.1")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
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
                message: format!("oauth token endpoint returned {provider_status}: {body}"),
                status_code: outbound_status,
            });
        }

        let token_data: TokenResponse =
            token_resp
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
                message: format!("oauth userinfo endpoint returned {provider_status}: {body}"),
                status_code: outbound_status,
            });
        }

        let profile: ProviderProfile =
            userinfo_resp
                .json()
                .await
                .map_err(|e| FrameworkError::Domain {
                    message: format!("oauth userinfo response parse failed: {e}"),
                    status_code: 502,
                })?;

        // Resolve a stable provider-side identifier. If the provider
        // sent neither `sub` nor `id`, we cannot safely attribute the
        // user — refuse the callback rather than collapse to a constant
        // that would conflate distinct users (ChatGPT audit
        // `torii_integration` HIGH #4). 502 because the upstream
        // produced an unusable payload.
        let provider_id = profile.id_str().ok_or_else(|| FrameworkError::Domain {
            message: format!(
                "oauth provider '{}' returned a userinfo payload with neither `sub` nor `id` — cannot attribute account",
                self.provider
            ),
            status_code: 502,
        })?;

        // Derive the user's email. Identity attribution keys on
        // `provider_id` (see HIGH #4), so this is purely the email
        // address recorded on the torii user row — but it must still
        // be a real, verified email, never a username or opaque ID:
        //
        // 1. If the userinfo response carries `email` and (for OIDC
        //    providers like Google) `email_verified == true`, use it
        //    directly.
        // 2. Otherwise, if the provider exposes a verified-emails
        //    endpoint (GitHub's `/user/emails`), fetch it and pick
        //    the primary verified address.
        // 3. If neither path yields a verified email, refuse the
        //    callback. We will not write a username, opaque
        //    provider id, or unverified address into the email
        //    column.
        //
        // Status: 502 because the provider returned a payload we
        // cannot turn into a valid account identifier; the caller
        // (browser) did nothing wrong.
        let userinfo_email_is_verified = match self.provider.as_str() {
            // Google sets `email_verified: true` on every primary
            // Google account email; OIDC convention. If they ever
            // emit `false`, ignore the address.
            "google" => profile.email_verified.unwrap_or(false),
            // GitHub's `/user` endpoint does not include an
            // `email_verified` flag — if `email` is present, GitHub
            // has already validated it (only verified emails are
            // ever returned from `/user`). Treat presence as
            // verified for GitHub specifically.
            "github" => true,
            // For provider names the framework does not recognise,
            // trust the userinfo `email` field if either:
            //   1. the payload explicitly carries `email_verified: true`
            //      (OIDC convention), or
            //   2. no `email_verified` field is present at all — most
            //      non-OIDC providers (and the integration-test mock
            //      server) do not emit the flag, and rejecting their
            //      already-validated `email` field would force every
            //      custom integration to also wire up an `emails`
            //      endpoint. An explicit `false` value still rejects.
            _ => profile.email_verified.unwrap_or(true),
        };

        let email = match profile
            .email
            .as_deref()
            .filter(|e| !e.is_empty() && userinfo_email_is_verified)
        {
            Some(addr) => addr.to_string(),
            None => fetch_verified_primary_email(&client, &endpoints, &token_data.access_token)
                .await?
                .ok_or_else(|| FrameworkError::Domain {
                    message: format!(
                        "oauth provider '{}' did not supply a verified email — \
                         the OAuth scope must grant verified-email access \
                         (e.g. `user:email` for GitHub, `openid email` for \
                         OIDC providers) and the account must have a \
                         verified primary address",
                        self.provider
                    ),
                    status_code: 502,
                })?,
        };

        // Upsert the user in torii's store. Failures here are genuine
        // server faults (DB unreachable, schema drift, etc.) so the 500
        // status code from `FrameworkError::internal` is correct.
        let user = torii
            .oauth()
            .get_or_create_user(&self.provider, &provider_id, &email, profile.name.clone())
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
        lock::read(configs(), "oauth provider configs")?
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
    fn endpoints_for(
        &self,
        config: &OAuthProviderConfig,
    ) -> Result<ProviderEndpoints, FrameworkError> {
        if let Some(override_) = &config.endpoints_override {
            return Ok(ProviderEndpoints {
                authorize: override_.authorize.clone(),
                token: override_.token.clone(),
                userinfo: override_.userinfo.clone(),
                emails: override_.emails.clone(),
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

// ── Ceremony payload ──────────────────────────────────────────────────────────

/// In-flight OAuth ceremony stored in `auth_ceremony_tokens` between
/// `begin` and `complete`. Atomic single-use via the table's
/// UNIQUE selector + conditional DELETE.
#[derive(Serialize, Deserialize)]
struct OAuthCeremonyPayload {
    provider: String,
    pkce_verifier: String,
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
    /// OIDC `email_verified` claim. Providers that follow OpenID
    /// Connect (Google, Microsoft, Okta, etc.) emit this flag on the
    /// userinfo endpoint; we require `true` before accepting
    /// `email` from the userinfo response.
    email_verified: Option<bool>,
    name: Option<String>,
    /// GitHub username.
    #[allow(dead_code)] // retained on the wire — informational only
    login: Option<String>,
}

/// GitHub `/user/emails` row. Used to recover the primary verified
/// email when `/user` omits it (private-email accounts).
#[derive(Deserialize)]
struct GithubEmailEntry {
    email: String,
    primary: bool,
    verified: bool,
}

/// Try the provider's emails endpoint to obtain a verified primary
/// email address.
///
/// Returns:
/// - `Ok(Some(addr))` if the endpoint returns a row marked
///   `primary == true && verified == true`.
/// - `Ok(None)` if the provider does not expose an emails endpoint or
///   the endpoint returns no usable address. The caller is expected
///   to surface a 502 to the user — we refuse to fall back to a
///   non-verified address or to a username.
/// - `Err(_)` if the network call itself fails (timeout, 5xx, parse
///   error). These are bad-upstream conditions and map to 502.
async fn fetch_verified_primary_email(
    client: &Client,
    endpoints: &ProviderEndpoints,
    access_token: &str,
) -> Result<Option<String>, FrameworkError> {
    let Some(url) = endpoints.emails.as_deref() else {
        return Ok(None);
    };
    let resp = client
        .get(url)
        .bearer_auth(access_token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "suprnova-oauth/0.1")
        .send()
        .await
        .map_err(|e| FrameworkError::Domain {
            message: format!("oauth emails endpoint network error: {e}"),
            status_code: 502,
        })?;

    if !resp.status().is_success() {
        let provider_status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let outbound_status = if provider_status.is_client_error() {
            400
        } else {
            502
        };
        return Err(FrameworkError::Domain {
            message: format!("oauth emails endpoint returned {provider_status}: {body}"),
            status_code: outbound_status,
        });
    }

    let entries: Vec<GithubEmailEntry> = resp.json().await.map_err(|e| FrameworkError::Domain {
        message: format!("oauth emails response parse failed: {e}"),
        status_code: 502,
    })?;

    Ok(entries
        .into_iter()
        .find(|e| e.primary && e.verified && !e.email.is_empty())
        .map(|e| e.email))
}

impl ProviderProfile {
    /// Returns the provider's stable user identifier. `None` if the
    /// provider response carries neither `sub` (OpenID Connect) nor
    /// `id` (GitHub-style). Callers MUST reject such responses as
    /// they cannot be safely attributed — collapsing missing IDs to
    /// a constant like `"unknown"` would conflate multiple distinct
    /// users under one identity (ChatGPT audit `torii_integration`
    /// HIGH #4).
    fn id_str(&self) -> Option<String> {
        if let Some(sub) = &self.sub {
            return Some(sub.clone());
        }
        if let Some(id) = &self.id_num {
            return Some(id.to_string());
        }
        None
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
