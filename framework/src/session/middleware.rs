//! Session middleware for suprnova framework

use crate::http::cookie::{Cookie, SameSite};
use crate::http::Response;
use crate::middleware::{Middleware, Next};
use crate::Request;
use async_trait::async_trait;
use rand::Rng;
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
    fn create_session_cookie(
        &self,
        session_id: &str,
    ) -> Result<Cookie, crate::FrameworkError> {
        let base = Cookie::encrypted(&self.config.cookie_name, session_id)?;
        let mut cookie = base
            .http_only(self.config.cookie_http_only)
            .secure(self.config.cookie_secure)
            .path(&self.config.cookie_path)
            .max_age(self.config.lifetime);

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
        .max_age(max_age);

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
    Cookie::forget(super::super::auth::remember::COOKIE_NAME)
        .path(&config.cookie_path)
        .secure(config.cookie_secure)
        .same_site(SameSite::Lax)
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
                eprintln!("Session read error: {}", e);
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
                    let ttl_minutes =
                        (self.config.remember_lifetime.as_secs() / 60) as i64;
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
                            // a forged token.
                            eprintln!("Remember-me verify error: {}", e);
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
            // Always save to update last_activity
            if let Err(e) = self.store.write(&session).await {
                eprintln!("Session write error: {}", e);
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

/// Helper to check if user is authenticated
pub fn is_authenticated() -> bool {
    session().map(|s| s.user_id.is_some()).unwrap_or(false)
}

/// Helper to get the authenticated user ID
pub fn auth_user_id() -> Option<String> {
    session().and_then(|s| s.user_id)
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
