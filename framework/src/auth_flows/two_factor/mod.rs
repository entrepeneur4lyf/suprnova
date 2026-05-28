//! Two-Factor Authentication via TOTP (RFC 6238).
//!
//! Stores per-user TOTP secrets and recovery codes in the
//! framework-owned `two_factor_credentials` table. Secrets and
//! recovery codes are encrypted at rest via [`crate::crypto::Crypt`].
//!
//! # Flow
//!
//! 1. [`TwoFactor::enroll`] generates a fresh secret + 10 recovery
//!    codes, persists them encrypted, and returns the otpauth URL, a
//!    QR-code SVG, and the plaintext recovery codes (shown to the
//!    user once and only once).
//! 2. [`TwoFactor::confirm`] sets `confirmed_at` after the user
//!    submits a valid TOTP code from their authenticator app.
//!    Required before [`TwoFactor::verify`] / [`TwoFactor::is_enabled`]
//!    treat 2FA as active.
//! 3. [`TwoFactor::verify`] checks a TOTP code on subsequent logins.
//! 4. [`TwoFactor::consume_recovery_code`] consumes a single recovery
//!    code; subsequent attempts against the same code return false.
//! 5. [`TwoFactor::disable`] removes the row entirely.
//!
//! The user identity is opaque to this module - callers pass any
//! stringy `user_id` (typically `torii::UserId.to_string()`). There
//! is no FK to a user table.

pub mod entity;
pub mod migration;
pub mod migration_replay;
pub mod recovery;

use crate::auth_flows::events::{TwoFactorChallenged, TwoFactorDisabled, TwoFactorEnrolled};
use crate::crypto::Crypt;
use crate::database::DB;
use crate::error::FrameworkError;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter,
};
use totp_rs::{Algorithm, Secret, TOTP};

const ISSUER_ENV: &str = "APP_NAME";
const DEFAULT_ISSUER: &str = "Suprnova";
const RECOVERY_CODE_COUNT: usize = 10;

/// Static facade for the 2FA TOTP lifecycle. See module docs.
pub struct TwoFactor;

/// Successful enrollment payload returned from [`TwoFactor::enroll`].
///
/// The recovery codes are plaintext and MUST be displayed to the user
/// exactly once - there is no API for retrieving them again after the
/// response is dropped.
#[derive(Debug, Clone)]
pub struct EnrollmentResponse {
    /// `otpauth://totp/...` URL suitable for QR-app deep linking.
    pub otpauth_url: String,
    /// SVG wrapping a base64-encoded PNG QR code; safe to embed in
    /// HTML via `{{ enrollment.qr_code_svg | safe }}`.
    pub qr_code_svg: String,
    /// Ten single-use recovery codes. Plaintext - show once.
    pub recovery_codes: Vec<String>,
}

/// Minimum contract for a user the [`TwoFactor`] facade can act on.
///
/// `user_id` is the opaque storage key (e.g.
/// `torii::UserId.to_string()`) and `email` is folded into the
/// `otpauth://` `account_name` segment so authenticator apps render
/// the row with a human-readable label.
pub trait TwoFactorUser: Send + Sync {
    fn user_id(&self) -> &str;
    fn email(&self) -> &str;
}

impl TwoFactor {
    /// Begin enrollment: generate a fresh secret + 10 recovery codes,
    /// persist them encrypted, and return the otpauth URL, a QR-code
    /// SVG, and the plaintext recovery codes (shown to the user once).
    ///
    /// The user must call [`Self::confirm`] with a valid TOTP code
    /// before 2FA actually gates logins — until then
    /// [`Self::is_enabled`] returns `false` and [`Self::verify`]
    /// short-circuits to `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns `FrameworkError::domain(.., 409)` when the user
    /// **already has a confirmed 2FA enrollment**. Overwriting a
    /// confirmed secret without proof of the existing one would let a
    /// session-hijacked attacker pivot from "I have a session" to "I
    /// have 2FA on this account." Call [`Self::re_enroll`] with a
    /// valid TOTP code or recovery code as proof.
    ///
    /// Re-enrolling on an unconfirmed (pending) row is allowed — the
    /// prior enrollment never became authoritative.
    pub async fn enroll<U: TwoFactorUser>(user: &U) -> Result<EnrollmentResponse, FrameworkError> {
        if Self::is_enabled(user).await? {
            return Err(FrameworkError::domain(
                "2FA is already enabled for this account; call re_enroll with a valid TOTP or recovery code as proof to rotate the secret",
                409,
            ));
        }
        Self::write_new_enrollment(user).await
    }

    /// Rotate the secret for an existing confirmed 2FA enrollment.
    /// Requires either a valid current TOTP code or an unused
    /// recovery code as proof of possession.
    ///
    /// On success, the row is overwritten with a fresh secret + 10
    /// fresh recovery codes; `confirmed_at` is reset to NULL so the
    /// user must call [`Self::confirm`] with a code from the new
    /// secret before 2FA is active again.
    ///
    /// # Errors
    ///
    /// - `FrameworkError::domain(.., 401)` when `proof` validates as
    ///   neither a TOTP code (current or within the replay-protected
    ///   window) nor a recovery code.
    /// - `FrameworkError::domain(.., 400)` when no confirmed
    ///   enrollment exists — call [`Self::enroll`] instead.
    pub async fn re_enroll<U: TwoFactorUser>(
        user: &U,
        proof: &str,
    ) -> Result<EnrollmentResponse, FrameworkError> {
        if !Self::is_enabled(user).await? {
            return Err(FrameworkError::domain(
                "no confirmed 2FA enrollment to rotate; call enroll first",
                400,
            ));
        }

        // Accept either path. TOTP path runs verify() which already
        // enforces replay protection; recovery path consumes a code
        // (single-use). Both block attackers without observed proof.
        let totp_accepted = Self::verify(user, proof).await?;
        let proof_accepted = if totp_accepted {
            true
        } else {
            Self::consume_recovery_code(user, proof).await?
        };

        if !proof_accepted {
            return Err(FrameworkError::domain(
                "re-enrollment proof is neither a valid TOTP code nor a recovery code",
                401,
            ));
        }

        Self::write_new_enrollment(user).await
    }

    /// Internal helper — generate + persist a fresh secret. Used by
    /// [`Self::enroll`] (no prior state) and [`Self::re_enroll`]
    /// (after proof). Overwrites any existing row's secret, recovery
    /// codes, and `confirmed_at` (re-confirmation required against
    /// the new secret).
    async fn write_new_enrollment<U: TwoFactorUser>(
        user: &U,
    ) -> Result<EnrollmentResponse, FrameworkError> {
        let secret_bytes = Secret::generate_secret()
            .to_bytes()
            .map_err(|e| FrameworkError::internal(format!("totp secret bytes: {e}")))?;
        let secret_b32 = Secret::Raw(secret_bytes.clone()).to_encoded().to_string();

        let issuer = std::env::var(ISSUER_ENV).unwrap_or_else(|_| DEFAULT_ISSUER.into());
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            secret_bytes,
            Some(issuer),
            user.email().to_string(),
        )
        .map_err(|e| FrameworkError::internal(format!("totp new: {e}")))?;

        let otpauth_url = totp.get_url();
        let qr_b64 = totp
            .get_qr_base64()
            .map_err(|e| FrameworkError::internal(format!("totp qr: {e}")))?;
        let qr_code_svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 256 256\">\
             <image href=\"data:image/png;base64,{qr_b64}\" width=\"256\" height=\"256\"/></svg>"
        );

        let recovery_codes = recovery::generate(RECOVERY_CODE_COUNT);
        let encrypted_secret = Crypt::encrypt_string(&secret_b32)?;
        let encrypted_recovery = Crypt::encrypt_string(&recovery_codes.join("\n"))?;

        upsert_row(
            user.user_id(),
            encrypted_secret,
            None,
            Some(encrypted_recovery),
        )
        .await?;

        Ok(EnrollmentResponse {
            otpauth_url,
            qr_code_svg,
            recovery_codes,
        })
    }

    /// Confirm a pending enrollment with a TOTP code from the user's
    /// authenticator app. On success, stamps `confirmed_at` and
    /// dispatches [`TwoFactorEnrolled`].
    ///
    /// # Errors
    ///
    /// - `FrameworkError::domain(.., 401)` if no row exists for this
    ///   user, or if the supplied code does not match.
    pub async fn confirm<U: TwoFactorUser>(user: &U, code: &str) -> Result<(), FrameworkError> {
        let secret_b32 = load_secret(user.user_id())
            .await?
            .ok_or_else(|| FrameworkError::domain("no pending 2FA enrollment", 401))?;

        if !check_code(&secret_b32, code)? {
            return Err(FrameworkError::domain("invalid 2FA code", 401));
        }

        set_confirmed_at(user.user_id(), chrono::Utc::now()).await?;

        // Discard dispatch errors - the confirmation has already
        // committed; a downstream listener failure must not surface
        // here. The dispatcher logs listener errors via tracing.
        let _ = crate::events::EventFacade::dispatch(TwoFactorEnrolled {
            user_id: user.user_id().to_string(),
        })
        .await;

        Ok(())
    }

    /// Verify a TOTP code for a user with a confirmed enrollment.
    ///
    /// Returns `Ok(false)` when 2FA is not enabled (no row, or row
    /// exists but `confirmed_at` is NULL) or the code does not match.
    /// Storage failures surface as `Err`.
    ///
    /// # Replay protection
    ///
    /// On a successful verify the server's current TOTP timestep is
    /// persisted to `last_used_timestep`. Subsequent verifications
    /// where `current_timestep <= last_used_timestep` are rejected
    /// even when the code itself is structurally valid — preventing
    /// an attacker who observed a valid code from re-submitting it
    /// within the same 30-second window. The legitimate cost is that
    /// a user who needs to 2FA twice within 30 seconds must wait for
    /// the next timestep; for the typical "verify once per login"
    /// flow this is invisible.
    ///
    /// The timestep claim is atomic. The stamp is written with a
    /// conditional `UPDATE ... WHERE last_used_timestep IS NULL OR
    /// last_used_timestep < :current`, and the verify only succeeds
    /// when that statement affects exactly one row. Two concurrent
    /// verifies in the same timestep therefore cannot both win: the
    /// first flips the column, the second's predicate no longer
    /// matches and it is treated as a replay. A plain read-modify-write
    /// would be a TOCTOU race — both verifies read the pre-stamp row,
    /// both validate the same code, both stamp — that silently defeats
    /// the guard under concurrency.
    ///
    /// # Brute-force throttling
    ///
    /// Failed verifies are recorded against the user's email via
    /// [`crate::auth_flows::BruteForce::record_failed_attempt`].
    /// Crossing the configured threshold locks the account from
    /// **both** 2FA and password login until an admin unlocks it or
    /// the lockout window expires — defense in depth against online
    /// brute-force of the TOTP search space. Successful verifies
    /// reset the failed-attempt counter via
    /// [`crate::auth_flows::BruteForce::reset_attempts`].
    pub async fn verify<U: TwoFactorUser>(user: &U, code: &str) -> Result<bool, FrameworkError> {
        let db = DB::connection()?;
        let Some(row) = entity::Entity::find_by_id(user.user_id().to_string())
            .one(db.inner())
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?
        else {
            return Ok(false);
        };
        if row.confirmed_at.is_none() {
            return Ok(false);
        }

        let current_timestep = current_totp_timestep();
        if let Some(last) = row.last_used_timestep
            && current_timestep <= last
        {
            // Fast-path replay rejection: this user already verified at
            // or after the current timestep, so refuse ANY code without
            // even decrypting the secret. This is an optimization and a
            // UX nicety, NOT the authoritative guard — under concurrency
            // two verifies can both read the pre-stamp row and pass here.
            // The atomic claim below is what actually closes the race.
            // Counted as a failed attempt — replays from an observer
            // should trip the lockout.
            record_2fa_failure(user.email()).await;
            return Ok(false);
        }

        let secret_b32 = Crypt::decrypt_string(&row.secret)?;
        if !check_code(&secret_b32, code)? {
            record_2fa_failure(user.email()).await;
            return Ok(false);
        }

        // Atomically claim this timestep. The conditional WHERE turns
        // check-and-stamp into a single statement: the first verify in a
        // given timestep flips `last_used_timestep` to `current`, and any
        // concurrent verify's predicate (`< current`) no longer matches,
        // so it affects zero rows. This is what makes the replay guard
        // hold under concurrency — the previous read-modify-write let two
        // racing verifies both stamp and both succeed (a TOCTOU race).
        let claim = entity::Entity::update_many()
            .col_expr(
                entity::Column::LastUsedTimestep,
                Expr::value(current_timestep),
            )
            .col_expr(entity::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
            .filter(entity::Column::UserId.eq(user.user_id()))
            .filter(
                Condition::any()
                    .add(entity::Column::LastUsedTimestep.is_null())
                    .add(entity::Column::LastUsedTimestep.lt(current_timestep)),
            )
            .exec(db.inner())
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor replay claim: {e}")))?;

        if claim.rows_affected == 0 {
            // A concurrent verify in the same timestep beat us to the
            // claim. Identical outcome to a sequential replay: reject and
            // count it as a failed attempt.
            record_2fa_failure(user.email()).await;
            return Ok(false);
        }

        reset_2fa_failures(user.email()).await;
        Ok(true)
    }

    /// Try to consume one recovery code. Returns `true` if a code
    /// matched and was removed (single-use), `false` if no row, no
    /// codes, no match, or the enrollment has not been confirmed yet.
    ///
    /// Recovery codes are a backup for an **active** 2FA enrollment,
    /// so this method short-circuits to `Ok(false)` while
    /// `confirmed_at` is NULL — matching [`Self::verify`]'s symmetry.
    /// Without this gate, an attacker who triggered enrollment on a
    /// victim account (or any flow that creates the row without
    /// confirming) could authenticate using only a fresh recovery
    /// code, bypassing TOTP entirely.
    pub async fn consume_recovery_code<U: TwoFactorUser>(
        user: &U,
        code: &str,
    ) -> Result<bool, FrameworkError> {
        if !Self::is_enabled(user).await? {
            return Ok(false);
        }
        let consumed = recovery::consume(user.user_id(), code).await?;
        // Same brute-force throttling as TwoFactor::verify — a wrong
        // recovery code counts as a failed attempt against the user's
        // email, so an attacker can't grind the 40-bit code space.
        if consumed {
            reset_2fa_failures(user.email()).await;
        } else {
            record_2fa_failure(user.email()).await;
        }
        Ok(consumed)
    }

    /// Returns `true` when an active (confirmed) 2FA enrollment
    /// exists for this user. Sugar over [`Self::is_enabled_by_id`]
    /// for callers that already hold a [`TwoFactorUser`].
    pub async fn is_enabled<U: TwoFactorUser>(user: &U) -> Result<bool, FrameworkError> {
        Self::is_enabled_by_id(user.user_id()).await
    }

    /// Returns `true` when an active (confirmed) 2FA enrollment
    /// exists for `user_id`.
    ///
    /// String-id variant of [`Self::is_enabled`]. The underlying query
    /// never touches the user's email, so callers that have a bare
    /// user-id string (e.g. inside a login handler after `Auth::attempt`
    /// returns) don't need to construct a [`TwoFactorUser`] just to
    /// answer "should this login go through a challenge?"
    pub async fn is_enabled_by_id(user_id: &str) -> Result<bool, FrameworkError> {
        let db = DB::connection()?;
        let row = entity::Entity::find_by_id(user_id.to_string())
            .one(db.inner())
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?;
        Ok(matches!(row, Some(r) if r.confirmed_at.is_some()))
    }

    /// Begin a 2FA challenge for `user_id`: revoke the user's
    /// remember-me tokens, clear the fully-authenticated session slot
    /// (if any), stash `user_id` as the pending user, and remember
    /// whether the user opted into remember-me at password-login
    /// time. The caller — typically a password-login handler that
    /// just resolved a user for whom [`Self::is_enabled_by_id`]
    /// returned `true` — should then redirect to the challenge page.
    /// The session remains in pending state until
    /// [`Self::complete_challenge`] succeeds (promoting pending →
    /// authed) or the user explicitly cancels via
    /// [`Self::cancel_challenge`].
    ///
    /// `Auth::id()` returns `None` while a challenge is pending, so
    /// any route gated by [`crate::AuthMiddleware`] keeps the user
    /// out. Compose
    /// [`crate::auth_flows::TwoFactorChallengeMiddleware`] in front
    /// of `AuthMiddleware` to redirect pending users to the
    /// challenge page rather than letting them fall through to the
    /// login page.
    ///
    /// # Arguments
    ///
    /// * `user_id` — the id of the user whose password verified.
    /// * `remember` — whether the original login form requested
    ///   remember-me. The preference is stashed in the session and
    ///   consumed by [`Self::complete_challenge`], which re-issues
    ///   the remember-me cookie on a successful challenge. Pass the
    ///   exact `remember` value the caller received from the login
    ///   form — `false` if the form had no remember-me checkbox.
    ///
    /// # Why revoke remember-me
    ///
    /// `Auth::attempt(creds, true)` issues the remember-me cookie
    /// and database row **before** the login handler sees the
    /// result and decides to gate on 2FA. Without an explicit revoke
    /// here, that cookie would outlive the demotion to pending: a
    /// user who closed their browser before completing the
    /// challenge would be auto-logged-in on the next visit via
    /// remember-me, bypassing 2FA entirely. We revoke before
    /// clearing the auth slot because revoke reads `Auth::id()` to
    /// identify whose tokens to delete — clearing first would make
    /// it a no-op.
    ///
    /// The cookie + row that `start_challenge` revokes are the
    /// pre-challenge ones. If `remember` is `true`,
    /// [`Self::complete_challenge`] issues a **fresh** cookie + row
    /// on the post-challenge user-id, so remember-me-with-2FA users
    /// get the same UX as remember-me-without-2FA users without the
    /// caller having to remember to re-issue the cookie themselves.
    pub async fn start_challenge(
        user_id: impl Into<String>,
        remember: bool,
    ) -> Result<(), FrameworkError> {
        let user_id = user_id.into();
        // Revoke FIRST while Auth::id() still resolves the user.
        crate::auth::Auth::revoke_remember_tokens().await?;
        // Pending and authed are mutually exclusive — clear any prior
        // authed state. This also covers the case where Auth::attempt
        // marked the user authed before the caller noticed 2FA was on.
        crate::session::middleware::clear_auth_user();
        crate::session::middleware::set_two_factor_pending(user_id);
        crate::session::middleware::set_two_factor_pending_remember(remember);
        // Mirror the change into request_state so Auth::id() agrees
        // for the rest of THIS request, not only after the next
        // round-trip. `clear_current_user` is the sync primitive —
        // `Auth::clear_authentication` is the broader logout helper
        // (revokes remember-me — already done above — and rotates
        // CSRF, which would be wrong mid-login).
        crate::auth::request_state::clear_current_user();
        Ok(())
    }

    /// Read the user-id of a session that has a 2FA challenge
    /// pending. Returns `None` outside a request scope or when no
    /// challenge is pending. Equivalent to
    /// [`crate::session::middleware::two_factor_pending_user_id`];
    /// exposed on the facade so consumers find it via
    /// `TwoFactor::*` autocomplete.
    pub fn pending_user_id() -> Option<String> {
        crate::session::middleware::two_factor_pending_user_id()
    }

    /// Cancel a pending 2FA challenge — clears the pending user-id
    /// from the session without authenticating anyone. Typical use
    /// is a "back to login" button on the challenge page.
    pub fn cancel_challenge() {
        crate::session::middleware::clear_two_factor_pending();
    }

    /// Complete the 2FA challenge by verifying `code` against the
    /// session's pending user. On success, promotes the pending user
    /// to fully authenticated, rotates the session id and CSRF token
    /// to defeat session fixation, re-issues the remember-me cookie
    /// when the original login form requested it, and dispatches the
    /// standard [`crate::auth::events::Login`] +
    /// [`crate::auth::events::Authenticated`] pair followed by the
    /// 2FA-specific [`crate::auth_flows::events::TwoFactorChallenged`].
    /// Returns the full [`crate::torii_integration::User`] so the
    /// caller can branch the post-login redirect on user attributes.
    ///
    /// Accepts either a current TOTP code or an unused recovery code —
    /// the recovery-code path matches Fortify's challenge controller,
    /// which lets users fall back to a recovery code if they've lost
    /// their authenticator. Recovery codes are consumed single-use on
    /// acceptance.
    ///
    /// Failed code attempts feed the per-account brute-force counter
    /// — the same way [`Self::verify`] does for direct verification —
    /// so an attacker who racked up bad challenge codes will trip
    /// `AccountLocked` after the configured threshold.
    ///
    /// # Promotion contract
    ///
    /// The promotion mirrors [`crate::auth::Auth::login_id`] /
    /// [`crate::auth::Auth::login_remember`]: a fresh session id
    /// (so a session id planted before the challenge cannot ride
    /// the post-challenge auth), a fresh CSRF token (so any cached
    /// pre-auth token cannot be replayed under the new privilege
    /// level), and the auth user written into the session. The
    /// `Login` / `Authenticated` dispatches are the same shape and
    /// guard-attribution as a no-2FA password login, so listeners
    /// that hook those events (last-login timestamps, audit logs,
    /// post-login redirects, …) fire here too.
    ///
    /// # Errors
    ///
    /// - [`FrameworkError::domain`] with status `400` if no challenge
    ///   is pending — the caller must invoke [`Self::start_challenge`]
    ///   (typically from a password-login handler) before
    ///   `complete_challenge` is meaningful.
    /// - [`FrameworkError::domain`] with status `401` if the pending
    ///   user-id no longer resolves to a torii user (deleted mid-
    ///   challenge) or the supplied code validates as neither a TOTP
    ///   code nor a recovery code.
    pub async fn complete_challenge(
        code: &str,
    ) -> Result<crate::torii_integration::User, FrameworkError> {
        let Some(pending_id) = crate::session::middleware::two_factor_pending_user_id() else {
            return Err(FrameworkError::domain(
                "no 2FA challenge pending; submit credentials first",
                400,
            ));
        };

        let Some(user) = crate::torii_integration::find_user_by_id(&pending_id).await? else {
            return Err(FrameworkError::domain(
                "pending 2FA user no longer exists",
                401,
            ));
        };

        // Adapter so we can call the existing `verify` /
        // `consume_recovery_code` — they thread the email through to
        // `BruteForce::record_failed_attempt` for the throttling
        // linkage, but the credential check itself is user-id-keyed.
        struct ChallengeAdapter<'a> {
            user_id: &'a str,
            email: &'a str,
        }
        impl TwoFactorUser for ChallengeAdapter<'_> {
            fn user_id(&self) -> &str {
                self.user_id
            }
            fn email(&self) -> &str {
                self.email
            }
        }

        let adapter = ChallengeAdapter {
            user_id: &pending_id,
            email: &user.email,
        };

        // TOTP first (fast path); fall back to recovery-code consume
        // so the user isn't locked out when they've lost their
        // authenticator app. Both paths independently throttle
        // through `BruteForce::record_failed_attempt`.
        let totp_accepted = Self::verify(&adapter, code).await?;
        let accepted = if totp_accepted {
            true
        } else {
            Self::consume_recovery_code(&adapter, code).await?
        };
        if !accepted {
            return Err(FrameworkError::domain("invalid 2FA code", 401));
        }

        // Read the remember-me preference the user supplied at
        // password-login time BEFORE clearing the pending bag — the
        // bag is about to be torn down as part of the promotion.
        let remember = crate::session::middleware::two_factor_pending_remember();

        // Promote: pending → authed. Mirrors `Auth::login_id`'s
        // contract — rotate the session id to defeat session
        // fixation, set the user, clear pending state, rotate CSRF.
        // A planted pre-challenge session id cannot ride the
        // post-challenge auth.
        crate::session::regenerate_session_id();
        crate::session::middleware::set_auth_user(&pending_id);
        crate::session::middleware::clear_two_factor_pending();
        crate::session::middleware::clear_two_factor_pending_remember();
        crate::session::session_mut(|session| {
            session.csrf_token = crate::session::generate_csrf_token();
        });

        // Re-issue remember-me if the user opted in pre-challenge.
        // The pre-challenge cookie was revoked by `start_challenge`;
        // this is a fresh row + cookie tied to the now-authenticated
        // user-id, with the configured default TTL. Without this,
        // remember-me-with-2FA would silently fail open relative to
        // remember-me-without-2FA.
        if remember {
            let ttl_minutes = (crate::session::SessionConfig::from_env()
                .remember_lifetime
                .as_secs()
                / 60) as i64;
            crate::auth::Auth::issue_remember_cookie(&pending_id, ttl_minutes).await?;
        }

        // Standard login lifecycle events first so listeners that
        // hook `Login` / `Authenticated` (last-login timestamps,
        // audit logs, post-login redirects) fire on the 2FA path
        // too — they cannot rely on `Auth::attempt` having fired
        // them, because attempt completed before 2FA gating
        // demoted the session. Then the 2FA-specific event for
        // code that wants to distinguish "logged in via challenge"
        // from "logged in via password alone."
        //
        // Dispatch errors are intentionally swallowed (logged by
        // the dispatcher) — the promotion has already committed; a
        // listener failure must not surface here.
        let guard = crate::auth::Auth::default_guard_name();
        let _ = crate::events::EventFacade::dispatch(crate::auth::events::Login {
            guard: guard.clone(),
            user_id: pending_id.clone(),
            remember,
        })
        .await;
        let _ = crate::events::EventFacade::dispatch(crate::auth::events::Authenticated {
            guard,
            user_id: pending_id.clone(),
        })
        .await;
        let _ = crate::events::EventFacade::dispatch(TwoFactorChallenged {
            user_id: pending_id,
        })
        .await;

        Ok(user)
    }

    /// Rotate the recovery codes for an active 2FA enrollment.
    ///
    /// Replaces the stored recovery-codes column with a fresh set of
    /// 10 codes. The plaintext codes are returned for one-time display
    /// — there is no API for retrieving them again. The secret and
    /// `confirmed_at` are left untouched (only the recovery-codes
    /// column rotates), so the user's existing authenticator app
    /// continues to work without re-pairing.
    ///
    /// Requires either a current TOTP code or an unused recovery code
    /// as proof of possession — same model as [`Self::re_enroll`]. A
    /// session-hijacked attacker that can reach this endpoint without
    /// proof would otherwise blow away the legitimate user's recovery
    /// codes (a denial-of-service against account recovery).
    ///
    /// # Errors
    ///
    /// - [`FrameworkError::domain`] with status `400` when no
    ///   confirmed enrollment exists — call [`Self::enroll`] /
    ///   [`Self::confirm`] first.
    /// - [`FrameworkError::domain`] with status `401` when `proof`
    ///   validates as neither a current TOTP code nor an unused
    ///   recovery code.
    pub async fn regenerate_recovery_codes<U: TwoFactorUser>(
        user: &U,
        proof: &str,
    ) -> Result<Vec<String>, FrameworkError> {
        if !Self::is_enabled(user).await? {
            return Err(FrameworkError::domain(
                "no confirmed 2FA enrollment; cannot regenerate recovery codes",
                400,
            ));
        }

        // Accept TOTP or recovery-code proof. `verify` enforces atomic
        // replay protection; `consume_recovery_code` burns the code on
        // success. Either path independently blocks an attacker
        // without observed proof.
        let totp_accepted = Self::verify(user, proof).await?;
        let proof_accepted = if totp_accepted {
            true
        } else {
            Self::consume_recovery_code(user, proof).await?
        };

        if !proof_accepted {
            return Err(FrameworkError::domain(
                "regenerate-recovery-codes proof is neither a valid TOTP code nor a recovery code",
                401,
            ));
        }

        let new_codes = recovery::generate(RECOVERY_CODE_COUNT);
        let encrypted = Crypt::encrypt_string(&new_codes.join("\n"))?;

        let db = DB::connection()?;
        let conn = db.inner();
        let row = entity::Entity::find_by_id(user.user_id().to_string())
            .one(conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?
            .ok_or_else(|| FrameworkError::internal("two_factor row vanished mid-regenerate"))?;
        let mut active: entity::ActiveModel = row.into();
        active.recovery_codes = Set(Some(encrypted));
        active.updated_at = Set(chrono::Utc::now());
        active
            .update(conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor update: {e}")))?;

        Ok(new_codes)
    }

    /// Disable 2FA entirely. Deletes the row and dispatches
    /// [`TwoFactorDisabled`] **only** when a row was actually
    /// removed.
    ///
    /// Idempotent: a no-op disable on a user who never enrolled is
    /// not an error. The event only fires on a real state transition
    /// (mirrors the [`super::events::AccountUnlocked`] contract) so
    /// audit listeners see one entry per actual disable, not one per
    /// click on a no-op button.
    pub async fn disable<U: TwoFactorUser>(user: &U) -> Result<(), FrameworkError> {
        let db = DB::connection()?;
        let result = entity::Entity::delete_by_id(user.user_id().to_string())
            .exec(db.inner())
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor delete: {e}")))?;

        if result.rows_affected > 0 {
            // Discard dispatch errors - the delete has already
            // committed; a listener failure must not surface here.
            let _ = crate::events::EventFacade::dispatch(TwoFactorDisabled {
                user_id: user.user_id().to_string(),
            })
            .await;
        }

        Ok(())
    }
}

async fn upsert_row(
    user_id: &str,
    encrypted_secret: String,
    confirmed_at: Option<chrono::DateTime<chrono::Utc>>,
    encrypted_recovery: Option<String>,
) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let conn = db.inner();
    let now = chrono::Utc::now();
    // SeaORM has no portable upsert across MySQL/Postgres/SQLite, so
    // we read-modify-write. Re-enrolling overwrites secret +
    // recovery_codes and clears `confirmed_at`, forcing the user
    // through the confirm flow again with the new authenticator.
    if let Some(existing) = entity::Entity::find_by_id(user_id.to_string())
        .one(conn)
        .await
        .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?
    {
        let mut active: entity::ActiveModel = existing.into();
        active.secret = Set(encrypted_secret);
        active.confirmed_at = Set(confirmed_at);
        active.recovery_codes = Set(encrypted_recovery);
        // Re-enrollment generates a fresh secret — any timestep
        // remembered against the old secret is meaningless.
        active.last_used_timestep = Set(None);
        active.updated_at = Set(now);
        active
            .update(conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor update: {e}")))?;
    } else {
        entity::ActiveModel {
            user_id: Set(user_id.to_string()),
            secret: Set(encrypted_secret),
            confirmed_at: Set(confirmed_at),
            recovery_codes: Set(encrypted_recovery),
            // Fresh enrollment — no prior verification timestep to
            // guard against replay yet.
            last_used_timestep: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(conn)
        .await
        .map_err(|e| FrameworkError::internal(format!("two_factor insert: {e}")))?;
    }
    Ok(())
}

async fn set_confirmed_at(
    user_id: &str,
    when: chrono::DateTime<chrono::Utc>,
) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let conn = db.inner();
    let row = entity::Entity::find_by_id(user_id.to_string())
        .one(conn)
        .await
        .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?
        .ok_or_else(|| FrameworkError::internal("two_factor row missing"))?;
    let mut active: entity::ActiveModel = row.into();
    active.confirmed_at = Set(Some(when));
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(conn)
        .await
        .map_err(|e| FrameworkError::internal(format!("two_factor update: {e}")))?;
    Ok(())
}

async fn load_secret(user_id: &str) -> Result<Option<String>, FrameworkError> {
    let db = DB::connection()?;
    let Some(row) = entity::Entity::find_by_id(user_id.to_string())
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?
    else {
        return Ok(None);
    };
    Ok(Some(Crypt::decrypt_string(&row.secret)?))
}

/// Current server-side TOTP timestep. Used by [`TwoFactor::verify`]
/// for replay protection — a successful verify stamps the row with
/// this value, and subsequent verifies at the same or earlier
/// timestep are refused even when the code itself would structurally
/// validate. 30-second step matches the TOTP construction in
/// [`check_code`] / enrollment.
fn current_totp_timestep() -> i64 {
    chrono::Utc::now().timestamp() / 30
}

/// Best-effort record of a failed 2FA attempt against
/// `BruteForce::record_failed_attempt`. Logs and swallows errors —
/// the throttling layer must never break the auth check it's
/// supplementing. This includes the "torii not initialised" case
/// (test environments that don't boot torii get no throttling, which
/// is acceptable — production deployments always init torii when 2FA
/// is in play).
async fn record_2fa_failure(email: &str) {
    if let Err(e) = crate::auth_flows::BruteForce::record_failed_attempt(email, None).await {
        tracing::debug!(
            "BruteForce::record_failed_attempt skipped for 2FA failure on {email}: {e}"
        );
    }
}

/// Best-effort reset of the failed-attempt counter after a successful
/// 2FA verify or recovery-code consume. Same swallow-and-log posture
/// as [`record_2fa_failure`].
async fn reset_2fa_failures(email: &str) {
    if let Err(e) = crate::auth_flows::BruteForce::reset_attempts(email).await {
        tracing::debug!("BruteForce::reset_attempts skipped for 2FA success on {email}: {e}");
    }
}

/// Verify a TOTP code against a base32-encoded secret. Centralised so
/// `confirm` and `verify` share identical parameters (SHA1 / 6
/// digits / skew=1 / 30s step - matching the enrollment-time
/// construction).
fn check_code(secret_b32: &str, code: &str) -> Result<bool, FrameworkError> {
    let secret_bytes = Secret::Encoded(secret_b32.into())
        .to_bytes()
        .map_err(|e| FrameworkError::internal(format!("decode totp secret: {e}")))?;
    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret_bytes, None, "user".into())
        .map_err(|e| FrameworkError::internal(format!("totp new: {e}")))?;
    totp.check_current(code)
        .map_err(|e| FrameworkError::internal(format!("totp check: {e}")))
}
