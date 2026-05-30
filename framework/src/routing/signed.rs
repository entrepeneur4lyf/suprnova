//! Signed URL generation and verification.
//!
//! Laravel's `URL::signedRoute()` / `URL::temporarySignedRoute()` /
//! `URL::hasValidSignature()` family in `Illuminate/Routing/UrlGenerator.php`.
//! Suprnova's port lives here so the routing module owns the full
//! `route(name) → URL` surface end to end.
//!
//! ## Wire format
//!
//! Given a generated URL `/path?foo=1&bar=2` (after route-name substitution +
//! per-segment percent-encoding from [`crate::routing::route`]) and optional
//! expiration `expires_at` (epoch seconds):
//!
//! 1. Append `expires` if present: `?foo=1&bar=2&expires=1748800000`
//! 2. Sort query pairs lexicographically by key so equivalent URLs hash
//!    identically regardless of caller insertion order.
//! 3. Build the canonical string `path?<sorted_kv>` (omit the `?` when no
//!    pairs exist).
//! 4. HMAC-SHA256 with the framework's APP_KEY; hex-encode the result.
//! 5. Append `&signature=<hex>` (or `?signature=<hex>` if no other params).
//!
//! Verification reverses the build: strip `signature`, recompute the HMAC over
//! the canonical form, and compare in constant time. Expired signatures
//! verify cleanly but report `expired` separately so callers can render a
//! refresh flow.
//!
//! ## Why HMAC over the path + sorted query
//!
//! - **Path** binds the URL to its route — switching `/orders/1` to
//!   `/orders/2` invalidates the signature even when query parameters match.
//! - **Sorted query** prevents trivial reorderings from producing different
//!   signatures for the same effective URL (matching Laravel's
//!   `ksort($queryString)` policy).
//! - **`expires` inside the signed payload** binds the expiration to the
//!   signature itself — a client cannot strip or extend the expiration
//!   without invalidating the HMAC.
//! - **HMAC-SHA256, hex** matches Laravel's wire format (32-byte digest,
//!   64-char hex) so signed URLs minted by either side remain
//!   interchangeable when keys match.
//!
//! ## Key source
//!
//! [`signed_url_key`] resolves the signing key from the framework's
//! [`Crypt`][crate::crypto::Crypt] keyring. Laravel uses `APP_KEY` for both
//! encryption and URL signing; Suprnova does the same so users get one
//! rotation story.

use crate::FrameworkError;
use crate::crypto::Crypt;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::BTreeMap;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Reserved query-parameter name for the signature value. Reserved
/// because we strip it on verification; a route that legitimately
/// expects a `signature` query param would collide.
pub const SIGNATURE_KEY: &str = "signature";

/// Reserved query-parameter name for the expiration timestamp (epoch
/// seconds). Same reservation rule as [`SIGNATURE_KEY`].
pub const EXPIRES_KEY: &str = "expires";

/// Outcome of [`verify_signature`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureVerdict {
    /// Signature is valid and (if present) not yet expired.
    Valid,
    /// Signature is structurally well-formed and matches the recomputed
    /// HMAC, but the `expires` timestamp is in the past.
    Expired,
    /// Signature is missing, malformed, or does not match the recomputed
    /// HMAC. Treat as untrusted — do not trust the embedded `expires`
    /// value either.
    Invalid,
}

impl SignatureVerdict {
    /// `true` when the URL is safe to act on. Equivalent to
    /// `matches!(self, SignatureVerdict::Valid)`.
    pub fn is_valid(self) -> bool {
        matches!(self, SignatureVerdict::Valid)
    }

    /// `true` when the signature was correct but the URL has expired.
    /// Useful for rendering "request a fresh link" UX.
    pub fn is_expired(self) -> bool {
        matches!(self, SignatureVerdict::Expired)
    }
}

/// Resolve the signing key for URL signatures.
///
/// Returns the active encryption key's raw 32 bytes. Falls back to a
/// `FrameworkError` if no key is installed — signed URLs are a
/// trust-boundary feature and silently signing with a missing key would
/// produce unverifiable links. The caller (route helpers, middleware)
/// should treat the error as a 500-equivalent boot misconfiguration.
fn signed_url_key() -> Result<Vec<u8>, FrameworkError> {
    if !Crypt::is_initialized() {
        return Err(FrameworkError::internal(
            "Cannot sign URLs: encryption key not installed. \
             Boot the framework via `Server::from_config(...)` so APP_KEY \
             is loaded before signed-URL helpers run.",
        ));
    }
    Crypt::current_key_bytes().ok_or_else(|| {
        FrameworkError::internal("Cannot sign URLs: active encryption key unavailable")
    })
}

/// Compute the HMAC-SHA256 over the canonical payload bytes and return
/// the hex-encoded digest. Pure function — no global state.
fn hmac_hex(key: &[u8], payload: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(key).expect("HMAC accepts any key length — input is fine");
    mac.update(payload);
    hex::encode(mac.finalize().into_bytes())
}

/// Decompose `url` into `(path, query_pairs)` where the path is everything
/// up to the first `?`. Fragment handling: a `#fragment` is dropped from the
/// canonical form because browsers never transmit it back to the server, so
/// signing over it would invalidate every link the moment a client adds an
/// anchor.
fn split_url(url: &str) -> (String, Vec<(String, String)>) {
    // Strip fragment first.
    let url = match url.find('#') {
        Some(i) => &url[..i],
        None => url,
    };
    match url.find('?') {
        Some(i) => {
            let path = url[..i].to_string();
            let pairs: Vec<(String, String)> =
                url::form_urlencoded::parse(&url.as_bytes()[i + 1..])
                    .into_owned()
                    .collect();
            (path, pairs)
        }
        None => (url.to_string(), Vec::new()),
    }
}

/// Reassemble `path` + sorted query pairs back into a canonical URL string.
fn canonicalize(path: &str, pairs: &BTreeMap<String, String>) -> String {
    if pairs.is_empty() {
        return path.to_string();
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        serializer.append_pair(k, v);
    }
    let mut out = String::with_capacity(path.len() + 64);
    out.push_str(path);
    out.push('?');
    out.push_str(&serializer.finish());
    out
}

/// Sign a URL with the framework signing key.
///
/// Returns the URL with a `signature` (and optional `expires`) query
/// parameter appended. The input may already contain query parameters;
/// they're preserved alphabetically alongside any new ones.
///
/// `expires_at_epoch_seconds = Some(ts)` produces a temporary signed URL;
/// `None` produces a permanent signed URL.
///
/// # Errors
///
/// Returns `FrameworkError` when the encryption key is not installed
/// (see [`signed_url_key`]).
pub fn sign_url(
    url: &str,
    expires_at_epoch_seconds: Option<i64>,
) -> Result<String, FrameworkError> {
    let key = signed_url_key()?;
    let (path, mut pairs) = split_url(url);

    // Strip any pre-existing `signature` so we never sign-over-sign;
    // strip pre-existing `expires` so the caller's argument wins.
    pairs.retain(|(k, _)| k != SIGNATURE_KEY && k != EXPIRES_KEY);

    if let Some(ts) = expires_at_epoch_seconds {
        pairs.push((EXPIRES_KEY.to_string(), ts.to_string()));
    }

    // Sort by key for canonical form.
    let sorted: BTreeMap<String, String> = pairs.into_iter().collect();
    let canonical = canonicalize(&path, &sorted);

    let signature = hmac_hex(&key, canonical.as_bytes());

    // Append signature OUTSIDE the canonicalised payload — verifiers
    // recompute over everything except `signature`, so position is
    // semantically irrelevant; we append last for human readability.
    let mut out = canonical;
    if sorted.is_empty() {
        out.push('?');
    } else {
        out.push('&');
    }
    out.push_str(SIGNATURE_KEY);
    out.push('=');
    out.push_str(&signature);
    Ok(out)
}

/// Verify a signed URL.
///
/// Reverses [`sign_url`]: strip the `signature` query parameter, recompute
/// the HMAC over the canonical form, and compare in constant time.
///
/// Behaviour:
/// - Returns [`SignatureVerdict::Invalid`] when `signature` is missing,
///   malformed (non-hex, wrong length), or does not match the recomputed
///   HMAC.
/// - Returns [`SignatureVerdict::Expired`] when the HMAC is valid but the
///   embedded `expires` value is in the past relative to `now_epoch_seconds`.
/// - Returns [`SignatureVerdict::Valid`] otherwise.
///
/// Pass `now_epoch_seconds` so the caller controls the clock (testability +
/// monotonic-test parity with Laravel's `Carbon::now()->getTimestamp()` in
/// `UrlGenerator::signatureHasNotExpired`).
///
/// # Errors
///
/// Returns `FrameworkError` when the encryption key is not installed.
pub fn verify_signature(
    url: &str,
    now_epoch_seconds: i64,
) -> Result<SignatureVerdict, FrameworkError> {
    let key = signed_url_key()?;
    let (path, pairs) = split_url(url);

    // Extract the candidate signature and the expires value.
    let mut sig: Option<String> = None;
    let mut expires: Option<i64> = None;
    let mut rest: Vec<(String, String)> = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        if k == SIGNATURE_KEY {
            sig = Some(v);
        } else {
            if k == EXPIRES_KEY {
                expires = v.parse::<i64>().ok();
            }
            rest.push((k, v));
        }
    }
    let Some(sig) = sig else {
        return Ok(SignatureVerdict::Invalid);
    };

    // Canonical recomputation.
    let sorted: BTreeMap<String, String> = rest.into_iter().collect();
    let canonical = canonicalize(&path, &sorted);
    let expected = hmac_hex(&key, canonical.as_bytes());

    // Constant-time compare. Length mismatch fails closed in the
    // `ConstantTimeEq` impl, but we short-circuit for hex-encoding
    // sanity (malformed `signature` should never near `ct_eq`).
    if sig.len() != expected.len() {
        return Ok(SignatureVerdict::Invalid);
    }
    let valid: bool = sig.as_bytes().ct_eq(expected.as_bytes()).into();
    if !valid {
        return Ok(SignatureVerdict::Invalid);
    }

    if let Some(ts) = expires
        && now_epoch_seconds > ts
    {
        return Ok(SignatureVerdict::Expired);
    }
    Ok(SignatureVerdict::Valid)
}

/// Convenience: sign a named route lookup.
///
/// Looks `name` up via [`crate::routing::route`], applies the optional
/// expiration, and signs the result. Fails with `FrameworkError` when the
/// route name is not registered or the encryption key is missing.
pub fn sign_route(
    name: &str,
    params: &[(&str, &str)],
    expires_at_epoch_seconds: Option<i64>,
) -> Result<String, FrameworkError> {
    let url = crate::routing::route(name, params).ok_or_else(|| {
        FrameworkError::internal(format!(
            "Cannot sign route '{name}': name is not registered. \
             Register via `.name(\"{name}\")` or `routes!{{}}`.",
        ))
    })?;
    sign_url(&url, expires_at_epoch_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{Crypt, EncryptionKey};

    fn ensure_key() {
        if !Crypt::is_initialized() {
            Crypt::init(EncryptionKey::generate());
        }
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn sign_then_verify_round_trips() {
        ensure_key();
        let url = "/orders/42?foo=1&bar=2";
        let signed = sign_url(url, None).expect("sign");
        assert!(signed.contains("signature="));
        let verdict = verify_signature(&signed, 0).expect("verify");
        assert_eq!(verdict, SignatureVerdict::Valid);
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn sign_is_order_independent_over_query_params() {
        ensure_key();
        let a = sign_url("/x?b=2&a=1", None).expect("sign a");
        let b = sign_url("/x?a=1&b=2", None).expect("sign b");
        // Canonical form is keyed by sort order, so the signature must match.
        let sig_a = a.rsplit("signature=").next().unwrap();
        let sig_b = b.rsplit("signature=").next().unwrap();
        assert_eq!(sig_a, sig_b, "param order must not change the signature");
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn tampered_path_fails_verification() {
        ensure_key();
        let signed = sign_url("/orders/42", None).expect("sign");
        let tampered = signed.replace("/orders/42", "/orders/43");
        assert_eq!(
            verify_signature(&tampered, 0).unwrap(),
            SignatureVerdict::Invalid,
            "tampered path must not validate",
        );
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn tampered_query_fails_verification() {
        ensure_key();
        let signed = sign_url("/x?u=alice", None).expect("sign");
        let tampered = signed.replace("u=alice", "u=eve");
        assert_eq!(
            verify_signature(&tampered, 0).unwrap(),
            SignatureVerdict::Invalid,
        );
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn expired_signature_reports_expired_not_invalid() {
        ensure_key();
        let signed = sign_url("/reset", Some(1000)).expect("sign");
        let verdict = verify_signature(&signed, 2000).expect("verify");
        assert_eq!(verdict, SignatureVerdict::Expired);
        assert!(verdict.is_expired());
        assert!(!verdict.is_valid());
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn unexpired_signature_validates() {
        ensure_key();
        let signed = sign_url("/reset", Some(5000)).expect("sign");
        let verdict = verify_signature(&signed, 1000).expect("verify");
        assert_eq!(verdict, SignatureVerdict::Valid);
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn stripping_signature_fails_verification() {
        ensure_key();
        let signed = sign_url("/x", None).expect("sign");
        // Strip the signature query param entirely.
        let no_sig = signed.split("?signature=").next().unwrap().to_string();
        assert_eq!(
            verify_signature(&no_sig, 0).unwrap(),
            SignatureVerdict::Invalid,
            "missing signature must be Invalid (not Valid by accident)",
        );
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn fragment_is_stripped_from_canonical_form() {
        ensure_key();
        let with_frag = sign_url("/about#section", None).expect("sign");
        // The signature is computed over `/about`, so re-signing without
        // the fragment yields the same signature.
        let without_frag = sign_url("/about", None).expect("sign-again");
        let s1 = with_frag.rsplit("signature=").next().unwrap();
        let s2 = without_frag.rsplit("signature=").next().unwrap();
        assert_eq!(
            s1, s2,
            "fragment must not influence the signature — browsers don't echo it back",
        );
    }

    #[test]
    #[serial_test::serial(crypt_install, route_registry)]
    fn sign_route_resolves_named_route() {
        ensure_key();
        crate::routing::clear_route_names_for_test();
        crate::routing::register_route_name("signed.test.route", "/items/{id}");
        let signed = sign_route("signed.test.route", &[("id", "42")], None).expect("sign route");
        assert!(signed.starts_with("/items/42?signature="));
        assert_eq!(
            verify_signature(&signed, 0).unwrap(),
            SignatureVerdict::Valid,
        );
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn sign_route_errors_on_unknown_name() {
        ensure_key();
        let err = sign_route("signed.test.does_not_exist_xyz", &[], None).unwrap_err();
        assert!(
            err.to_string().contains("is not registered"),
            "error must explain the missing name; got {err}",
        );
    }

    #[test]
    #[serial_test::serial(crypt_install)]
    fn malformed_signature_hex_is_invalid_not_panic() {
        ensure_key();
        // Bare junk in the signature slot.
        let url = "/x?signature=not-hex-at-all";
        assert_eq!(verify_signature(url, 0).unwrap(), SignatureVerdict::Invalid,);
    }
}
