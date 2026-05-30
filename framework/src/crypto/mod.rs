//! Application-level encryption.
//!
//! [`Crypt`] is a Laravel-style static facade for AES-256-GCM encryption.
//! The active key ring is held in a process-wide [`OnceLock`] populated
//! by `Server::from_config()` from the `APP_KEY` and (optionally)
//! `APP_KEY_PREVIOUS` environment variables.
//!
//! # Key rotation
//!
//! Production-grade key rotation is supported via a key *ring*: one
//! current key (used for all new encryptions) plus an ordered list of
//! previous keys (tried as fallbacks during decrypt). Operators can
//! roll `APP_KEY` without re-encrypting every existing column in lock-
//! step:
//!
//! 1. Set `APP_KEY_PREVIOUS=<old key>` (comma-separated for multi-step
//!    rotation: `<oldest>,...,<newest>`).
//! 2. Set the new `APP_KEY` value.
//! 3. Deploy. New writes use the new key; reads of old data fall
//!    through to the previous list and emit a `tracing::warn!` per
//!    decrypt so an operator-side re-encrypt job can be scheduled.
//! 4. Run a re-encrypt job that reads + saves every model with an
//!    encrypted cast — `Cast::to_storage` always uses the current key,
//!    so a no-op `find(); save()` migrates the row.
//! 5. Remove `APP_KEY_PREVIOUS` after the job finishes.
//!
//! Encryption *always* uses the current key. Decryption tries current
//! first; if that fails, each previous key is tried in order. On a
//! previous-key hit, a `tracing::warn!` is emitted (no plaintext or
//! ciphertext in the log payload — just the fact + an opaque
//! "re-encrypt to remove APP_KEY_PREVIOUS dependency" hint) so admins
//! know to schedule a re-encrypt pass.
//!
//! # Production hardening
//!
//! The `_test_install_*` helpers below are gated behind
//! `cfg(any(test, feature = "testing"))`. The `testing` feature is part
//! of `suprnova`'s default feature set so that downstream test suites
//! can use `Storage::fake()` and similar helpers without opting in. In
//! production binaries this means the test hooks are present in the
//! compiled artifact, but they remain unreachable for two independent
//! reasons:
//!
//! 1. They are `#[doc(hidden)]` and prefixed `_test_` so application
//!    code can't reach them without going out of its way.
//! 2. `Server::from_config` validates `APP_KEY` *on every boot*, not
//!    only when the key ring is uninitialized — so a hypothetical
//!    test-hook install in production would still fail boot if
//!    `APP_KEY` is missing or malformed. The validation is the
//!    load-bearing defense; the cfg gate is defense in depth.
//!
//! Operators who want the test hooks entirely absent from production
//! artifacts should depend on `suprnova` with `default-features = false`
//! and explicitly enable only what they ship. The defense-in-depth
//! posture above means this is a tightening, not a fix — the boot
//! validation closes the actual exploit either way.

pub(crate) mod aead;
pub mod key;

pub use key::EncryptionKey;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::OnceLock;

use crate::FrameworkError;
use crate::config::Environment;

/// Cryptographic purpose tag bound into AES-GCM associated data (AAD)
/// to give every surface its own decrypt domain.
///
/// The framework reuses one `APP_KEY` to encrypt cookies, pagination
/// cursors, 2FA secrets, recovery codes, and column-level cast values.
/// Without domain separation, a ciphertext produced for one surface
/// could be replayed into another that happens to accept the same
/// plaintext shape — the crypto layer would not catch the mismatch.
///
/// Each variant maps to a stable label (e.g. `b"suprnova:cookie:v1"`)
/// that is passed as AAD to AES-256-GCM. GCM mixes the AAD into the
/// authentication tag without including it in the wire bytes, so:
///
/// - Encrypting with `Cookie` and decrypting with `Cursor` fails the
///   tag check — replay across surfaces is rejected at the crypto
///   layer, before any post-decrypt parsing.
/// - The on-wire format is unchanged: still
///   `base64(nonce || ciphertext || tag)`. AAD is an authentication
///   *input*, not part of the ciphertext.
///
/// Adding a new surface (e.g. a future "queue payload encryption") is
/// adding a new variant + label here, not changing the wire format.
///
/// The `:v1` suffix is reserved for a future label rotation: bumping
/// to `:v2` invalidates old ciphertext for that surface only, leaving
/// the rest of the key/wire format alone.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CryptPurpose {
    /// Encrypted HTTP cookies built via [`crate::http::Cookie::encrypted`]
    /// and read via [`crate::http::Cookie::read_encrypted`]. Includes
    /// the session cookie, the remember-me cookie, and the
    /// maintenance-mode bypass cookie.
    Cookie,
    /// Pagination cursors produced by
    /// [`crate::pagination::CursorPaginator::encode_value`] and consumed
    /// by [`crate::pagination::CursorPaginator::decode_value`]. Cursors
    /// travel on the wire (typically `?cursor=…`).
    Cursor,
    /// The encrypted base32 TOTP secret stored on
    /// `two_factor_authentications.secret`.
    TwoFactorSecret,
    /// The newline-joined plaintext recovery codes stored on
    /// `two_factor_authentications.recovery_codes`. Distinct from
    /// [`Self::TwoFactorSecret`] so ciphertext from one column cannot
    /// be replayed into the other within the same row.
    TwoFactorRecovery,
    /// Column values produced by the `AsEncrypted*` casts in
    /// [`crate::eloquent::casts`]. One label covers all four cast
    /// variants — within-cast replay across columns requires a DB
    /// write, at which point the attacker already has access to the
    /// stored ciphertext.
    Cast,
}

impl CryptPurpose {
    /// The stable byte label bound as AAD when encrypting / decrypting
    /// under this purpose.
    pub(crate) fn aad(self) -> &'static [u8] {
        match self {
            CryptPurpose::Cookie => b"suprnova:cookie:v1",
            CryptPurpose::Cursor => b"suprnova:cursor:v1",
            CryptPurpose::TwoFactorSecret => b"suprnova:2fa:secret:v1",
            CryptPurpose::TwoFactorRecovery => b"suprnova:2fa:recovery:v1",
            CryptPurpose::Cast => b"suprnova:cast:v1",
        }
    }
}

/// Internal process-wide key ring. Public via the [`Crypt`] facade.
///
/// `current` is used for every encrypt; decrypt tries `current` first,
/// then each entry in `previous` in order. The ring is sealed for the
/// lifetime of the process after [`Crypt::init`] /
/// [`Crypt::init_with_keyring`] is called — to "rotate" you redeploy
/// with new env vars.
pub(crate) struct KeyRing {
    pub(crate) current: EncryptionKey,
    pub(crate) previous: Vec<EncryptionKey>,
}

static CRYPT_RING: OnceLock<KeyRing> = OnceLock::new();

/// Where a successful decrypt sourced its key.
///
/// Exposed (via the `decrypt_*_inner` test helpers) so tests can pin
/// rotation semantics without needing to capture `tracing` output.
/// Production code paths route this through a `tracing::warn!` and
/// drop the discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptOrigin {
    /// Decrypted with the current `APP_KEY`. Normal happy path.
    Current,
    /// Decrypted with a previous key in the ring — the `usize` is the
    /// zero-based index into `APP_KEY_PREVIOUS` (lower = older). A
    /// `tracing::warn!` was emitted; the operator should re-encrypt
    /// this column under the current key.
    Previous(usize),
}

/// Process-wide encryption facade.
///
/// Initialize once via [`Crypt::init`] (back-compat: single key) or
/// [`Crypt::init_with_keyring`] (rotation-aware: current + previous
/// list). The framework calls one of these on boot from `APP_KEY`
/// (+ optional `APP_KEY_PREVIOUS`). Use the static methods anywhere
/// afterwards.
///
/// # Wire format
///
/// `encrypt_string` and `encrypt` return URL-safe base64 (no padding)
/// over `nonce || ciphertext_with_tag`. Each call gets a fresh random
/// nonce. **The wire format does not carry the purpose / AAD label** —
/// callers supply the same [`CryptPurpose`] on encrypt and decrypt by
/// independent agreement; the GCM authentication tag detects a
/// mismatch.
///
/// # Purpose-bound AAD
///
/// Every encrypt / decrypt method takes a [`CryptPurpose`] that is
/// bound into the AES-GCM authentication tag as associated data. This
/// gives each surface (cookie, cursor, 2FA secret, etc.) its own
/// decrypt domain: ciphertext produced under one purpose fails to
/// decrypt under another, blocking cross-surface ciphertext replay
/// even when both surfaces accept the same plaintext shape.
pub struct Crypt;

impl Crypt {
    /// Install a single-key ring (no previous keys). Back-compat shim
    /// for callers that pre-date the rotation API.
    ///
    /// Subsequent calls are a no-op and emit a `tracing::warn!` — the
    /// ring is sealed for the lifetime of the process.
    pub fn init(key: EncryptionKey) {
        Self::init_with_keyring(key, Vec::new());
    }

    /// Install a rotation-aware key ring. `current` encrypts; `previous`
    /// are tried in order on decrypt fallback (older entries first by
    /// convention, but the ring tries them all so order only matters
    /// for which fallback fires first when more than one would work —
    /// extremely unlikely with random 256-bit keys).
    ///
    /// Subsequent calls are a no-op and emit a `tracing::warn!`.
    pub fn init_with_keyring(current: EncryptionKey, previous: Vec<EncryptionKey>) {
        if CRYPT_RING.set(KeyRing { current, previous }).is_err() {
            tracing::warn!("Crypt::init called more than once; ignoring");
        }
    }

    /// Whether a key has been installed.
    pub fn is_initialized() -> bool {
        CRYPT_RING.get().is_some()
    }

    fn ring() -> Result<&'static KeyRing, FrameworkError> {
        CRYPT_RING.get().ok_or_else(|| {
            FrameworkError::internal("Crypt is not initialized — set APP_KEY before serving")
        })
    }

    /// Encrypt a UTF-8 string under `purpose`. Returns base64-url-no-pad
    /// over `nonce || ciphertext_with_tag`. Always uses the current
    /// key.
    ///
    /// The `purpose` is bound as AEAD associated data — see
    /// [`CryptPurpose`]. The returned wire is rejected by any decrypt
    /// call that supplies a different purpose.
    pub fn encrypt_string(
        purpose: CryptPurpose,
        plaintext: &str,
    ) -> Result<String, FrameworkError> {
        let ring = Self::ring()?;
        let wire = aead::encrypt(&ring.current, purpose.aad(), plaintext.as_bytes())?;
        Ok(URL_SAFE_NO_PAD.encode(wire))
    }

    /// Decrypt a base64-url-no-pad payload previously produced by
    /// [`Self::encrypt_string`]. Tries the current key first, then each
    /// previous key. On a previous-key hit, emits a `tracing::warn!`.
    ///
    /// The `purpose` must match the value supplied at encrypt time, or
    /// the GCM authentication tag check fails — see [`CryptPurpose`].
    pub fn decrypt_string(purpose: CryptPurpose, wire: &str) -> Result<String, FrameworkError> {
        let (plain, origin) = Self::decrypt_string_inner(purpose, wire)?;
        Self::log_rotation_warning(origin);
        Ok(plain)
    }

    /// Encrypt any `Serialize` value by JSON-encoding then encrypting
    /// under `purpose`. Always uses the current key.
    ///
    /// The `purpose` is bound as AEAD associated data — see
    /// [`CryptPurpose`].
    pub fn encrypt<T: Serialize>(
        purpose: CryptPurpose,
        value: &T,
    ) -> Result<String, FrameworkError> {
        let ring = Self::ring()?;
        let json = serde_json::to_vec(value)
            .map_err(|e| FrameworkError::internal(format!("Crypt JSON encode failed: {e}")))?;
        let wire = aead::encrypt(&ring.current, purpose.aad(), &json)?;
        Ok(URL_SAFE_NO_PAD.encode(wire))
    }

    /// Decrypt and JSON-decode a payload previously produced by
    /// [`Self::encrypt`]. Tries the current key first, then each
    /// previous key. On a previous-key hit, emits a `tracing::warn!`.
    ///
    /// The `purpose` must match the value supplied at encrypt time.
    pub fn decrypt<T: DeserializeOwned>(
        purpose: CryptPurpose,
        wire: &str,
    ) -> Result<T, FrameworkError> {
        let (value, origin) = Self::decrypt_inner::<T>(purpose, wire)?;
        Self::log_rotation_warning(origin);
        Ok(value)
    }

    /// Heuristic check that `value` *looks like* a payload produced by
    /// [`Self::encrypt_string`] or [`Self::encrypt`] — URL-safe base64
    /// (no padding) over at least `nonce || tag` bytes.
    ///
    /// Mirrors Laravel's `Encrypter::appearsEncrypted`, which their
    /// `EncryptCookies` middleware uses to skip already-encrypted
    /// cookies on the egress pass. **This is not a tamper check** — it
    /// never calls into AES-GCM, so it cannot distinguish a valid
    /// ciphertext from random bytes of the right shape. Callers that
    /// need authentication must call [`Self::decrypt_string`] /
    /// [`Self::decrypt`] and handle the error.
    ///
    /// Returns `false` for any input that does not decode as URL-safe
    /// base64 or whose decoded length is shorter than `12 + 16` bytes
    /// (GCM nonce + tag), since such a payload can never be a valid
    /// AEAD ciphertext under this wire format.
    pub fn appears_encrypted(value: &str) -> bool {
        const MIN_AEAD: usize = 12 + 16; // nonce + GCM tag
        URL_SAFE_NO_PAD
            .decode(value.trim())
            .map(|bytes| bytes.len() >= MIN_AEAD)
            .unwrap_or(false)
    }

    /// Number of `APP_KEY_PREVIOUS` keys installed alongside the
    /// current one. Returns `0` if `Crypt` is uninitialized or no
    /// previous keys were supplied at boot.
    ///
    /// Mirrors the *cardinality* of Laravel's
    /// `Encrypter::getPreviousKeys()`. We deliberately do NOT expose
    /// the key bytes themselves — `EncryptionKey`'s `Debug` impl
    /// redacts (see `key.rs:67`) so the keyring stays opaque from
    /// every safe surface.
    pub fn previous_key_count() -> usize {
        CRYPT_RING.get().map(|r| r.previous.len()).unwrap_or(0)
    }

    /// Whether any `APP_KEY_PREVIOUS` entry is installed. Shorthand
    /// for `Self::previous_key_count() > 0`. Useful when an operator
    /// dashboard wants to surface "rotation in progress."
    pub fn has_previous_keys() -> bool {
        Self::previous_key_count() > 0
    }

    /// Return the raw 32 bytes of the currently-active encryption key,
    /// or `None` when `Crypt` is uninitialized. Internal hook for
    /// derive-key consumers within the framework — signed-URL HMAC,
    /// password-reset token HMAC, future SDK signing helpers.
    ///
    /// **Not exported.** The bytes are sensitive — callers must treat
    /// them as material for an HMAC or KDF, never log or expose them.
    /// `EncryptionKey` keeps its `Debug` redacted; this accessor does
    /// the same by returning a `Vec<u8>` that holds no `Debug` trace
    /// of its contents (`Vec<u8>: Debug` prints the bytes, but no
    /// `Vec<u8>` produced here is ever Debug-printed inside the crate).
    pub(crate) fn current_key_bytes() -> Option<Vec<u8>> {
        CRYPT_RING.get().map(|r| r.current.as_bytes().to_vec())
    }

    /// Test-and-internal hook: decrypt a string under `purpose` AND
    /// report which key in the ring succeeded. Exposed at `pub(crate)`
    /// so tests in the same crate (and the macro-generated
    /// `From<inner::Model>` could, in principle, surface origin to
    /// operators per-column without going through `tracing`) can pin
    /// rotation behaviour without wrestling with `tracing::Subscriber`
    /// capture.
    ///
    /// Public surface: [`Crypt::decrypt_string`].
    #[doc(hidden)]
    pub fn decrypt_string_inner(
        purpose: CryptPurpose,
        wire: &str,
    ) -> Result<(String, DecryptOrigin), FrameworkError> {
        let ring = Self::ring()?;
        let bytes = URL_SAFE_NO_PAD
            .decode(wire.trim())
            .map_err(|e| FrameworkError::internal(format!("Crypt base64 decode failed: {e}")))?;
        let (plain_bytes, origin) = decrypt_with_ring(ring, purpose.aad(), &bytes)?;
        let plain = String::from_utf8(plain_bytes).map_err(|e| {
            FrameworkError::internal(format!("Crypt decrypted bytes not UTF-8: {e}"))
        })?;
        Ok((plain, origin))
    }

    /// Test-and-internal hook: decrypt a JSON-encoded value under
    /// `purpose` AND report which key in the ring succeeded. See
    /// [`Self::decrypt_string_inner`].
    #[doc(hidden)]
    pub fn decrypt_inner<T: DeserializeOwned>(
        purpose: CryptPurpose,
        wire: &str,
    ) -> Result<(T, DecryptOrigin), FrameworkError> {
        let ring = Self::ring()?;
        let bytes = URL_SAFE_NO_PAD
            .decode(wire.trim())
            .map_err(|e| FrameworkError::internal(format!("Crypt base64 decode failed: {e}")))?;
        let (plain_bytes, origin) = decrypt_with_ring(ring, purpose.aad(), &bytes)?;
        let value: T = serde_json::from_slice(&plain_bytes)
            .map_err(|e| FrameworkError::internal(format!("Crypt JSON decode failed: {e}")))?;
        Ok((value, origin))
    }

    fn log_rotation_warning(origin: DecryptOrigin) {
        if let DecryptOrigin::Previous(index) = origin {
            // The log payload deliberately does NOT carry the
            // plaintext or the ciphertext — both are sensitive. We log
            // the fact + an actionable hint so an operator running a
            // log search for "APP_KEY_PREVIOUS" lands on every value
            // that still depends on an old key.
            tracing::warn!(
                previous_index = index,
                "Crypt decrypted a value with APP_KEY_PREVIOUS[{index}]; re-encrypt \
                 (load + save) this row under the current APP_KEY and remove the \
                 corresponding APP_KEY_PREVIOUS entry once the rotation completes."
            );
        }
    }
}

/// Trial-decrypt `wire` against every key in `ring` with `aad` bound
/// into the GCM tag check. Current first, then each previous in order.
/// Returns `(plain_bytes, origin)` on the first success; if every key
/// fails returns the error from the *current* key (the most likely
/// useful diagnostic — a previous-key failure would always be "wrong
/// key" since previous keys typically can't decrypt new data).
fn decrypt_with_ring(
    ring: &KeyRing,
    aad: &[u8],
    wire: &[u8],
) -> Result<(Vec<u8>, DecryptOrigin), FrameworkError> {
    match aead::decrypt(&ring.current, aad, wire) {
        Ok(plain) => Ok((plain, DecryptOrigin::Current)),
        Err(current_err) => {
            for (index, prev) in ring.previous.iter().enumerate() {
                if let Ok(plain) = aead::decrypt(prev, aad, wire) {
                    return Ok((plain, DecryptOrigin::Previous(index)));
                }
            }
            // All keys failed. Surface the current-key error since
            // that's the one operators care about — the previous keys
            // are best-effort fallbacks.
            Err(current_err)
        }
    }
}

/// Boot-time policy decision: given the runtime environment and the raw
/// value of `APP_KEY` (`None` if unset, `Some("")` if set-but-empty —
/// callers may pass either), decide which [`EncryptionKey`] to install.
///
/// This is the legacy single-key resolver. New callers should prefer
/// [`resolve_boot_keyring`], which also threads `APP_KEY_PREVIOUS`.
/// `resolve_boot_keyring` is implemented in terms of this function for
/// the current-key half so the two paths stay aligned.
///
/// Production fails closed: missing or empty `APP_KEY` is an `Err`
/// with an actionable message. Local/development/testing fall back to
/// a freshly-generated transient key.
///
/// # Errors
///
/// - Production / Staging / Custom env without a valid key
/// - Any environment with a malformed `APP_KEY` (wrong length, bad
///   base64) — bad keys never fall through to a generated dev key
///   because that would silently mask a misconfigured production
///   deployment.
pub fn resolve_boot_key(
    environment: &Environment,
    app_key: Option<&str>,
) -> Result<BootKey, FrameworkError> {
    // Treat empty string the same as unset — both mean "no key
    // configured." Strips trailing whitespace too so a `APP_KEY=` line
    // with a stray space doesn't accidentally parse.
    let supplied = app_key.map(str::trim).filter(|s| !s.is_empty());

    match (environment, supplied) {
        (_, Some(raw)) => {
            // Explicit key always wins. A malformed key is an error in
            // every environment — never fall back to a generated dev
            // key because that would mask a typo in production.
            let key = EncryptionKey::from_base64(raw).map_err(|e| {
                FrameworkError::internal(format!(
                    "APP_KEY is set but invalid: {e}. Expected 32 bytes \
                     encoded as URL-safe base64 (no padding). Run \
                     `suprnova key:generate` to mint a new one."
                ))
            })?;
            Ok(BootKey::Configured(key))
        }
        (Environment::Local | Environment::Development | Environment::Testing, None) => {
            // Dev environments still need a key for sessions and
            // cursors to work — we just don't require the operator
            // to set one up before `cargo run`. Generated transient
            // keys reset on every restart, which is a feature in
            // development (no stale-session weirdness) but the
            // caller should log a warn so the operator knows
            // sessions won't persist across boots.
            Ok(BootKey::GeneratedTransient(EncryptionKey::generate()))
        }
        (env, None) => Err(FrameworkError::internal(format!(
            "APP_KEY is required when APP_ENV={env}. Generate one with \
             `suprnova key:generate` and set it in your environment \
             (e.g. .env or your secrets manager). Suprnova refuses to \
             boot without an encryption key outside of local/development/\
             testing because session cookies and pagination cursors would \
             otherwise be unsigned and forgeable."
        ))),
    }
}

/// Boot-time keyring resolution. Threads both `APP_KEY` and
/// `APP_KEY_PREVIOUS` from the environment.
///
/// `app_key_previous` is the raw value of `APP_KEY_PREVIOUS` (a
/// comma-separated list of base64 keys for multi-step rotation), or
/// `None`/empty for the single-key path. Whitespace around each comma-
/// separated entry is trimmed; empty entries (e.g. trailing comma) are
/// skipped silently.
///
/// # Errors
///
/// - Any error that [`resolve_boot_key`] would return on the current
///   key.
/// - Any entry in `APP_KEY_PREVIOUS` is malformed — a half-rotated
///   secret should fail loudly at boot, not silently drop a fallback
///   key and leave columns undecryptable.
pub fn resolve_boot_keyring(
    environment: &Environment,
    app_key: Option<&str>,
    app_key_previous: Option<&str>,
) -> Result<BootKeyRing, FrameworkError> {
    let current = resolve_boot_key(environment, app_key)?;
    let previous = parse_previous_keys(app_key_previous)?;
    Ok(BootKeyRing { current, previous })
}

/// Parse `APP_KEY_PREVIOUS` (or any comma-separated list of base64
/// keys) into a vector of [`EncryptionKey`]. Empty / whitespace
/// entries are skipped; the empty-string-overall case yields `Vec::new()`.
///
/// Any malformed entry is an error — see [`resolve_boot_keyring`].
fn parse_previous_keys(raw: Option<&str>) -> Result<Vec<EncryptionKey>, FrameworkError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (i, entry) in trimmed.split(',').enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            // Tolerate `APP_KEY_PREVIOUS=a,,b` and trailing commas —
            // the operator may use a templated config that leaves an
            // empty slot during a partial rotation.
            continue;
        }
        let key = EncryptionKey::from_base64(entry).map_err(|e| {
            FrameworkError::internal(format!(
                "APP_KEY_PREVIOUS entry #{i} is invalid: {e}. Expected a \
                 comma-separated list of 32-byte keys encoded as URL-safe \
                 base64 (no padding). To rotate without re-encrypting \
                 existing data, list the old key(s) here. Run `suprnova \
                 key:generate` to mint replacements."
            ))
        })?;
        out.push(key);
    }
    Ok(out)
}

/// Result of [`resolve_boot_key`]. The caller installs the inner key
/// via [`Crypt::init`]; the discriminator is preserved so the boot
/// path can emit the right log message (a generated dev key needs a
/// loud warn that the operator may want to persist it).
#[derive(Debug)]
pub enum BootKey {
    /// Operator supplied a valid `APP_KEY` in the environment.
    Configured(EncryptionKey),
    /// No `APP_KEY` set and the environment permits a transient dev
    /// key. The boot path generated a fresh random key on the spot —
    /// it will not survive a restart.
    GeneratedTransient(EncryptionKey),
}

impl BootKey {
    pub fn into_key(self) -> EncryptionKey {
        match self {
            BootKey::Configured(k) | BootKey::GeneratedTransient(k) => k,
        }
    }

    pub fn is_generated(&self) -> bool {
        matches!(self, BootKey::GeneratedTransient(_))
    }
}

/// Result of [`resolve_boot_keyring`]. Wraps the current key (as a
/// [`BootKey`] so the dev/transient warn behaviour is preserved) plus
/// the parsed `APP_KEY_PREVIOUS` list. The caller installs the inner
/// ring via [`Crypt::init_with_keyring`].
#[derive(Debug)]
pub struct BootKeyRing {
    /// The current key — used for encrypt + decrypted-first.
    pub current: BootKey,
    /// Previous keys in declared order. Empty if `APP_KEY_PREVIOUS`
    /// is unset or contains only whitespace / empty entries.
    pub previous: Vec<EncryptionKey>,
}

impl BootKeyRing {
    /// `true` iff the current key was generated transiently (dev env,
    /// no `APP_KEY` supplied). Mirrors [`BootKey::is_generated`] for
    /// callers that want to log the dev-key warn.
    pub fn is_current_generated(&self) -> bool {
        self.current.is_generated()
    }

    /// Consume the ring into `(current_key, previous_keys)`. Used by
    /// the boot path immediately before [`Crypt::init_with_keyring`].
    pub fn into_keys(self) -> (EncryptionKey, Vec<EncryptionKey>) {
        (self.current.into_key(), self.previous)
    }
}

// ----------------------------------------------------------------------
// Test-only key installation hooks.
//
// HIGH audit #334: these helpers used to be plain `pub fn` items guarded
// only by `#[doc(hidden)]`, which is not a real boundary — `doc(hidden)`
// hides them from rustdoc but does NOT remove them from the binary or
// from `use suprnova::crypto::*` access. A consumer (or attacker) could
// install a key before `Server::from_config`, bypassing the APP_KEY
// validation that the server runs only when `Crypt` is uninitialized.
//
// They are now gated behind `cfg(any(test, feature = "testing"))` —
// when a downstream consumer disables `default-features`, these
// functions vanish from the binary entirely. The complementary fix in
// `Server::from_config` (always run APP_KEY validation, not just when
// `Crypt` is uninitialized) closes the same exposure from a different
// angle so the hard boundary holds even if a future consumer
// re-enables the feature.
// ----------------------------------------------------------------------

/// Test-only helper: install a single key without going through
/// `OnceLock::set` for the second-and-later test in a suite. Returns
/// `true` if the key was actually installed, `false` if a ring was
/// already present.
///
/// Tests must serialize themselves via a `Mutex<()>` because the global
/// `CRYPT_RING` is shared.
///
/// **Test-only — do not call from production code.** Compiled out when
/// the `testing` feature is disabled.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub fn _test_install_key(key: EncryptionKey) -> bool {
    CRYPT_RING
        .set(KeyRing {
            current: key,
            previous: Vec::new(),
        })
        .is_ok()
}

/// Test-only helper: install a key ring (current + previous list)
/// directly. Same semantics as [`_test_install_key`] but exposes the
/// rotation surface needed by
/// `framework/tests/eloquent_casts_encrypted_key_rotation.rs`.
///
/// Returns `true` if the ring was actually installed, `false` if a
/// ring was already present.
///
/// **Test-only — do not call from production code.** Compiled out when
/// the `testing` feature is disabled.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub fn _test_install_keyring(current: EncryptionKey, previous: Vec<EncryptionKey>) -> bool {
    CRYPT_RING.set(KeyRing { current, previous }).is_ok()
}

/// Test-only helper: encrypt `plaintext` under an *arbitrary* key and
/// `purpose` (bypassing the installed ring). Used to mint ciphertext
/// under a key that isn't the current `APP_KEY` so rotation tests can
/// simulate "this column was written when the old key was current."
///
/// `purpose` must match the AAD the eventual decrypt path will supply
/// — e.g. to simulate a row written by an `AsEncrypted` cast, pass
/// [`CryptPurpose::Cast`]; the cast's `from_storage` decrypt will then
/// authenticate the wire under the same AAD.
///
/// Calls `aead::encrypt` directly and applies the same base64 wire
/// format as [`Crypt::encrypt_string`]. The returned string is byte-
/// for-byte indistinguishable from a normal `Crypt::encrypt_string`
/// output produced under `key` for the same `purpose`.
///
/// **Test-only — do not call from production code.** Compiled out when
/// the `testing` feature is disabled.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub fn _test_encrypt_with(
    key: &EncryptionKey,
    purpose: CryptPurpose,
    plaintext: &str,
) -> Result<String, FrameworkError> {
    let wire = aead::encrypt(key, purpose.aad(), plaintext.as_bytes())?;
    Ok(URL_SAFE_NO_PAD.encode(wire))
}

#[cfg(test)]
mod boot_tests {
    //! Tests for [`resolve_boot_key`] and [`resolve_boot_keyring`].
    //! These do NOT touch the global `CRYPT_RING` `OnceLock` — they
    //! exercise the pure decision functions. End-to-end Crypt
    //! installation is covered by
    //! `framework/tests/app_key_enforcement.rs` (one scenario per
    //! test binary because `OnceLock` is process-wide).

    use super::*;

    #[test]
    fn production_without_key_fails_closed() {
        let err = resolve_boot_key(&Environment::Production, None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("APP_KEY is required"),
            "expected actionable message, got: {msg}"
        );
        assert!(
            msg.contains("suprnova key:generate"),
            "message should point at the CLI helper, got: {msg}"
        );
    }

    #[test]
    fn production_with_empty_key_fails_closed() {
        // Empty string and whitespace-only count as "unset" — the
        // operator likely has `APP_KEY=` with nothing after the equals.
        assert!(resolve_boot_key(&Environment::Production, Some("")).is_err());
        assert!(resolve_boot_key(&Environment::Production, Some("   ")).is_err());
    }

    #[test]
    fn staging_without_key_fails_closed() {
        assert!(resolve_boot_key(&Environment::Staging, None).is_err());
    }

    #[test]
    fn custom_env_without_key_fails_closed() {
        // Unknown environments are treated production-like — anything
        // we don't explicitly recognize as a dev environment must not
        // silently downgrade.
        assert!(resolve_boot_key(&Environment::Custom("k8s".into()), None).is_err());
    }

    #[test]
    fn production_with_valid_key_succeeds() {
        let key = EncryptionKey::generate().to_base64();
        let resolved = resolve_boot_key(&Environment::Production, Some(&key)).unwrap();
        assert!(!resolved.is_generated());
    }

    #[test]
    fn production_with_malformed_key_errors_even_with_value() {
        // A bad key in production must error — never fall back to a
        // generated key, because that would mask a typo or a
        // half-rotated secret.
        let err =
            resolve_boot_key(&Environment::Production, Some("not-valid-base64!!!")).unwrap_err();
        assert!(format!("{err}").contains("APP_KEY is set but invalid"));
    }

    #[test]
    fn dev_env_without_key_generates_transient() {
        for env in [
            Environment::Local,
            Environment::Development,
            Environment::Testing,
        ] {
            let resolved = resolve_boot_key(&env, None).unwrap();
            assert!(
                resolved.is_generated(),
                "expected generated transient key for {env}, got Configured"
            );
        }
    }

    #[test]
    fn dev_env_with_explicit_key_uses_it() {
        // Even in local, if the operator supplies a key we use it
        // (sessions persist across restarts).
        let key = EncryptionKey::generate().to_base64();
        let resolved = resolve_boot_key(&Environment::Local, Some(&key)).unwrap();
        assert!(!resolved.is_generated());
    }

    #[test]
    fn dev_env_with_malformed_key_still_errors() {
        // Even in local, an explicit-but-bad key is an error — better
        // to fail at boot than silently mask a typo.
        let err = resolve_boot_key(&Environment::Local, Some("not-valid-base64!!!")).unwrap_err();
        assert!(format!("{err}").contains("APP_KEY is set but invalid"));
    }

    // ---- Keyring (rotation) coverage ------------------------------------

    #[test]
    fn keyring_with_no_previous_is_empty_vec() {
        let app_key = EncryptionKey::generate().to_base64();
        let ring = resolve_boot_keyring(&Environment::Production, Some(&app_key), None)
            .expect("no previous → ok");
        assert!(ring.previous.is_empty());
    }

    #[test]
    fn keyring_with_empty_previous_string_is_empty_vec() {
        // `APP_KEY_PREVIOUS=` and `APP_KEY_PREVIOUS=   ` both mean
        // "no previous keys" — same way `APP_KEY=` means unset.
        let app_key = EncryptionKey::generate().to_base64();
        for raw in ["", "   ", "  \t  "] {
            let ring = resolve_boot_keyring(&Environment::Production, Some(&app_key), Some(raw))
                .expect("empty previous → ok");
            assert!(ring.previous.is_empty(), "raw={raw:?}");
        }
    }

    #[test]
    fn keyring_parses_single_previous_key() {
        let app_key = EncryptionKey::generate().to_base64();
        let prev = EncryptionKey::generate().to_base64();
        let ring = resolve_boot_keyring(&Environment::Production, Some(&app_key), Some(&prev))
            .expect("single previous key parses");
        assert_eq!(ring.previous.len(), 1);
    }

    #[test]
    fn keyring_parses_multi_step_rotation() {
        // Operators chaining multiple rotations (e.g. quarterly key
        // rolls during a slow re-encrypt) supply
        // `APP_KEY_PREVIOUS=k_oldest,k_middle,k_newest`.
        let app_key = EncryptionKey::generate().to_base64();
        let k1 = EncryptionKey::generate().to_base64();
        let k2 = EncryptionKey::generate().to_base64();
        let k3 = EncryptionKey::generate().to_base64();
        let combined = format!("{k1},{k2},{k3}");
        let ring = resolve_boot_keyring(&Environment::Production, Some(&app_key), Some(&combined))
            .expect("3 previous keys parse");
        assert_eq!(ring.previous.len(), 3);
    }

    #[test]
    fn keyring_skips_empty_entries_in_list() {
        // Templated config files sometimes leave gaps like
        // `APP_KEY_PREVIOUS=a,,b` during a partial rotation. The
        // empty entries are tolerated as "no key in this slot" — not
        // an error.
        let app_key = EncryptionKey::generate().to_base64();
        let k1 = EncryptionKey::generate().to_base64();
        let k2 = EncryptionKey::generate().to_base64();
        let combined = format!("{k1},,{k2},");
        let ring = resolve_boot_keyring(&Environment::Production, Some(&app_key), Some(&combined))
            .expect("empty entries are tolerated");
        assert_eq!(ring.previous.len(), 2);
    }

    #[test]
    fn keyring_errors_on_malformed_previous_entry() {
        // Half-rotated secret: typo in one previous key entry. Must
        // fail at boot, not silently drop the fallback (which would
        // leave columns undecryptable with no diagnostic).
        let app_key = EncryptionKey::generate().to_base64();
        let good = EncryptionKey::generate().to_base64();
        let combined = format!("{good},not-valid-base64!!!");
        let err = resolve_boot_keyring(&Environment::Production, Some(&app_key), Some(&combined))
            .expect_err("malformed previous entry must fail boot");
        let msg = format!("{err}");
        assert!(
            msg.contains("APP_KEY_PREVIOUS entry #1 is invalid"),
            "expected entry-specific diagnostic, got: {msg}"
        );
    }

    #[test]
    fn keyring_propagates_missing_app_key_error_in_production() {
        // The keyring resolver delegates current-key validation to
        // `resolve_boot_key`. Production without APP_KEY must still
        // fail closed even if APP_KEY_PREVIOUS is set — that's a
        // common misconfiguration during a botched rotation.
        let prev = EncryptionKey::generate().to_base64();
        let err = resolve_boot_keyring(&Environment::Production, None, Some(&prev))
            .expect_err("no current key in prod must fail closed");
        assert!(format!("{err}").contains("APP_KEY is required"));
    }

    #[test]
    fn keyring_dev_with_no_app_key_generates_transient_current() {
        // Dev environments with only APP_KEY_PREVIOUS set still get
        // a generated transient current key — the dev workflow
        // shouldn't break just because the operator left
        // APP_KEY_PREVIOUS pointing at the last production key.
        let prev = EncryptionKey::generate().to_base64();
        let ring = resolve_boot_keyring(&Environment::Local, None, Some(&prev))
            .expect("local with previous-only succeeds");
        assert!(ring.is_current_generated());
        assert_eq!(ring.previous.len(), 1);
    }

    // ---- Trial-decrypt loop coverage -----------------------------------

    /// AAD used by the trial-decrypt loop tests. Any stable label
    /// works; the rotation walk is independent of the AAD value as long
    /// as encrypt and decrypt agree.
    const TEST_AAD: &[u8] = b"suprnova:test-ring:v1";

    #[test]
    fn decrypt_with_ring_uses_current_first() {
        let current = EncryptionKey::generate();
        let prev = EncryptionKey::generate();
        let ring = KeyRing {
            current: current.clone(),
            previous: vec![prev],
        };
        let wire = aead::encrypt(&current, TEST_AAD, b"hello").unwrap();
        let (plain, origin) = decrypt_with_ring(&ring, TEST_AAD, &wire).unwrap();
        assert_eq!(plain, b"hello");
        assert_eq!(origin, DecryptOrigin::Current);
    }

    #[test]
    fn decrypt_with_ring_falls_back_to_previous() {
        let current = EncryptionKey::generate();
        let prev = EncryptionKey::generate();
        // Encrypt under the OLD key, then verify the new ring decrypts.
        let wire = aead::encrypt(&prev, TEST_AAD, b"legacy-payload").unwrap();
        let ring = KeyRing {
            current,
            previous: vec![prev],
        };
        let (plain, origin) = decrypt_with_ring(&ring, TEST_AAD, &wire).unwrap();
        assert_eq!(plain, b"legacy-payload");
        assert_eq!(origin, DecryptOrigin::Previous(0));
    }

    #[test]
    fn decrypt_with_ring_walks_full_previous_list() {
        // Multi-step rotation: ciphertext was encrypted under the
        // oldest key, two newer keys have since been retired, and a
        // fourth is now current. Ring must walk all three previous
        // entries to find the match.
        let current = EncryptionKey::generate();
        let middle = EncryptionKey::generate();
        let middle_2 = EncryptionKey::generate();
        let oldest = EncryptionKey::generate();
        let wire = aead::encrypt(&oldest, TEST_AAD, b"ancient-payload").unwrap();
        let ring = KeyRing {
            current,
            previous: vec![oldest, middle, middle_2],
        };
        let (plain, origin) = decrypt_with_ring(&ring, TEST_AAD, &wire).unwrap();
        assert_eq!(plain, b"ancient-payload");
        assert_eq!(origin, DecryptOrigin::Previous(0));
    }

    #[test]
    fn decrypt_with_ring_errors_when_no_key_matches() {
        let current = EncryptionKey::generate();
        let prev = EncryptionKey::generate();
        let unrelated = EncryptionKey::generate();
        let wire = aead::encrypt(&unrelated, TEST_AAD, b"unreachable").unwrap();
        let ring = KeyRing {
            current,
            previous: vec![prev],
        };
        let err = decrypt_with_ring(&ring, TEST_AAD, &wire).unwrap_err();
        // The surfaced error is whatever `aead::decrypt` returned
        // for the current key (most useful diagnostic for the
        // operator — a previous-key fail is expected for new data).
        assert!(format!("{err}").contains("AEAD decrypt failed"));
    }

    #[test]
    fn decrypt_with_ring_rejects_mismatched_aad() {
        // Domain separation through the ring: a wire encrypted with
        // AAD_A must NOT decrypt under AAD_B even when the key matches.
        // This is the property that blocks cross-surface ciphertext
        // replay through the whole facade, not just the raw `aead`
        // module.
        let current = EncryptionKey::generate();
        let ring = KeyRing {
            current: current.clone(),
            previous: vec![],
        };
        let aad_a = b"suprnova:purpose-a:v1";
        let aad_b = b"suprnova:purpose-b:v1";
        let wire = aead::encrypt(&current, aad_a, b"crosswire").unwrap();
        assert!(decrypt_with_ring(&ring, aad_b, &wire).is_err());
        // Sanity: same AAD still decrypts via the ring.
        let (plain, _origin) = decrypt_with_ring(&ring, aad_a, &wire).unwrap();
        assert_eq!(plain, b"crosswire");
    }
}
