//! `BruteForce` — facade over `Torii::brute_force()`, plus
//! [`LoginThrottleMiddleware`] for HTTP-layer throttling that
//! short-circuits login requests at the front door when the
//! targeted account is already locked.
//!
//! # Why a facade
//!
//! Same rationale as [`crate::auth_flows::EmailVerification`] and
//! [`crate::auth_flows::PasswordReset`]: consumers depend on
//! `suprnova::*`, never on `torii::*`. The facade hides the `Torii<R>`
//! generic and centralises error mapping (`ToriiError → FrameworkError`).
//!
//! # Event semantics on `unlock_account`
//!
//! Torii's `unlock_account` reports a `bool` — `true` if the account
//! had been locked at the time of the call, `false` if it was already
//! unlocked. We **only** fire the [`AccountUnlocked`] event when
//! `was_locked == true`. This keeps listeners free of spurious
//! "unlock" notifications when an idempotent unlock call lands on an
//! already-clean account (e.g. a successful password reset that runs
//! `unlock_account` defensively).
//!
//! The event dispatch itself is best-effort: a listener panic or a
//! transient dispatcher error does not surface as an `Err` from
//! `unlock_account` — the database mutation has already committed,
//! and a notification path must never roll back a successful
//! security-state transition.

use crate::auth_flows::events::{AccountLocked, AccountUnlocked};
use crate::error::FrameworkError;
use crate::torii_integration::instance;
use torii::LockoutStatus;

/// Facade for brute-force-protection operations.
///
/// All methods delegate to the global Torii instance — call
/// [`crate::torii_integration::init_torii`] before invoking any of them.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth_flows::BruteForce;
///
/// // From a failed authenticate() handler:
/// let status = BruteForce::record_failed_attempt(
///     &email,
///     Some(&peer_ip),
/// )
/// .await?;
/// if status.is_locked {
///     // Surface 423 Locked or 429 Too Many Requests to the user.
/// }
///
/// // From the post-successful-login bookkeeping path:
/// BruteForce::reset_attempts(&email).await?;
/// ```
pub struct BruteForce;

impl BruteForce {
    /// Record a failed authentication attempt for `email`. Optionally
    /// stamp the client IP for audit logs.
    ///
    /// Returns the updated [`LockoutStatus`]. If the attempt crossed
    /// the configured threshold, `status.is_locked` is `true` and
    /// `status.locked_until` is populated.
    ///
    /// Fires [`AccountLocked`] **only** on the unlocked → locked
    /// state transition. Subsequent calls while the account remains
    /// locked do not re-fire the event — listeners can treat each
    /// `AccountLocked` as a fresh security incident worth notifying.
    pub async fn record_failed_attempt(
        email: &str,
        ip: Option<&str>,
    ) -> Result<LockoutStatus, FrameworkError> {
        let torii = instance()?;
        let was_locked = torii
            .brute_force()
            .is_locked(email)
            .await
            .map_err(map_err)?;
        let status = torii
            .brute_force()
            .record_failed_attempt(email, ip)
            .await
            .map_err(map_err)?;

        if !was_locked && status.is_locked {
            let _ = crate::events::EventFacade::dispatch(AccountLocked {
                email: email.to_string(),
                failed_attempts: status.failed_attempts,
            })
            .await;
        }

        Ok(status)
    }

    /// Fetch the current [`LockoutStatus`] for `email` without
    /// recording anything. Safe to call for emails that have no
    /// attempt history — torii reports zero attempts / unlocked.
    pub async fn get_lockout_status(email: &str) -> Result<LockoutStatus, FrameworkError> {
        instance()?
            .brute_force()
            .get_lockout_status(email)
            .await
            .map_err(map_err)
    }

    /// Convenience check — `true` if the account is currently locked.
    /// Equivalent to `get_lockout_status(email).await?.is_locked`.
    pub async fn is_locked(email: &str) -> Result<bool, FrameworkError> {
        instance()?
            .brute_force()
            .is_locked(email)
            .await
            .map_err(map_err)
    }

    /// Clear the failed-attempt counter for `email`. Use after a
    /// successful authentication so a user's earlier typos don't
    /// linger toward a lockout.
    ///
    /// Does **not** dispatch [`AccountUnlocked`] — `reset_attempts`
    /// is the success-path bookkeeping operation, not an admin
    /// unlock. See [`BruteForce::unlock_account`] for the
    /// audit-event-firing variant.
    pub async fn reset_attempts(email: &str) -> Result<(), FrameworkError> {
        instance()?
            .brute_force()
            .reset_attempts(email)
            .await
            .map_err(map_err)
    }

    /// Admin / forced unlock. Clears the attempt counter and the
    /// `locked_at` timestamp, immediately allowing the account to
    /// authenticate again.
    ///
    /// Returns `true` if the account was previously locked (so a
    /// real state transition occurred), `false` otherwise. The
    /// [`AccountUnlocked`] event fires **only** on `true` — see the
    /// module-level docs for rationale.
    pub async fn unlock_account(email: &str) -> Result<bool, FrameworkError> {
        let was_locked = instance()?
            .brute_force()
            .unlock_account(email)
            .await
            .map_err(map_err)?;

        if was_locked {
            // Intentionally discard the dispatch error — the unlock has
            // already committed; a downstream listener failure must not
            // surface as an unlock failure to the caller. The dispatcher
            // itself logs listener errors via tracing.
            let _ = crate::events::EventFacade::dispatch(AccountUnlocked {
                email: email.to_string(),
            })
            .await;
        }

        Ok(was_locked)
    }
}

fn map_err(e: torii::ToriiError) -> FrameworkError {
    FrameworkError::internal(format!("torii brute force: {e}"))
}

// ============================================================================
// HTTP middleware: LoginThrottleMiddleware
// ============================================================================

use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;
use std::sync::Arc;

/// HTTP middleware that short-circuits a login request when the
/// targeted account is currently locked due to too many failed
/// attempts.
///
/// Composes naturally with [`crate::RateLimitMiddleware`] for IP-level
/// throttling — this middleware handles **per-account** lockout, the
/// rate-limit middleware handles per-IP / per-route quotas. Run both
/// for a layered defence against credential stuffing.
///
/// # Email extraction
///
/// The email is pulled from the request by a caller-supplied closure
/// so the middleware doesn't need to know how the login form is
/// shaped. The closure signature is **sync over `&Request`**:
///
/// ```text
/// Fn(&Request) -> Option<String>
/// ```
///
/// Reading the request body is `async` and consumes `Request`, so the
/// closure cannot stream the JSON or form body. Practical extraction
/// surfaces are:
///
/// * a header (`X-Login-Email`), set by a preceding framework
///   middleware or a CSRF / session pre-processor;
/// * a query-string parameter (`?email=…`);
/// * a route parameter (`/login/{email}`).
///
/// Returning `None` is the explicit "I have nothing to check" signal —
/// the middleware passes the request through unchanged. This makes the
/// middleware safe to install on routes that occasionally see anonymous
/// traffic (e.g. the same `POST /login` endpoint that also handles
/// "request password reset" with no email field).
///
/// # Response shape
///
/// On lock the middleware returns:
///
/// * HTTP `429 Too Many Requests`
/// * `Retry-After: <seconds>` — computed from the lockout's
///   `locked_until` timestamp via [`LockoutStatus::retry_after_seconds`].
///   Falls back to 900 (15 minutes — torii's default `lockout_period`)
///   if the timestamp is somehow absent.
/// * Body: `"Account locked due to too many failed login attempts. Try again later."`
///
/// # Backend-error policy
///
/// When `BruteForce::get_lockout_status` errors (database hiccup,
/// torii outage), the middleware's response is governed by an
/// explicit [`BackendErrorPolicy`] that defaults to
/// [`BackendErrorPolicy::FailClosed`]: a 503 with `Retry-After: 1`.
/// The login endpoint is the most sensitive route in the stack — an
/// attacker who can degrade the backing store must not be able to
/// bypass brute-force protection and resume credential-stuffing.
/// Deployments that prefer to keep login available during a
/// brute-force-backend outage can opt into [`BackendErrorPolicy::FailOpen`]
/// via [`LoginThrottleMiddleware::on_backend_error`]; the error is
/// logged at `error` regardless of policy.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth_flows::{BackendErrorPolicy, LoginThrottleMiddleware};
/// use suprnova::Router;
///
/// let throttle = LoginThrottleMiddleware::new(|req| {
///     // Pull the email from a header your login form populates.
///     req.header("X-Login-Email").map(|s| s.to_string())
/// });
///
/// // To prefer availability over the lockout guarantee during a
/// // backend outage:
/// let throttle = throttle.on_backend_error(BackendErrorPolicy::FailOpen);
///
/// let router = Router::new()
///     .post("/login", login_handler)
///     .middleware(throttle);
/// ```
/// Type-erased email-extractor closure stored inside
/// [`LoginThrottleMiddleware`]. Sync over `&Request` — see the
/// middleware's docs for why body access isn't possible.
type EmailExtractor = dyn Fn(&Request) -> Option<String> + Send + Sync + 'static;

/// Policy for how [`LoginThrottleMiddleware`] reacts when the
/// brute-force backend itself errors (database hiccup, torii
/// outage), as opposed to a request legitimately being over its
/// lockout threshold.
///
/// Distinct from the over-quota path (always HTTP 429 with the
/// `Retry-After` from `LockoutStatus::retry_after_seconds`). A
/// backend error means the middleware cannot make a lockout
/// decision at all, so it must choose between availability and the
/// lockout guarantee — and for a credential-stuffing-sensitive
/// route, the right default is to refuse the request.
///
/// This mirrors [`crate::rate_limit::BackendErrorPolicy`] for the
/// generic rate-limit middleware, but defaults the OPPOSITE way:
/// `FailClosed` for login (refuse the request) versus the
/// rate-limit default of `FailOpen` (let traffic through).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendErrorPolicy {
    /// Pass the request through when the backend errors.
    /// Prioritizes availability: a torii outage does not block the
    /// login endpoint. Useful for low-risk deployments where
    /// downtime is the bigger threat than credential stuffing. The
    /// error is logged at `error` so the outage is still visible.
    FailOpen,
    /// Reject the request with HTTP 503 (`Retry-After: 1`) when the
    /// backend errors. Prioritizes the lockout guarantee: during a
    /// brute-force-backend outage, an attacker would otherwise have
    /// an open window to retry credential stuffing without
    /// lockout. The default; the error is logged at `error`.
    #[default]
    FailClosed,
}

/// Middleware that throttles login attempts via the configured brute-force
/// backend. Wraps the request, extracts the candidate email, and either
/// short-circuits with a 429 (or 503 on backend error per
/// [`BackendErrorPolicy`]) or runs the inner handler.
pub struct LoginThrottleMiddleware {
    extract_email: Arc<EmailExtractor>,
    on_backend_error: BackendErrorPolicy,
}

impl LoginThrottleMiddleware {
    /// Build a `LoginThrottleMiddleware` with `extract_email` as the
    /// closure that maps each request to an optional email to check.
    /// Returning `None` passes the request through.
    ///
    /// The backend-error policy defaults to
    /// [`BackendErrorPolicy::FailClosed`]; flip it explicitly via
    /// [`Self::on_backend_error`] when availability outweighs the
    /// lockout guarantee for the deployment's threat model.
    pub fn new<F>(extract_email: F) -> Self
    where
        F: Fn(&Request) -> Option<String> + Send + Sync + 'static,
    {
        Self {
            extract_email: Arc::new(extract_email),
            // Default fail-closed: a sensitive route under brute-force
            // protection must not silently lose the protection when the
            // backing store hiccups. Operators can flip to FailOpen via
            // `on_backend_error` when availability outweighs the
            // lockout guarantee for their threat model.
            on_backend_error: BackendErrorPolicy::FailClosed,
        }
    }

    /// Choose how the middleware reacts to a brute-force-backend
    /// error (e.g. torii's database is unreachable), as distinct from
    /// the over-quota path (always HTTP 429). Defaults to
    /// [`BackendErrorPolicy::FailClosed`].
    pub fn on_backend_error(mut self, policy: BackendErrorPolicy) -> Self {
        self.on_backend_error = policy;
        self
    }
}

#[async_trait]
impl Middleware for LoginThrottleMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // 1. Extract the candidate email. No email → pass through.
        let Some(email) = (self.extract_email)(&request) else {
            return next(request).await;
        };

        // 2. Fetch lockout status in one round-trip so the retry-after
        //    seconds reflect the real `locked_until` and not a constant.
        //    Backend errors route through the configured policy.
        let status = match BruteForce::get_lockout_status(&email).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    email = %email,
                    "brute-force backend error in login throttle middleware"
                );
                return match self.on_backend_error {
                    BackendErrorPolicy::FailOpen => next(request).await,
                    BackendErrorPolicy::FailClosed => Err(HttpResponse::text(
                        "Login throttle backend unavailable. Try again shortly.",
                    )
                    .status(503)
                    .header("retry-after", "1")),
                };
            }
        };

        if !status.is_locked {
            return next(request).await;
        }

        // 3. Compute Retry-After from `locked_until`. Torii's default
        //    `lockout_period` is 15 minutes (900s); fall back to that
        //    if the timestamp is somehow absent (defensive — torii
        //    populates it whenever is_locked is true).
        let retry_after = status
            .retry_after_seconds()
            .filter(|s| *s > 0)
            .unwrap_or(900);

        Err(HttpResponse::text(
            "Account locked due to too many failed login attempts. Try again later.",
        )
        .status(429)
        .header("retry-after", retry_after.to_string()))
    }
}

#[cfg(test)]
mod login_throttle_policy_tests {
    use super::*;

    /// `LoginThrottleMiddleware::new` must default to FailClosed —
    /// regression test for the policy default.
    #[test]
    fn default_policy_is_fail_closed() {
        let mw = LoginThrottleMiddleware::new(|_req: &Request| Some("a@b.c".into()));
        assert_eq!(mw.on_backend_error, BackendErrorPolicy::FailClosed);
        assert_eq!(
            BackendErrorPolicy::default(),
            BackendErrorPolicy::FailClosed
        );
    }

    /// `on_backend_error` is the builder method — assert the override
    /// actually flips the field.
    #[test]
    fn on_backend_error_overrides_default_policy() {
        let mw = LoginThrottleMiddleware::new(|_req: &Request| Some("a@b.c".into()))
            .on_backend_error(BackendErrorPolicy::FailOpen);
        assert_eq!(mw.on_backend_error, BackendErrorPolicy::FailOpen);
    }
}
