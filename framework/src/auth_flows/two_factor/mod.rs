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

use crate::auth_flows::events::{TwoFactorDisabled, TwoFactorEnrolled};
use crate::crypto::Crypt;
use crate::database::DB;
use crate::error::FrameworkError;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
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
    /// persist them encrypted (overwriting any prior row for this
    /// user, including a previously confirmed enrollment), and return
    /// the otpauth URL, a QR-code SVG, and the plaintext recovery
    /// codes.
    ///
    /// The user must call [`Self::confirm`] with a valid TOTP code
    /// before 2FA actually gates logins - until then
    /// [`Self::is_enabled`] returns `false` and [`Self::verify`]
    /// short-circuits to `Ok(false)`.
    pub async fn enroll<U: TwoFactorUser>(
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
    pub async fn confirm<U: TwoFactorUser>(
        user: &U,
        code: &str,
    ) -> Result<(), FrameworkError> {
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
    pub async fn verify<U: TwoFactorUser>(
        user: &U,
        code: &str,
    ) -> Result<bool, FrameworkError> {
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
            // Within or before the timestep the user last successfully
            // verified at — refuse to accept ANY code, even one that
            // would structurally validate. Closes the in-window replay
            // window.
            return Ok(false);
        }

        let secret_b32 = Crypt::decrypt_string(&row.secret)?;
        let matched = check_code(&secret_b32, code)?;
        if matched {
            // Persist the timestep so the same code (or any other
            // structurally-valid code in this window) can't be
            // replayed within this 30-second slot.
            let mut active: entity::ActiveModel = row.into();
            active.last_used_timestep = Set(Some(current_timestep));
            active.updated_at = Set(chrono::Utc::now());
            active.update(db.inner()).await.map_err(|e| {
                FrameworkError::internal(format!("two_factor update: {e}"))
            })?;
        }
        Ok(matched)
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
        recovery::consume(user.user_id(), code).await
    }

    /// Returns `true` when an active (confirmed) 2FA enrollment
    /// exists for this user.
    pub async fn is_enabled<U: TwoFactorUser>(user: &U) -> Result<bool, FrameworkError> {
        let db = DB::connection()?;
        let row = entity::Entity::find_by_id(user.user_id().to_string())
            .one(db.inner())
            .await
            .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?;
        Ok(matches!(row, Some(r) if r.confirmed_at.is_some()))
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

/// Verify a TOTP code against a base32-encoded secret. Centralised so
/// `confirm` and `verify` share identical parameters (SHA1 / 6
/// digits / skew=1 / 30s step - matching the enrollment-time
/// construction).
fn check_code(secret_b32: &str, code: &str) -> Result<bool, FrameworkError> {
    let secret_bytes = Secret::Encoded(secret_b32.into())
        .to_bytes()
        .map_err(|e| FrameworkError::internal(format!("decode totp secret: {e}")))?;
    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret_bytes,
        None,
        "user".into(),
    )
    .map_err(|e| FrameworkError::internal(format!("totp new: {e}")))?;
    totp.check_current(code)
        .map_err(|e| FrameworkError::internal(format!("totp check: {e}")))
}
