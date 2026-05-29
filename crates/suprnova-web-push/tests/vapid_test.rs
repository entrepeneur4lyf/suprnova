use suprnova_web_push::{VapidKey, VapidSigner};

#[test]
fn vapid_key_generates_p256_keypair() {
    let key = VapidKey::generate();
    let pub_b64 = key.public_key_uncompressed_b64url();
    assert_eq!(
        pub_b64.len(),
        87,
        "VAPID public key must be 87-char base64url"
    );
    assert!(
        pub_b64.starts_with("B"),
        "uncompressed P-256 point starts with 0x04 → base64url 'B'"
    );
}

#[test]
fn vapid_signer_produces_jwt_with_three_segments() {
    let key = VapidKey::generate();
    let signer = VapidSigner::new(key);
    let jwt = signer
        .sign("https://example.org", "mailto:admin@example.org", 12 * 3600)
        .unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3, "JWT must have 3 dot-separated segments");
    let header_bytes = base64_url_no_pad_decode(parts[0]).unwrap();
    let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
    assert_eq!(header["typ"], "JWT");
    assert_eq!(header["alg"], "ES256");
}

#[test]
fn vapid_signer_claims_have_aud_sub_exp() {
    let key = VapidKey::generate();
    let signer = VapidSigner::new(key);
    let jwt = signer
        .sign("https://fcm.googleapis.com", "mailto:a@b.com", 12 * 3600)
        .unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    let claims_bytes = base64_url_no_pad_decode(parts[1]).unwrap();
    let claims: serde_json::Value = serde_json::from_slice(&claims_bytes).unwrap();
    assert_eq!(claims["aud"], "https://fcm.googleapis.com");
    assert_eq!(claims["sub"], "mailto:a@b.com");
    let exp = claims["exp"].as_i64().unwrap();
    let now = chrono::Utc::now().timestamp();
    assert!(
        exp > now && exp <= now + 12 * 3600 + 5,
        "exp must be ~12h in the future"
    );
}

#[test]
fn vapid_signer_emits_exact_rfc8292_claim_set() {
    // Lock the JWT claim set down to {iat, exp, sub, aud}. RFC 8292 §2
    // requires aud/sub/exp; we include iat for replay-window tracking.
    // We deliberately DROP `nbf` (jwt-simple defaults to it) because push
    // services with negative clock skew reject the request before nbf
    // passes — observed against some non-FCM endpoints.
    //
    // We also assert NO unexpected extras (e.g. jti, iss, nonce) so a
    // future jwt-simple bump or signer refactor that re-introduces extras
    // fails this test rather than silently shipping a wider claim set.
    let key = VapidKey::generate();
    let signer = VapidSigner::new(key);
    let jwt = signer
        .sign("https://example.org", "mailto:admin@example.org", 12 * 3600)
        .unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    let claims_bytes = base64_url_no_pad_decode(parts[1]).unwrap();
    let claims: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&claims_bytes).unwrap();

    let keys: std::collections::BTreeSet<&str> = claims.keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<&str> =
        ["iat", "exp", "sub", "aud"].into_iter().collect();
    assert_eq!(
        keys, expected,
        "claim set must be exactly {{iat, exp, sub, aud}} — extras risk clock-skew rejection on strict push services"
    );

    // nbf rejection is the regression we're guarding — explicit absence.
    assert!(
        !claims.contains_key("nbf"),
        "nbf must be absent — push services with negative clock skew reject otherwise"
    );
}

fn base64_url_no_pad_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s)
}

// ---------------------------------------------------------------------------
// VAPID TTL bounds — RFC 8292 caps the JWT lifetime at 24 hours. Zero /
// negative TTLs would produce already-expired tokens; the previous `as u64`
// cast quietly wrapped negatives into multi-century lifetimes.
// ---------------------------------------------------------------------------

#[test]
fn sign_rejects_zero_ttl() {
    let signer = VapidSigner::new(VapidKey::generate());
    let err = signer
        .sign("https://example.org", "mailto:a@b.com", 0)
        .unwrap_err();
    assert!(
        format!("{err}").contains("TTL must be positive"),
        "got: {err}"
    );
}

#[test]
fn sign_rejects_negative_ttl() {
    let signer = VapidSigner::new(VapidKey::generate());
    let err = signer
        .sign("https://example.org", "mailto:a@b.com", -1)
        .unwrap_err();
    assert!(
        format!("{err}").contains("TTL must be positive"),
        "got: {err}"
    );
}

#[test]
fn sign_rejects_ttl_above_24h() {
    let signer = VapidSigner::new(VapidKey::generate());
    let err = signer
        .sign("https://example.org", "mailto:a@b.com", 24 * 3600 + 1)
        .unwrap_err();
    assert!(format!("{err}").contains("exceeds RFC 8292"), "got: {err}");
}

#[test]
fn sign_accepts_exactly_24h_ttl() {
    let signer = VapidSigner::new(VapidKey::generate());
    let jwt = signer
        .sign("https://example.org", "mailto:a@b.com", 24 * 3600)
        .expect("24h boundary must be accepted");
    assert_eq!(jwt.split('.').count(), 3, "valid JWT must have 3 segments");
}
