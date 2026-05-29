//! Session middleware for suprnova framework

use crate::Request;
use crate::http::Response;
use crate::http::cookie::{Cookie, SameSite};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;
use rand::RngExt;
use std::sync::{Arc, Mutex};

use super::config::SessionConfig;
use super::driver::DatabaseSessionDriver;
use super::store::{SessionData, SessionStore};

// Per-request session slot. `tokio::task_local!` (not `thread_local!`)
// so the binding survives `.await` points that resume on a different
// worker thread — the same fix applied to `InertiaContext` in Tier 0.
//
// The slot is `Arc<Mutex<Option<SessionData>>>` rather than a bare
// `RefCell` because (a) the future inside `SESSION_CONTEXT.scope` may
// move between threads (so we need `Send + Sync`), and (b) the
// middleware needs to read the saved session back out *after* the
// scope returns. Closures passed to `session_mut` do not await, so a
// synchronous `std::sync::Mutex` is sound — guards drop before `.await`.
tokio::task_local! {
    pub(crate) static SESSION_CONTEXT: Arc<Mutex<Option<SessionData>>>;
    /// Per-request slot for cookies that handlers want to attach to the
    /// outgoing response. `Auth::login_remember` and
    /// `Auth::revoke_remember_tokens` push into here; `SessionMiddleware`
    /// drains the slot when assembling the response, applying each cookie
    /// next to the session cookie.
    ///
    /// We can't have handlers mutate the `Response` directly — they
    /// return one synchronously, and the cookie machinery is in the
    /// middleware layer that already owns the response. A task-local
    /// slot is the same shape we use for the session itself.
    pub(crate) static PENDING_COOKIES: Arc<Mutex<Vec<Cookie>>>;
}

/// Push a cookie into the per-request pending-cookies slot.
///
/// Internal helper used by `Auth::login_remember` /
/// `Auth::revoke_remember_tokens`. The session middleware drains the
/// slot after the handler returns and attaches every cookie to the
/// response.
///
/// If called outside a request scope (no `PENDING_COOKIES` task-local
/// installed — e.g. unit tests without the middleware) the cookie is
/// silently dropped. Production code always runs through
/// `SessionMiddleware::handle`, which installs the slot.
#[allow(dead_code)]
pub(crate) fn push_pending_cookie(cookie: Cookie) {
    let _ = PENDING_COOKIES.try_with(|slot| {
        slot.lock().unwrap().push(cookie);
    });
}

/// Get the current session (read-only)
///
/// Returns a clone of the current session data if available.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::session::session;
///
/// if let Some(session) = session() {
///     let name: Option<String> = session.get("name");
/// }
/// ```
pub fn session() -> Option<SessionData> {
    SESSION_CONTEXT
        .try_with(|slot| slot.lock().unwrap().clone())
        .ok()
        .flatten()
}

/// Get the current session and modify it
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::session::session_mut;
///
/// session_mut(|session| {
///     session.put("name", "John");
/// });
/// ```
pub fn session_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut SessionData) -> R,
{
    SESSION_CONTEXT
        .try_with(|slot| slot.lock().unwrap().as_mut().map(f))
        .ok()
        .flatten()
}

/// Set the session context for the current request. Internal helper —
/// not part of the public surface. Tests should use the
/// `new_session_slot_for_test` / `session_scope_for_test` helpers
/// in [`crate::session`] instead.
#[allow(dead_code)]
pub(crate) fn set_session(session: SessionData) {
    let _ = SESSION_CONTEXT.try_with(|slot| {
        *slot.lock().unwrap() = Some(session);
    });
}

/// Clear the session context. Internal — see [`set_session`].
#[allow(dead_code)]
pub(crate) fn clear_session() {
    let _ = SESSION_CONTEXT.try_with(|slot| {
        *slot.lock().unwrap() = None;
    });
}

/// Take the session out of the context. Internal — used by
/// `SessionMiddleware` for the save step.
#[allow(dead_code)]
pub(crate) fn take_session() -> Option<SessionData> {
    SESSION_CONTEXT
        .try_with(|slot| slot.lock().unwrap().take())
        .ok()
        .flatten()
}

/// Generate a cryptographically secure session ID
///
/// Generates a 40-character alphanumeric string.
pub fn generate_session_id() -> String {
    let mut rng = rand::rng();
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

    (0..40)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Generate a CSRF token
///
/// Same format as session ID for consistency.
pub fn generate_csrf_token() -> String {
    generate_session_id()
}

/// Session middleware
///
/// Handles session lifecycle:
/// 1. Reads session ID from cookie
/// 2. Loads session data from storage
/// 3. Makes session available during request
/// 4. Saves session after request
/// 5. Sets session cookie on response
pub struct SessionMiddleware {
    config: SessionConfig,
    store: Arc<dyn SessionStore>,
}

impl SessionMiddleware {
    /// Create a new session middleware with the given configuration
    pub fn new(config: SessionConfig) -> Self {
        let store = Arc::new(DatabaseSessionDriver::new(config.lifetime));
        Self { config, store }
    }

    /// Create session middleware with a custom store
    pub fn with_store(config: SessionConfig, store: Arc<dyn SessionStore>) -> Self {
        Self { config, store }
    }

    /// Construct the middleware AND spawn a background task that
    /// calls [`SessionStore::gc`] once per `interval`. The Tokio
    /// equivalent of Laravel's `StartSession::collectGarbage` lottery
    /// — a real spawned task instead of a 2/100 chance per request.
    ///
    /// Errors from `gc()` are logged at `warn!` and do not kill the
    /// loop. Apps that want explicit scheduling control should keep
    /// using `new` / `with_store` and register their own
    /// [`crate::Schedule`] entry.
    pub fn install_with_gc(config: SessionConfig, interval: std::time::Duration) -> Self {
        let me = Self::new(config);
        let store = me.store.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match store.gc().await {
                    Ok(removed) if removed > 0 => {
                        tracing::debug!(removed, "session gc removed expired rows");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "session gc failed");
                    }
                }
            }
        });
        me
    }

    /// Convenience: spawn a once-per-hour `gc()` background task and
    /// return the middleware. Drop-in replacement for `new(config)` in
    /// production bootstrap code.
    pub fn install(config: SessionConfig) -> Self {
        Self::install_with_gc(config, std::time::Duration::from_secs(3600))
    }

    /// Read access to the bound session store. Lets callers feed the
    /// same store into a `Schedule` entry without rebuilding it.
    pub fn store(&self) -> Arc<dyn SessionStore> {
        self.store.clone()
    }

    /// Build the outbound session cookie. Returns `Err` if `Crypt`
    /// failed to encrypt the session id — which by design only happens
    /// when `Crypt` is not initialized.
    ///
    /// `Server::from_config` guarantees `Crypt` is installed before
    /// any middleware runs (it fails boot otherwise outside dev
    /// environments, and generates a transient dev key otherwise), so
    /// the error path is purely defensive. If it ever does fire, the
    /// middleware fails the request closed rather than emit a
    /// plaintext session id — codex review finding #1.
    fn create_session_cookie(&self, session_id: &str) -> Result<Cookie, crate::FrameworkError> {
        let base = Cookie::encrypted(&self.config.cookie_name, session_id)?;
        let mut cookie = base
            .http_only(self.config.cookie_http_only)
            .secure(self.config.cookie_secure)
            .path(&self.config.cookie_path)
            .partitioned(self.config.cookie_partitioned);

        // `expire_on_close = true` → omit `Max-Age` so the browser
        // forgets the cookie when the window closes. Mirrors
        // Laravel's `session.expire_on_close`.
        if !self.config.expire_on_close {
            cookie = cookie.max_age(self.config.lifetime);
        }

        if let Some(ref domain) = self.config.cookie_domain {
            cookie = cookie.domain(domain);
        }

        cookie = match self.config.cookie_same_site.to_lowercase().as_str() {
            "strict" => cookie.same_site(SameSite::Strict),
            "none" => cookie.same_site(SameSite::None),
            _ => cookie.same_site(SameSite::Lax),
        };

        Ok(cookie)
    }
}

/// Build an outbound remember-me cookie carrying the encrypted plaintext.
///
/// Framework-internal helper. `Auth::login_remember` and the
/// middleware rotation path both call it so cookie attribute defaults
/// live in one place. Mirrors the security profile of the session
/// cookie (HttpOnly, optional Secure, SameSite=Lax).
///
/// `max_age` is set explicitly to match the TTL of the underlying
/// `remember_tokens` row — codex review demanded "expires-at matches
/// token expiration." Callers (login_remember + middleware rotation)
/// pass the same `ttl_minutes` they used to issue the row.
///
/// Exposed as `pub` (rather than `pub(crate)`) because integration
/// tests in `framework/tests/remember_me.rs` need to verify the cookie
/// attributes a real handler would emit. `#[doc(hidden)]` keeps it out
/// of the public rustdoc surface.
#[doc(hidden)]
pub fn create_remember_cookie(
    config: &SessionConfig,
    plaintext: &str,
    max_age: std::time::Duration,
) -> Result<Cookie, crate::FrameworkError> {
    let base = Cookie::encrypted(super::super::auth::remember::COOKIE_NAME, plaintext)?;
    let mut cookie = base
        .http_only(true)
        .secure(config.cookie_secure)
        .path(&config.cookie_path)
        .partitioned(config.cookie_partitioned)
        .max_age(max_age);

    if let Some(ref domain) = config.cookie_domain {
        cookie = cookie.domain(domain);
    }

    cookie = match config.cookie_same_site.to_lowercase().as_str() {
        "strict" => cookie.same_site(SameSite::Strict),
        "none" => cookie.same_site(SameSite::None),
        _ => cookie.same_site(SameSite::Lax),
    };

    Ok(cookie)
}

/// Build a Max-Age=0 cookie that tells the client to drop the
/// `remember_me` cookie. Used by `Auth::revoke_remember_tokens` and by
/// the middleware when a remember cookie fails verification.
///
/// `pub` + `#[doc(hidden)]` for the same reason as
/// `create_remember_cookie`: integration tests need to verify the
/// "clear cookie" shape, but consumers should not depend on it.
#[doc(hidden)]
pub fn create_forget_remember_cookie(config: &SessionConfig) -> Cookie {
    let mut cookie = Cookie::forget(super::super::auth::remember::COOKIE_NAME)
        .path(&config.cookie_path)
        .secure(config.cookie_secure)
        .partitioned(config.cookie_partitioned)
        .same_site(SameSite::Lax);
    if let Some(ref domain) = config.cookie_domain {
        cookie = cookie.domain(domain);
    }
    cookie
}

#[async_trait]
impl Middleware for SessionMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Defensive: refuse to run at all when `Crypt` isn't installed.
        // `Server::from_config` guarantees a key is in place before
        // middleware boots (failing closed in production, generating a
        // transient key in dev). If we somehow got here without one
        // — e.g. an embedder built a service loop without going through
        // `Server::from_config` — bail out closed rather than emit or
        // accept plaintext session ids. Codex review finding #1.
        if !crate::crypto::Crypt::is_initialized() {
            return Err(crate::http::HttpResponse::text(
                "Internal Server Error: encryption key not installed",
            )
            .status(500));
        }

        // Read the session ID from the inbound cookie. The cookie
        // value is AES-256-GCM ciphertext; decrypt failure (tamper,
        // key rotation) silently mints a fresh session id rather than
        // logging per-request — same fail-quietly semantics as Laravel
        // when the SESSION cookie is unreadable.
        let session_id = match request.cookie(&self.config.cookie_name) {
            Some(raw) => Cookie::read_encrypted(&raw)
                .ok()
                .unwrap_or_else(generate_session_id),
            None => generate_session_id(),
        };

        // Load session from store
        let mut session = match self.store.read(&session_id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                // Create new session
                SessionData::new(session_id.clone(), generate_csrf_token())
            }
            Err(e) => {
                // Store read failed (outage, corruption). Degrade
                // gracefully by minting a fresh session — same posture as
                // Laravel when the session row is unreadable. `warn!`, not
                // `error!`: this fires once per request, so during an
                // outage an error-level line would spam at request rate.
                tracing::warn!(error = %e, "session read failed; minting a fresh session");
                SessionData::new(session_id.clone(), generate_csrf_token())
            }
        };

        // Age flash data from previous request
        session.age_flash_data();

        // Per-request bag of cookies handlers want attached. Populated
        // by `push_pending_cookie` (called from `Auth::login_remember`
        // and `Auth::revoke_remember_tokens`) and drained below right
        // next to where the session cookie is attached.
        let pending: Arc<Mutex<Vec<Cookie>>> = Arc::new(Mutex::new(Vec::new()));

        // Remember-me hydration: when the inbound request has no
        // active session (no user_id loaded) but does carry a valid
        // `remember_me` cookie, verify the token, rotate it, hydrate
        // the session, and queue the fresh cookie. This is the
        // "browser was closed, session expired, but the user ticked
        // remember-me a month ago" path. Bad/expired/forged cookies
        // are cleared so the client stops shipping garbage.
        if session.user_id.is_none()
            && let Some(raw_cookie) = request.cookie(crate::auth::remember::COOKIE_NAME)
        {
            match Cookie::read_encrypted(&raw_cookie) {
                Ok(plaintext) => {
                    let ttl_minutes = (self.config.remember_lifetime.as_secs() / 60) as i64;
                    match crate::auth::remember::verify_and_rotate(&plaintext, ttl_minutes).await {
                        Ok(Some((user_id, new_plaintext))) => {
                            // Hydrate session. Mirrors `Auth::login`:
                            // regenerate session id + CSRF token to
                            // prevent session fixation off a stale id
                            // and to invalidate any pre-login form
                            // tokens.
                            session.id = generate_session_id();
                            session.user_id = Some(user_id);
                            session.csrf_token = generate_csrf_token();
                            session.dirty = true;

                            // Mark this request as authenticated via the
                            // remember-me cookie so `StatefulGuard::via_remember`
                            // reports it. No-op when no auth request-state
                            // scope is installed.
                            crate::auth::request_state::set_via_remember(true);

                            // Queue the rotated cookie. Its Max-Age
                            // mirrors the new row's TTL so the
                            // browser stops sending it the moment
                            // the server-side row expires. If we
                            // can't encrypt (Crypt deinitialized
                            // between boot and now — impossible in
                            // practice but defensive), drop quietly
                            // rather than fail the request: the user
                            // is already authenticated this turn,
                            // they just won't get a refreshed cookie.
                            if let Ok(c) = create_remember_cookie(
                                &self.config,
                                &new_plaintext,
                                self.config.remember_lifetime,
                            ) {
                                pending.lock().unwrap().push(c);
                            }
                        }
                        Ok(None) => {
                            // Cookie decrypted to a token nothing
                            // matched — tell the client to drop it.
                            pending
                                .lock()
                                .unwrap()
                                .push(create_forget_remember_cookie(&self.config));
                        }
                        Err(e) => {
                            // DB error — log and continue without
                            // remember-me. Don't clear the cookie:
                            // this might be a transient outage, not
                            // a forged token. `warn!` (not `error!`) for
                            // the same per-request-spam reason as the
                            // session-read path above.
                            tracing::warn!(
                                error = %e,
                                "remember-me verification failed; continuing without it"
                            );
                        }
                    }
                }
                Err(_) => {
                    // Cookie present but can't be decrypted (tamper,
                    // old key). Clear it so the client stops sending
                    // garbage.
                    pending
                        .lock()
                        .unwrap()
                        .push(create_forget_remember_cookie(&self.config));
                }
            }
        }

        // Bind both the session and the pending-cookies slot to
        // `tokio::task_local!` so they survive `.await` points that
        // resume on a different worker thread. Handlers read/write
        // through `session()` / `session_mut()` / `push_pending_cookie`.
        let slot: Arc<Mutex<Option<SessionData>>> = Arc::new(Mutex::new(Some(session)));
        let response = SESSION_CONTEXT
            .scope(
                slot.clone(),
                PENDING_COOKIES.scope(pending.clone(), next(request)),
            )
            .await;

        // Take the potentially-modified session back out of the slot.
        let session = slot.lock().unwrap().take();

        // Drain pending cookies — both the ones queued from the
        // middleware (remember-me rotation / clear) and any queued by
        // handlers via `Auth::login_remember` etc.
        let mut response = response;
        let pending_cookies = std::mem::take(&mut *pending.lock().unwrap());

        // Save session and add cookie to response
        if let Some(session) = session {
            // Always save — even an unmodified session gets its
            // last_activity bumped (sliding expiration).
            if let Err(e) = self.store.write(&session).await {
                if session.is_dirty() {
                    // The session was mutated this request (login, logout,
                    // CSRF rotation, flash, remember-me hydration, ...) and
                    // we could not persist it. Returning the handler's
                    // success response now would lie: the client would get
                    // a session cookie for state the store never recorded,
                    // so the next request loads an empty session and the
                    // mutation silently vanishes — e.g. a "successful"
                    // login that didn't stick. Fail closed. We return
                    // BEFORE create_session_cookie below, so no cookie is
                    // attached: a cookie for an id the store never saw is
                    // worse than none.
                    tracing::error!(
                        error = %e,
                        session_id = %session.id,
                        "session write failed for a mutated session; failing closed with 500"
                    );
                    return Err(crate::http::HttpResponse::text(
                        "Internal Server Error: session persistence failed",
                    )
                    .status(500));
                }
                // Not dirty: the write was only a last_activity touch, so
                // the user-visible state is intact. Log and let the request
                // through rather than 500 every read-only request during a
                // transient store outage. `warn!` for the same
                // per-request-spam reason as the read path.
                tracing::warn!(
                    error = %e,
                    session_id = %session.id,
                    "session last-activity write failed (session unmodified); continuing"
                );
            }

            // Add session cookie to response. Encryption must succeed
            // here — we already verified Crypt is initialized at the
            // top of `handle`. If it doesn't, fail the request closed.
            let cookie = match self.create_session_cookie(&session.id) {
                Ok(c) => c,
                Err(_) => {
                    return Err(crate::http::HttpResponse::text(
                        "Internal Server Error: session cookie encryption failed",
                    )
                    .status(500));
                }
            };

            response = match response {
                Ok(res) => Ok(res.cookie(cookie)),
                Err(res) => Err(res.cookie(cookie)),
            };
        }

        // Attach every pending cookie. Done after the session cookie
        // so the relative ordering in the `Set-Cookie` header list is
        // stable (session first, then remember-me / clears).
        for cookie in pending_cookies {
            response = match response {
                Ok(res) => Ok(res.cookie(cookie)),
                Err(res) => Err(res.cookie(cookie)),
            };
        }

        response
    }
}

/// Regenerate the session ID (for security after login)
///
/// This creates a new session ID while preserving session data,
/// which helps prevent session fixation attacks.
pub fn regenerate_session_id() {
    session_mut(|session| {
        session.id = generate_session_id();
        session.dirty = true;
    });
}

/// Invalidate the current session (clear all data)
pub fn invalidate_session() {
    session_mut(|session| {
        session.flush();
        session.csrf_token = generate_csrf_token();
    });
}

/// Helper to get the CSRF token from current session
pub fn get_csrf_token() -> Option<String> {
    session().map(|s| s.csrf_token)
}

/// Mint a new CSRF token for the current session without otherwise
/// touching session data. Mirrors Laravel's `Store::regenerateToken`
/// (`Illuminate/Session/Store.php:755-758`). Returns the new token
/// (or `None` when no session scope is installed).
pub fn regenerate_csrf_token() -> Option<String> {
    session_mut(|session| {
        let token = generate_csrf_token();
        session.csrf_token = token.clone();
        session.dirty = true;
        token
    })
}

/// Helper to check if user is authenticated
pub fn is_authenticated() -> bool {
    auth_user_id().is_some()
}

/// Helper to get the authenticated user ID
///
/// Consults the request-scoped auth state first (so a `once` /
/// `set_user` authentication that was never written to the session is
/// still visible to `Auth::id()`), then falls back to the persisted
/// session user.
pub fn auth_user_id() -> Option<String> {
    crate::auth::request_state::current_user_id().or_else(|| session().and_then(|s| s.user_id))
}

/// Helper to set the authenticated user
pub fn set_auth_user(user_id: impl Into<String>) {
    let user_id = user_id.into();
    session_mut(|session| {
        session.user_id = Some(user_id);
        session.dirty = true;
    });
}

/// Helper to clear the authenticated user (logout)
pub fn clear_auth_user() {
    session_mut(|session| {
        session.user_id = None;
        session.dirty = true;
    });
}

/// Reserved key under `SessionData::data` for the "password verified
/// but 2FA challenge not yet completed" user-id.
///
/// Kept in the generic data bag rather than as a typed field on
/// `SessionData` so adding 2FA-challenge support doesn't require
/// every session driver to learn about a new column — the bag is
/// already serialized end-to-end.
const TWO_FACTOR_PENDING_KEY: &str = "_two_factor_pending_user_id";

/// Read the user-id of a user who has authenticated their password
/// but has not yet completed the 2FA TOTP challenge. Returns `None`
/// outside the request scope or when no challenge is pending.
///
/// Backs [`crate::auth_flows::TwoFactor::pending_user_id`].
pub fn two_factor_pending_user_id() -> Option<String> {
    session().and_then(|s| {
        s.data
            .get(TWO_FACTOR_PENDING_KEY)
            .and_then(|v| v.as_str().map(String::from))
    })
}

/// Stash a "2FA challenge pending" user-id in the session. The caller
/// (typically [`crate::auth_flows::TwoFactor::start_challenge`]) is
/// responsible for clearing the fully-authenticated slot first —
/// pending and authed are mutually exclusive states.
pub fn set_two_factor_pending(user_id: impl Into<String>) {
    let user_id = user_id.into();
    session_mut(|session| {
        session.data.insert(
            TWO_FACTOR_PENDING_KEY.to_string(),
            serde_json::Value::String(user_id),
        );
        session.dirty = true;
    });
}

/// Clear the "2FA challenge pending" user-id. Called by a successful
/// challenge completion (which promotes pending → authed), an
/// explicit "cancel challenge" UI action, or a fresh logout.
pub fn clear_two_factor_pending() {
    session_mut(|session| {
        if session.data.remove(TWO_FACTOR_PENDING_KEY).is_some() {
            session.dirty = true;
        }
    });
}

/// Reserved key under `SessionData::data` for the "user asked to be
/// remembered" preference that was supplied to
/// [`crate::auth_flows::TwoFactor::start_challenge`] and needs to
/// survive until [`crate::auth_flows::TwoFactor::complete_challenge`]
/// can re-issue the remember-me cookie.
///
/// Lives in the generic data bag rather than as a typed field on
/// `SessionData` for the same reason as [`TWO_FACTOR_PENDING_KEY`] —
/// avoiding driver churn for a feature whose state is naturally
/// transient.
const TWO_FACTOR_PENDING_REMEMBER_KEY: &str = "_two_factor_pending_remember";

/// Read the "user asked to be remembered" preference stashed by
/// [`set_two_factor_pending_remember`]. Returns `false` outside a
/// request scope or when no preference was set.
///
/// Backs [`crate::auth_flows::TwoFactor::complete_challenge`]'s
/// remember-me re-issue path.
pub fn two_factor_pending_remember() -> bool {
    session()
        .and_then(|s| {
            s.data
                .get(TWO_FACTOR_PENDING_REMEMBER_KEY)
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false)
}

/// Stash the "user asked to be remembered" preference alongside the
/// pending user-id. The caller — typically
/// [`crate::auth_flows::TwoFactor::start_challenge`] — passes through
/// the `remember` argument it received from the login form.
///
/// Stored as a JSON boolean; clears the slot when `remember` is
/// `false` to keep the bag minimal.
pub fn set_two_factor_pending_remember(remember: bool) {
    if remember {
        session_mut(|session| {
            session.data.insert(
                TWO_FACTOR_PENDING_REMEMBER_KEY.to_string(),
                serde_json::Value::Bool(true),
            );
            session.dirty = true;
        });
    } else {
        clear_two_factor_pending_remember();
    }
}

/// Clear the "remember-me on completion" preference. Called by a
/// successful challenge completion (after consuming the value), by
/// [`clear_two_factor_pending`] callers that want a clean teardown, or
/// when the preference is explicitly being reset to `false`.
pub fn clear_two_factor_pending_remember() {
    session_mut(|session| {
        if session
            .data
            .remove(TWO_FACTOR_PENDING_REMEMBER_KEY)
            .is_some()
        {
            session.dirty = true;
        }
    });
}
