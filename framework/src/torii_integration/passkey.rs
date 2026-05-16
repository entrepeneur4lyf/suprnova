//! Passkey (WebAuthn/FIDO2) authentication facade.
//!
//! Suprnova wraps [`webauthn_rs`] for challenge generation and verification, and
//! delegates long-term credential storage to torii's passkey service.
//!
//! # Architecture
//!
//! torii 0.5.x's `PasskeyAuth` surface is a low-level credential store:
//! `register_credential(user_id, credential_id_bytes, public_key_bytes, name)`.
//! It has **no** built-in WebAuthn challenge generation — that is handled by
//! `webauthn_rs::Webauthn`, which Suprnova builds from `ToriiConfig::passkey_rp_id`
//! and `ToriiConfig::passkey_rp_origin` at init time.
//!
//! In-flight registration and authentication state is stored in the user's
//! **session** (via `session_mut` / `session`) under provider-scoped keys.
//! This is session-safe: it survives process restarts (session is
//! database-backed), works across multi-replica deployments, and is scoped to
//! the initiating browser session — no cross-session leakage.
//!
//! # Re-exports
//!
//! Consumers should `use suprnova::torii_integration::passkey::*` rather than
//! importing `webauthn_rs::prelude` directly; that keeps the public API stable
//! even if we change the underlying webauthn crate.

use std::sync::OnceLock;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use uuid::Uuid;
use webauthn_rs::prelude::{Passkey, Url, Webauthn, WebauthnBuilder};

use super::{Session, User, find_or_create_user_by_email, instance};
use crate::error::FrameworkError;
use crate::session::{session, session_mut};

// ─────────────────────────────────────────────────────────────────────────────
// Public re-exports — consumers never need to import webauthn_rs directly
// ─────────────────────────────────────────────────────────────────────────────

pub use webauthn_rs::prelude::{
    AuthenticationResult as PasskeyAuthenticationResult, CreationChallengeResponse,
    PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

// ─────────────────────────────────────────────────────────────────────────────
// Global state
// ─────────────────────────────────────────────────────────────────────────────

/// The global `Webauthn` instance, built once from `ToriiConfig` at init time.
static WEBAUTHN: OnceLock<Webauthn> = OnceLock::new();

// Session keys for in-flight passkey state.
const SESSION_KEY_REG: &str = "passkey_reg";
const SESSION_KEY_AUTH: &str = "passkey_auth";

/// Store the in-flight `PasskeyRegistration` in the current request session.
fn store_reg_state(reg: &PasskeyRegistration) -> Result<(), FrameworkError> {
    let json = serde_json::to_string(reg)
        .map_err(|e| FrameworkError::internal(format!("passkey: serialize reg state: {e}")))?;
    session_mut(|s| s.put(SESSION_KEY_REG, json));
    Ok(())
}

/// Retrieve and consume the in-flight `PasskeyRegistration` from the session.
fn take_reg_state() -> Result<PasskeyRegistration, FrameworkError> {
    let json: String = session()
        .and_then(|s| s.get::<String>(SESSION_KEY_REG))
        .ok_or_else(|| FrameworkError::internal("passkey: no registration in progress (session key missing)"))?;
    session_mut(|s| { s.forget(SESSION_KEY_REG); });
    serde_json::from_str(&json)
        .map_err(|e| FrameworkError::internal(format!("passkey: deserialize reg state: {e}")))
}

/// Store the in-flight `PasskeyAuthentication` in the current request session.
fn store_auth_state(auth: &PasskeyAuthentication) -> Result<(), FrameworkError> {
    let json = serde_json::to_string(auth)
        .map_err(|e| FrameworkError::internal(format!("passkey: serialize auth state: {e}")))?;
    session_mut(|s| s.put(SESSION_KEY_AUTH, json));
    Ok(())
}

/// Retrieve and consume the in-flight `PasskeyAuthentication` from the session.
fn take_auth_state() -> Result<PasskeyAuthentication, FrameworkError> {
    let json: String = session()
        .and_then(|s| s.get::<String>(SESSION_KEY_AUTH))
        .ok_or_else(|| FrameworkError::internal("passkey: no authentication in progress (session key missing)"))?;
    session_mut(|s| { s.forget(SESSION_KEY_AUTH); });
    serde_json::from_str(&json)
        .map_err(|e| FrameworkError::internal(format!("passkey: deserialize auth state: {e}")))
}

/// Initialise the global `Webauthn` instance.
///
/// Called automatically by [`crate::torii_integration::init_torii`] when
/// `ToriiConfig` has passkey fields set. Safe to call multiple times — subsequent
/// calls are no-ops.
///
/// # Errors
///
/// Returns [`FrameworkError`] if `rp_id` is not a valid effective domain of
/// `rp_origin` (a `webauthn_rs` constraint).
pub(crate) fn init_webauthn(rp_id: &str, rp_origin: &str) -> Result<(), FrameworkError> {
    if WEBAUTHN.get().is_some() {
        return Ok(());
    }

    let origin = Url::parse(rp_origin)
        .map_err(|e| FrameworkError::internal(format!("passkey rp_origin invalid URL: {e}")))?;

    let webauthn = WebauthnBuilder::new(rp_id, &origin)
        .map_err(|e| FrameworkError::internal(format!("webauthn builder: {e:?}")))?
        .rp_name(rp_id)
        .build()
        .map_err(|e| FrameworkError::internal(format!("webauthn build: {e:?}")))?;

    // Race is harmless — both instances are equivalent.
    let _ = WEBAUTHN.set(webauthn);
    Ok(())
}

fn webauthn_instance() -> Result<&'static Webauthn, FrameworkError> {
    WEBAUTHN
        .get()
        .ok_or_else(|| FrameworkError::internal("Webauthn not initialised. Call init_torii() with passkey_rp_id / passkey_rp_origin set."))
}

// ─────────────────────────────────────────────────────────────────────────────
// Suprnova facade types
// ─────────────────────────────────────────────────────────────────────────────

/// The result of a successful `begin_registration` call.
///
/// Pass `raw_options` (as JSON) to the browser's `navigator.credentials.create()`.
/// The `challenge` and `rp_id` fields are extracted for convenience — use them
/// to render a human-readable confirmation or to log the challenge for debugging.
#[derive(Debug)]
pub struct PasskeyRegistrationChallenge {
    /// Base64url-encoded challenge bytes (the `challenge` field inside
    /// `raw_options.publicKey`).
    pub challenge: String,
    /// The email that was passed to `begin_registration`.
    pub user_email: String,
    /// Relying-party identifier (e.g. `"localhost"` or `"example.com"`).
    pub rp_id: String,
    /// The full `CreationChallengeResponse` to send verbatim to the browser.
    pub raw_options: CreationChallengeResponse,
}

/// The result of a successful `begin_authentication` call.
///
/// Pass `raw_options` (as JSON) to the browser's `navigator.credentials.get()`.
#[derive(Debug)]
pub struct PasskeyAuthenticationChallenge {
    /// Base64url-encoded challenge bytes.
    pub challenge: String,
    /// The email that was passed to `begin_authentication`.
    pub user_email: String,
    /// The full `RequestChallengeResponse` to send verbatim to the browser.
    pub raw_options: RequestChallengeResponse,
}

// ─────────────────────────────────────────────────────────────────────────────
// Facade
// ─────────────────────────────────────────────────────────────────────────────

/// Facade for passkey (WebAuthn/FIDO2) authentication operations.
///
/// Obtained via [`crate::Auth::passkey()`].
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::Auth;
///
/// // Registration
/// let challenge = Auth::passkey()
///     .begin_registration("alice@example.com")
///     .await?;
/// // Send challenge.raw_options as JSON to the browser.
///
/// // After the browser calls navigator.credentials.create():
/// let user = Auth::passkey()
///     .finish_registration("alice@example.com", credential_from_browser)
///     .await?;
///
/// // Authentication
/// let auth_challenge = Auth::passkey()
///     .begin_authentication("alice@example.com")
///     .await?;
/// // Send auth_challenge.raw_options as JSON to the browser.
///
/// // After the browser calls navigator.credentials.get():
/// let (user, session) = Auth::passkey()
///     .finish_authentication("alice@example.com", credential_from_browser)
///     .await?;
/// ```
pub struct PasskeyAuth;

impl PasskeyAuth {
    /// Begin the passkey registration ceremony for a user identified by email.
    ///
    /// If no account with this email exists, one is created automatically.
    /// The returned [`PasskeyRegistrationChallenge`] contains `raw_options` which
    /// should be sent as JSON to the browser for `navigator.credentials.create()`.
    ///
    /// The in-flight registration state is stored server-side keyed by `email`;
    /// call [`finish_registration`] with the same email to complete the flow.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError`] if Torii or Webauthn is not initialised, or if
    /// the user could not be created.
    pub async fn begin_registration(
        &self,
        email: &str,
    ) -> Result<PasskeyRegistrationChallenge, FrameworkError> {
        let webauthn = webauthn_instance()?;

        // Get-or-create the user account via the repository layer (no dummy
        // password row created).
        let user = find_or_create_user_by_email(email).await?;

        // Derive a stable UUID from the opaque torii UserId string.
        // torii's UserId is a prefixed ID (e.g. "usr_..."), not a UUID.
        // We derive a deterministic v5 UUID from it so webauthn always sees
        // the same user_unique_id for the same account.
        let user_uuid =
            Uuid::new_v5(&Uuid::NAMESPACE_URL, user.id.as_str().as_bytes());

        // Get existing credentials so webauthn can exclude them.
        let existing: Vec<webauthn_rs::prelude::CredentialID> = instance()?
            .passkey()
            .get_user_credentials(&user.id)
            .await
            .map_err(|e| FrameworkError::internal(format!("passkey: fetch credentials: {e}")))?
            .into_iter()
            .map(|c| c.credential_id.into())
            .collect();

        let exclude = if existing.is_empty() {
            None
        } else {
            Some(existing)
        };

        let (ccr, pending_reg) = webauthn
            .start_passkey_registration(user_uuid, email, email, exclude)
            .map_err(|e| FrameworkError::internal(format!("webauthn start_passkey_registration: {e:?}")))?;

        // Derive the human-readable challenge string from the raw challenge bytes.
        let challenge_str = URL_SAFE_NO_PAD.encode(&*ccr.public_key.challenge);
        let rp_id = ccr.public_key.rp.id.clone();

        // Persist in-flight state in the user's session (not a process-local map).
        store_reg_state(&pending_reg)?;

        Ok(PasskeyRegistrationChallenge {
            challenge: challenge_str,
            user_email: email.to_string(),
            rp_id,
            raw_options: ccr,
        })
    }

    /// Complete the passkey registration ceremony.
    ///
    /// `response` is the `RegisterPublicKeyCredential` returned by the browser
    /// after `navigator.credentials.create()`. The email must match the one used
    /// in the preceding `begin_registration` call.
    ///
    /// On success the credential is persisted via torii and the user is returned.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError`] if:
    /// - No in-flight registration state exists for this email.
    /// - Webauthn verification fails (bad signature, wrong origin, etc.).
    /// - Credential storage fails.
    pub async fn finish_registration(
        &self,
        email: &str,
        response: RegisterPublicKeyCredential,
    ) -> Result<User, FrameworkError> {
        let webauthn = webauthn_instance()?;

        // Retrieve and consume in-flight state from the session (one-time use).
        let reg_state = take_reg_state()?;

        // Verify the browser response and extract the Passkey.
        let passkey = webauthn
            .finish_passkey_registration(&response, &reg_state)
            .map_err(|e| FrameworkError::internal(format!("webauthn finish_passkey_registration: {e:?}")))?;

        // Load the user (must exist — begin_registration created them).
        let user = find_or_create_user_by_email(email).await?;

        // Serialise the webauthn Passkey struct into bytes for torii storage.
        // Torii's passkey store is a raw-byte key/value; we store the JSON
        // representation of `Passkey` as the public_key bytes.
        let cred_id: Vec<u8> = passkey.cred_id().to_vec();
        let passkey_bytes = serde_json::to_vec(&passkey)
            .map_err(|e| FrameworkError::internal(format!("passkey: serialize passkey: {e}")))?;

        instance()?
            .passkey()
            .register_credential(&user.id, cred_id, passkey_bytes, None)
            .await
            .map_err(|e| FrameworkError::internal(format!("passkey: store credential: {e}")))?;

        Ok(user)
    }

    /// Begin the passkey authentication ceremony for a user identified by email.
    ///
    /// All registered passkeys for the user are loaded and passed to webauthn as
    /// the allow-list. The returned [`PasskeyAuthenticationChallenge`] contains
    /// `raw_options` which should be sent to the browser for
    /// `navigator.credentials.get()`.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError`] if:
    /// - Torii or Webauthn is not initialised.
    /// - The user has no registered passkeys.
    pub async fn begin_authentication(
        &self,
        email: &str,
    ) -> Result<PasskeyAuthenticationChallenge, FrameworkError> {
        let webauthn = webauthn_instance()?;

        // Resolve user — we need the user_id to fetch credentials.
        let user = find_or_create_user_by_email(email).await?;

        // Load stored passkeys.
        let stored_creds = instance()?
            .passkey()
            .get_user_credentials(&user.id)
            .await
            .map_err(|e| FrameworkError::internal(format!("passkey: fetch credentials: {e}")))?;

        if stored_creds.is_empty() {
            return Err(FrameworkError::internal(
                "passkey: user has no registered passkeys",
            ));
        }

        // Deserialise the stored passkey blobs back into webauthn Passkey structs.
        let passkeys: Vec<Passkey> = stored_creds
            .into_iter()
            .map(|c| {
                serde_json::from_slice::<Passkey>(&c.public_key).map_err(|e| {
                    FrameworkError::internal(format!("passkey: deserialize credential: {e}"))
                })
            })
            .collect::<Result<_, _>>()?;

        let (rcr, pending_auth) = webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(|e| FrameworkError::internal(format!("webauthn start_passkey_authentication: {e:?}")))?;

        let challenge_str = URL_SAFE_NO_PAD.encode(&*rcr.public_key.challenge);

        // Persist in-flight state in the user's session.
        store_auth_state(&pending_auth)?;

        Ok(PasskeyAuthenticationChallenge {
            challenge: challenge_str,
            user_email: email.to_string(),
            raw_options: rcr,
        })
    }

    /// Complete the passkey authentication ceremony.
    ///
    /// `response` is the `PublicKeyCredential` returned by the browser after
    /// `navigator.credentials.get()`. The email must match the one used in the
    /// preceding `begin_authentication` call.
    ///
    /// On success the authenticator's counter is updated and a new session is
    /// created. Returns the authenticated user and session.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError`] if:
    /// - No in-flight authentication state exists for this email.
    /// - Webauthn verification fails.
    /// - Session creation fails.
    pub async fn finish_authentication(
        &self,
        email: &str,
        response: PublicKeyCredential,
    ) -> Result<(User, Session), FrameworkError> {
        let webauthn = webauthn_instance()?;

        // Retrieve and consume in-flight state from the session (one-time use).
        let auth_state_val = take_auth_state()?;

        // Load the user and their stored passkeys (needed for finish_passkey_authentication).
        let user = find_or_create_user_by_email(email).await?;

        let stored_creds = instance()?
            .passkey()
            .get_user_credentials(&user.id)
            .await
            .map_err(|e| FrameworkError::internal(format!("passkey: fetch credentials: {e}")))?;

        let mut passkeys: Vec<Passkey> = stored_creds
            .iter()
            .map(|c| {
                serde_json::from_slice::<Passkey>(&c.public_key).map_err(|e| {
                    FrameworkError::internal(format!("passkey: deserialize credential: {e}"))
                })
            })
            .collect::<Result<_, _>>()?;

        // Verify the browser response.
        let auth_result: PasskeyAuthenticationResult = webauthn
            .finish_passkey_authentication(&response, &auth_state_val)
            .map_err(|e| FrameworkError::internal(format!("webauthn finish_passkey_authentication: {e:?}")))?;

        // Update the counter on the matching passkey and persist.
        // We assert the matched credential is present — webauthn verifies it is in
        // the allow-list we provided, so a mismatch here would be an internal bug.
        let used_cred_id = auth_result.cred_id();
        let mut updated = false;
        for (stored, passkey) in stored_creds.iter().zip(passkeys.iter_mut()) {
            if stored.credential_id == used_cred_id.as_ref() {
                passkey.update_credential(&auth_result);
                let updated_bytes = serde_json::to_vec(passkey).map_err(|e| {
                    FrameworkError::internal(format!("passkey: serialize updated passkey: {e}"))
                })?;
                // Re-register to update bytes (torii has no update API; delete + add).
                instance()?
                    .passkey()
                    .delete_credential(&stored.credential_id)
                    .await
                    .map_err(|e| FrameworkError::internal(format!("passkey: delete old credential: {e}")))?;
                instance()?
                    .passkey()
                    .register_credential(
                        &user.id,
                        stored.credential_id.clone(),
                        updated_bytes,
                        stored.name.clone(),
                    )
                    .await
                    .map_err(|e| FrameworkError::internal(format!("passkey: update credential: {e}")))?;
                updated = true;
                break;
            }
        }

        // Defensive guard: webauthn verifies the credential is in the allow-list
        // we provided; if we still didn't find it, something is inconsistent.
        if !updated {
            return Err(FrameworkError::internal(
                "passkey: authenticated credential not found in stored set — internal consistency error",
            ));
        }

        // Create a new session.
        let session = instance()?
            .create_session(&user.id, None, None)
            .await
            .map_err(|e| FrameworkError::internal(format!("passkey: create session: {e}")))?;

        Ok((user, session))
    }
}
