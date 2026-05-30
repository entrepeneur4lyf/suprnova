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
//! # `danger-allow-state-serialisation` feature
//!
//! `webauthn-rs` namespaces serde support for `PasskeyRegistration` /
//! `PasskeyAuthentication` behind a feature called `danger-allow-state-serialisation`.
//! The `danger-` prefix is upstream's warning that serialised challenge state
//! can be replayed or tampered with if the storage is unauthenticated or not
//! single-use. We meet both conditions:
//!
//! 1. **Authenticated storage.** Session payloads ride in AES-256-GCM-encrypted
//!    session cookies (keyed by `APP_KEY`). An attacker cannot tamper with the
//!    serialised `PasskeyRegistration` without invalidating the MAC.
//! 2. **Single-use semantics.** Both `finish_registration` and `finish_authentication`
//!    call `session_mut(|s| s.forget(KEY))` immediately after deserialising the
//!    state, so a captured serialised blob cannot be replayed against the same
//!    session.
//!
//! Enabling this feature without those two guarantees would be unsafe.
//!
//! # Re-exports
//!
//! Consumers should `use suprnova::torii_integration::passkey::*` rather than
//! importing `webauthn_rs::prelude` directly; that keeps the public API stable
//! even if we change the underlying webauthn crate.

use std::sync::OnceLock;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use uuid::Uuid;
use webauthn_rs::prelude::{Passkey, Url, Webauthn, WebauthnBuilder};

use super::{
    Session, User, UserId, find_or_create_user_by_email, find_user_by_email_lookup_only, instance,
};
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

// Session keys for in-flight passkey ceremonies.
const SESSION_KEY_REG: &str = "passkey_reg";
const SESSION_KEY_AUTH: &str = "passkey_auth";

/// In-flight passkey **registration** ceremony, persisted in the session
/// between `begin_registration` and `finish_registration`.
///
/// # Security
///
/// Codex review finding #3: storing only the WebAuthn `PasskeyRegistration`
/// state in session let a caller finish registration for a different email
/// than they started with — the finisher could attach the credential to any
/// account by passing a different email to `finish_registration`.
///
/// This struct binds the WebAuthn state to the begin-time **email + user_id**
/// so `finish_registration` can reject calls that target a different account.
#[derive(serde::Serialize, serde::Deserialize)]
struct PasskeyRegistrationCeremony {
    /// The WebAuthn `PasskeyRegistration` challenge state produced by
    /// `webauthn-rs` during `start_passkey_registration`. Single-use:
    /// consumed in `finish_passkey_registration`.
    state: PasskeyRegistration,
    /// The email passed to `begin_registration`. `finish_registration`
    /// must be called with an email that matches this value
    /// (case-insensitive comparison).
    email: String,
    /// The torii `UserId` of the account this ceremony was begun for.
    /// Belt-and-braces: even if email comparison is bypassed, the user
    /// the credential attaches to is the one identified here.
    user_id: UserId,
}

/// In-flight passkey **authentication** ceremony, persisted in the session
/// between `begin_authentication` and `finish_authentication`.
///
/// Binds the WebAuthn authentication challenge to the email + user_id that
/// the flow was begun for, so the finisher cannot pass a different email
/// and end up authenticated as another account.
#[derive(serde::Serialize, serde::Deserialize)]
struct PasskeyAuthenticationCeremony {
    /// The WebAuthn `PasskeyAuthentication` challenge state.
    state: PasskeyAuthentication,
    /// The email passed to `begin_authentication`.
    email: String,
    /// The torii `UserId` of the account this ceremony was begun for.
    user_id: UserId,
}

/// Refuse to mint a ceremony when no `SessionMiddleware` is active.
///
/// Without a session, the ceremony selector that `store_*_ceremony`
/// writes is dropped on the floor, and the matching `finish_*` call
/// would have no way to retrieve it — the ceremony row would orphan
/// in `auth_ceremony_tokens` until the TTL prune. Surface the
/// misconfiguration as an actionable internal error before any DB
/// writes happen.
fn require_session_present(facade_call: &'static str) -> Result<(), FrameworkError> {
    if session_mut(|_| ()).is_some() {
        return Ok(());
    }
    Err(FrameworkError::internal(format!(
        "{facade_call} invoked outside a session — \
         mount SessionMiddleware on the route group that handles \
         this endpoint so the ceremony selector survives the \
         begin/finish round-trip."
    )))
}

/// Store the in-flight registration ceremony in the auth_ceremony_tokens
/// table and put just the ceremony selector in the session. The session
/// is no longer the source of truth for the ceremony payload — the table
/// is, with a UNIQUE selector + conditional DELETE for atomic single-use
/// consumption. ChatGPT audit `torii_integration` HIGH #3.
async fn store_registration_ceremony(
    ceremony: &PasskeyRegistrationCeremony,
) -> Result<(), FrameworkError> {
    // The public `begin_*` facades enforce session presence upfront;
    // this is a defence-in-depth re-check so a future caller that
    // bypasses the facade still cannot orphan a row.
    require_session_present("passkey::store_registration_ceremony")?;
    let selector = uuid::Uuid::new_v4().to_string();
    // 10 minutes is the standard WebAuthn ceremony timeout — generous
    // enough for slow browsers + biometric prompts, short enough that
    // dead ceremonies prune quickly.
    super::ceremony::issue(
        &selector,
        super::ceremony::kind::PASSKEY_REGISTER,
        ceremony,
        10,
    )
    .await?;
    session_mut(|s| s.put(SESSION_KEY_REG, selector));
    Ok(())
}

/// Atomically consume the in-flight registration ceremony from the
/// auth_ceremony_tokens table. The session carries only the selector;
/// the actual payload lives in the table where a conditional DELETE
/// guarantees exactly-once consumption under concurrency.
///
/// Missing ceremony is a **caller** problem (400), not a server fault —
/// the caller either never started a ceremony, the ceremony expired,
/// or another concurrent finish-request already consumed it.
async fn take_registration_ceremony() -> Result<PasskeyRegistrationCeremony, FrameworkError> {
    let selector: String = session()
        .and_then(|s| s.get::<String>(SESSION_KEY_REG))
        .ok_or_else(|| FrameworkError::Domain {
            message: "passkey registration not started or expired".to_string(),
            status_code: 400,
        })?;
    // Best-effort cleanup of the session pointer. The atomic DELETE
    // below is the single-use authority; the session.forget here is
    // janitorial. If two concurrent finishes race, both forget the same
    // selector — harmless idempotency.
    session_mut(|s| {
        s.forget(SESSION_KEY_REG);
    });
    super::ceremony::consume(&selector, super::ceremony::kind::PASSKEY_REGISTER)
        .await?
        .ok_or_else(|| FrameworkError::Domain {
            message: "passkey registration ceremony missing, expired, or already consumed"
                .to_string(),
            status_code: 400,
        })
}

/// Store the in-flight authentication ceremony in the
/// auth_ceremony_tokens table. See [`store_registration_ceremony`].
async fn store_authentication_ceremony(
    ceremony: &PasskeyAuthenticationCeremony,
) -> Result<(), FrameworkError> {
    // Defence-in-depth — the public facade already enforces this.
    require_session_present("passkey::store_authentication_ceremony")?;
    let selector = uuid::Uuid::new_v4().to_string();
    super::ceremony::issue(
        &selector,
        super::ceremony::kind::PASSKEY_AUTHENTICATE,
        ceremony,
        10,
    )
    .await?;
    session_mut(|s| s.put(SESSION_KEY_AUTH, selector));
    Ok(())
}

/// Atomically consume the in-flight authentication ceremony. See
/// [`take_registration_ceremony`].
async fn take_authentication_ceremony() -> Result<PasskeyAuthenticationCeremony, FrameworkError> {
    let selector: String = session()
        .and_then(|s| s.get::<String>(SESSION_KEY_AUTH))
        .ok_or_else(|| FrameworkError::Domain {
            message: "passkey authentication not started or expired".to_string(),
            status_code: 400,
        })?;
    session_mut(|s| {
        s.forget(SESSION_KEY_AUTH);
    });
    super::ceremony::consume(&selector, super::ceremony::kind::PASSKEY_AUTHENTICATE)
        .await?
        .ok_or_else(|| FrameworkError::Domain {
            message: "passkey authentication ceremony missing, expired, or already consumed"
                .to_string(),
            status_code: 400,
        })
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

        // Session-presence check goes BEFORE the
        // `find_or_create_user_by_email` write so a sessionless caller
        // cannot side-effect a user row (account-enumeration / probing
        // hardening) and cannot orphan a ceremony row downstream.
        require_session_present("passkey::begin_registration")?;

        // Get-or-create the user account via the repository layer (no dummy
        // password row created). Registration is the one path where
        // find-or-create is appropriate: a brand-new email registering a
        // passkey *is* a sign-up.
        let user = find_or_create_user_by_email(email).await?;

        // Derive a stable UUID from the opaque torii UserId string.
        // torii's UserId is a prefixed ID (e.g. "usr_..."), not a UUID.
        // We derive a deterministic v5 UUID from it so webauthn always sees
        // the same user_unique_id for the same account.
        let user_uuid = Uuid::new_v5(&Uuid::NAMESPACE_URL, user.id.as_str().as_bytes());

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
            .map_err(|e| {
                FrameworkError::internal(format!("webauthn start_passkey_registration: {e:?}"))
            })?;

        // Derive the human-readable challenge string from the raw challenge bytes.
        let challenge_str = URL_SAFE_NO_PAD.encode(&*ccr.public_key.challenge);
        let rp_id = ccr.public_key.rp.id.clone();

        // Bind the WebAuthn state to the begin-time email + user_id so
        // `finish_registration` can reject calls that target a different
        // account. (Codex review finding #3.)
        store_registration_ceremony(&PasskeyRegistrationCeremony {
            state: pending_reg,
            email: email.to_string(),
            user_id: user.id.clone(),
        })
        .await?;

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

        // Retrieve and consume the in-flight ceremony from the session
        // (one-time use). Missing or expired ceremony → 400.
        let ceremony = take_registration_ceremony().await?;

        // **Codex review finding #3**: reject if the caller-supplied email
        // doesn't match the email the ceremony was begun for. Without this
        // check, a session that started registration for alice@example.com
        // could attach a credential to bob@example.com by calling
        // `finish_registration("bob@example.com", ...)`.
        //
        // Comparison is case-insensitive on the ASCII range (RFC 5321 §2.4
        // says the local-part is technically case-sensitive, but production
        // email systems uniformly normalise to lowercase, and webauthn-rs
        // does not normalise on its own).
        if !email.eq_ignore_ascii_case(&ceremony.email) {
            return Err(FrameworkError::Domain {
                message: "passkey registration email mismatch — session was started for a different account".to_string(),
                status_code: 400,
            });
        }

        // Verify the browser response against the **session-bound** state.
        // WebAuthn finish failures are user/client protocol failures
        // (bad challenge response, malformed attestation, signature
        // mismatch). 401 is the right code — these must not generate
        // 500 responses or internal-error telemetry. ChatGPT audit
        // `torii_integration` HIGH #2.
        let passkey = webauthn
            .finish_passkey_registration(&response, &ceremony.state)
            .map_err(|e| FrameworkError::Domain {
                message: format!("webauthn registration verification failed: {e:?}"),
                status_code: 401,
            })?;

        // Load the user via the **session-bound** email (belt-and-braces:
        // even if the email comparison were bypassed, the lookup goes
        // through the identity that was bound at begin-time). Lookup-only:
        // `find_or_create` here would re-create the user if it had been
        // deleted, which we'd rather surface as an internal-state error.
        let user = find_user_by_email_lookup_only(&ceremony.email)
            .await?
            .ok_or_else(|| FrameworkError::internal(
                "passkey: user disappeared between begin and finish — internal state inconsistency",
            ))?;

        // Defensive guard: the user we re-fetched must be the same user
        // the ceremony was bound to. A mismatch here would indicate a
        // race between two sessions racing to claim the same email, or
        // a bug in `find_or_create_by_email`'s idempotence.
        if user.id != ceremony.user_id {
            return Err(FrameworkError::internal(
                "passkey: user_id changed between begin and finish — internal state inconsistency",
            ));
        }

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

        // Session-presence check FIRST — without a session the
        // ceremony cannot survive past this call, so refuse before
        // touching torii's user store. Distinct from the
        // "passkey authentication failed" branch below, which masks
        // user-existence to defeat enumeration; the
        // session-misconfig error is for the operator, not the caller,
        // and surfaces as an internal error.
        require_session_present("passkey::begin_authentication")?;

        // Resolve user — **lookup-only**. Authentication is the wrong
        // place to create accounts on miss (codex review finding #3): a
        // probing attacker who guesses email addresses could otherwise
        // silently provision accounts they don't own.
        let user = find_user_by_email_lookup_only(email)
            .await?
            .ok_or_else(|| {
                FrameworkError::Domain {
                    // Generic message — do not confirm/deny user existence
                    // beyond what the protocol unavoidably reveals.
                    message: "passkey authentication failed".to_string(),
                    status_code: 401,
                }
            })?;

        // Load stored passkeys.
        let stored_creds = instance()?
            .passkey()
            .get_user_credentials(&user.id)
            .await
            .map_err(|e| FrameworkError::internal(format!("passkey: fetch credentials: {e}")))?;

        if stored_creds.is_empty() {
            return Err(FrameworkError::Domain {
                message: "passkey authentication failed".to_string(),
                status_code: 401,
            });
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

        let (rcr, pending_auth) =
            webauthn
                .start_passkey_authentication(&passkeys)
                .map_err(|e| {
                    FrameworkError::internal(format!(
                        "webauthn start_passkey_authentication: {e:?}"
                    ))
                })?;

        let challenge_str = URL_SAFE_NO_PAD.encode(&*rcr.public_key.challenge);

        // Bind the WebAuthn state to the begin-time email + user_id so
        // `finish_authentication` can reject calls that target a
        // different account.
        store_authentication_ceremony(&PasskeyAuthenticationCeremony {
            state: pending_auth,
            email: email.to_string(),
            user_id: user.id.clone(),
        })
        .await?;

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

        // Retrieve and consume the in-flight ceremony from the session
        // (one-time use). Missing or expired → 400.
        let ceremony = take_authentication_ceremony().await?;

        // **Codex review finding #3**: reject if the caller-supplied email
        // doesn't match the email the ceremony was begun for.
        if !email.eq_ignore_ascii_case(&ceremony.email) {
            return Err(FrameworkError::Domain {
                message: "passkey authentication email mismatch — session was started for a different account".to_string(),
                status_code: 400,
            });
        }

        // Load the user via the **session-bound** email (lookup-only —
        // authentication never provisions accounts).
        let user = find_user_by_email_lookup_only(&ceremony.email)
            .await?
            .ok_or_else(|| FrameworkError::Domain {
                message: "passkey authentication failed".to_string(),
                status_code: 401,
            })?;

        if user.id != ceremony.user_id {
            return Err(FrameworkError::internal(
                "passkey: user_id changed between begin and finish — internal state inconsistency",
            ));
        }

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

        // Verify the browser response against the **session-bound** state.
        // WebAuthn finish failures are user/client protocol failures
        // (bad challenge response, malformed assertion, signature
        // mismatch). 401 is the right code — these must not generate
        // 500 responses or internal-error telemetry. ChatGPT audit
        // `torii_integration` HIGH #2.
        let auth_result: PasskeyAuthenticationResult = webauthn
            .finish_passkey_authentication(&response, &ceremony.state)
            .map_err(|e| FrameworkError::Domain {
                message: format!("webauthn authentication verification failed: {e:?}"),
                status_code: 401,
            })?;

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
                // Persist the new credential blob atomically. Torii's
                // `PasskeyService` only exposes `register_credential` +
                // `delete_credential` — a delete+add sequence opens a
                // window where a storage failure between the two
                // operations drops a valid credential permanently and
                // also lets two concurrent finishes race on the
                // counter. Bypass the two-step API by issuing a
                // single UPDATE against the underlying `passkeys`
                // row that torii owns. The framework shares the same
                // database connection with torii (the
                // `auth_ceremony_tokens` flow already depends on
                // this), and the table shape is fixed by torii's
                // migration `m20250304_000004_create_passkeys_table`.
                update_passkey_credential_blob(&stored.credential_id, &updated_bytes).await?;
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

/// Atomically rewrite the `data_json` blob for a single passkey row,
/// keyed by the base64-encoded credential id that torii stores.
///
/// Torii's `SeaORMPasskeyRepository` writes the credential blob into
/// `passkeys.data_json` as a JSON object (`{"credential_id":...,
/// "public_key":..., "name":..., "created_at":..., "last_used_at":...}`)
/// with the credential id encoded as base64-standard. We mirror that
/// encoding and rewrite only the `public_key` field so the counter
/// update lands as a single statement instead of a delete+insert
/// pair that can drop the credential under storage failure.
///
/// Returns:
/// - `Ok(())` when the UPDATE affected exactly one row.
/// - `Err(_)` for connection/DB errors and for the
///   no-rows-affected case — torii's auth path already proved the
///   credential is in our allow-list, so a missing row at the point
///   of update is an internal inconsistency, not a user error.
async fn update_passkey_credential_blob(
    credential_id: &[u8],
    updated_passkey_bytes: &[u8],
) -> Result<(), FrameworkError> {
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use sea_orm::{ConnectionTrait, Statement};

    let conn = crate::database::DB::connection()?;
    let credential_id_b64 = BASE64_STANDARD.encode(credential_id);

    // First read the current row so we can rewrite only the
    // `public_key` field of `data_json` while preserving
    // `credential_id`, `name`, `created_at`, `last_used_at`. We use a
    // raw SELECT keyed on `credential_id` because the framework does
    // not depend on torii-storage-seaorm's `passkey::Entity`
    // privately, and re-deriving the entity here would couple the
    // framework to torii's migration shape twice.
    let backend = conn.inner().get_database_backend();
    let select_stmt = Statement::from_sql_and_values(
        backend,
        r#"SELECT "data_json" FROM "passkeys" WHERE "credential_id" = $1"#,
        [credential_id_b64.clone().into()],
    );
    let current_row = conn
        .inner()
        .query_one(select_stmt)
        .await
        .map_err(|e| FrameworkError::internal(format!("passkey: select credential row: {e}")))?
        .ok_or_else(|| {
            FrameworkError::internal(
                "passkey: credential row vanished between authentication and counter update — \
                 internal consistency error",
            )
        })?;

    let current_data: String = current_row
        .try_get_by("data_json")
        .map_err(|e| FrameworkError::internal(format!("passkey: read data_json column: {e}")))?;

    let mut json: serde_json::Value = serde_json::from_str(&current_data)
        .map_err(|e| FrameworkError::internal(format!("passkey: parse data_json: {e}")))?;

    // Rewrite the `public_key` field with the freshly-updated passkey
    // blob (counter incremented by webauthn-rs::update_credential).
    json["public_key"] = serde_json::Value::String(BASE64_STANDARD.encode(updated_passkey_bytes));

    let new_data = json.to_string();

    let update_stmt = Statement::from_sql_and_values(
        backend,
        r#"UPDATE "passkeys" SET "data_json" = $1, "updated_at" = $2 WHERE "credential_id" = $3"#,
        [
            new_data.into(),
            chrono::Utc::now().into(),
            credential_id_b64.into(),
        ],
    );
    let result =
        conn.inner().execute(update_stmt).await.map_err(|e| {
            FrameworkError::internal(format!("passkey: update credential blob: {e}"))
        })?;

    if result.rows_affected() != 1 {
        return Err(FrameworkError::internal(format!(
            "passkey: counter update affected {} rows (expected 1) — \
             credential row may have been deleted concurrently",
            result.rows_affected()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod credential_blob_tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use sea_orm::ConnectionTrait;

    async fn fresh_test_db() -> (crate::database::DbConnection, Vec<u8>, Vec<u8>, Vec<u8>) {
        // Each test gets its own unique in-memory database so we never
        // collide with the shared `?cache=shared` pool the integration
        // tests use.
        let dbname = format!("update-passkey-{}", uuid::Uuid::new_v4());
        let url = format!("sqlite:file:{dbname}?mode=memory&cache=shared");
        let cfg = crate::database::DatabaseConfig::builder()
            .url(&url)
            .max_connections(2)
            .min_connections(1)
            .logging(false)
            .build();
        let conn = crate::database::DbConnection::connect(&cfg)
            .await
            .expect("connect ephemeral sqlite for passkey blob test");

        // Minimal `passkeys` table mirroring torii's schema. We don't
        // pull in torii's migrator here — the helper only touches
        // `credential_id` + `data_json` + `updated_at`.
        conn.inner()
            .execute_unprepared(
                r#"CREATE TABLE "passkeys" (
                       "id"             INTEGER PRIMARY KEY AUTOINCREMENT,
                       "user_id"        TEXT NOT NULL,
                       "credential_id"  TEXT NOT NULL,
                       "data_json"      TEXT NOT NULL,
                       "created_at"     TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                       "updated_at"     TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
                   )"#,
            )
            .await
            .expect("create passkeys table");

        let credential_id = vec![1u8, 2, 3, 4, 5];
        let old_pk = vec![10u8, 11, 12];
        let new_pk = vec![20u8, 21, 22, 23, 24];

        let credential_id_b64 = BASE64_STANDARD.encode(&credential_id);
        let data_json = serde_json::json!({
            "credential_id": credential_id_b64,
            "public_key": BASE64_STANDARD.encode(&old_pk),
            "name": "Test Key",
            "created_at": chrono::Utc::now().to_rfc3339(),
            "last_used_at": serde_json::Value::Null,
        })
        .to_string();

        let insert = sea_orm::Statement::from_sql_and_values(
            conn.inner().get_database_backend(),
            r#"INSERT INTO "passkeys" ("user_id", "credential_id", "data_json")
               VALUES ($1, $2, $3)"#,
            [
                "usr_test".into(),
                credential_id_b64.into(),
                data_json.into(),
            ],
        );
        conn.inner().execute(insert).await.expect("insert row");

        (conn, credential_id, old_pk, new_pk)
    }

    /// The helper must rewrite the row in place: row count stays 1,
    /// `public_key` reflects the new bytes, and the credential_id /
    /// name / created_at fields are preserved.
    #[tokio::test]
    async fn update_blob_preserves_row_and_overwrites_public_key() {
        let (conn, credential_id, _old_pk, new_pk) = fresh_test_db().await;
        crate::App::singleton(conn.clone());

        update_passkey_credential_blob(&credential_id, &new_pk)
            .await
            .expect("update succeeds");

        // Row count must still be exactly 1.
        let count_stmt = sea_orm::Statement::from_sql_and_values(
            conn.inner().get_database_backend(),
            r#"SELECT COUNT(*) AS n FROM "passkeys""#,
            vec![],
        );
        let row = conn.inner().query_one(count_stmt).await.unwrap().unwrap();
        let n: i64 = row.try_get_by("n").unwrap();
        assert_eq!(n, 1, "row count must remain 1 after atomic update");

        // `data_json.public_key` must reflect the freshly-encoded
        // bytes, and the rest of the JSON must survive.
        let credential_id_b64 = BASE64_STANDARD.encode(&credential_id);
        let select_stmt = sea_orm::Statement::from_sql_and_values(
            conn.inner().get_database_backend(),
            r#"SELECT "data_json" FROM "passkeys" WHERE "credential_id" = $1"#,
            [credential_id_b64.into()],
        );
        let row = conn.inner().query_one(select_stmt).await.unwrap().unwrap();
        let data: String = row.try_get_by("data_json").unwrap();
        let json: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(
            json["public_key"].as_str().unwrap(),
            BASE64_STANDARD.encode(&new_pk),
            "public_key must reflect updated bytes"
        );
        assert_eq!(json["name"].as_str().unwrap(), "Test Key");
    }

    /// Calling the helper for a credential that doesn't exist must
    /// produce an internal error rather than silently no-op.
    #[tokio::test]
    async fn update_blob_errors_when_credential_missing() {
        let (conn, _credential_id, _, new_pk) = fresh_test_db().await;
        crate::App::singleton(conn);

        let err = update_passkey_credential_blob(&[99u8, 99, 99], &new_pk)
            .await
            .expect_err("missing credential must surface an error");
        let msg = err.to_string();
        assert!(
            msg.contains("vanished") || msg.contains("0 rows"),
            "expected vanished/0-rows error, got: {msg}"
        );
    }
}
