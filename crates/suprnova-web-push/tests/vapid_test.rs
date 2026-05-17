use suprnova_web_push::{VapidKey, VapidSigner};

#[test]
fn vapid_key_generates_p256_keypair() {
    let key = VapidKey::generate();
    let pub_b64 = key.public_key_uncompressed_b64url();
    assert_eq!(pub_b64.len(), 87, "VAPID public key must be 87-char base64url");
    assert!(pub_b64.starts_with("B"), "uncompressed P-256 point starts with 0x04 → base64url 'B'");
}

#[test]
fn vapid_signer_produces_jwt_with_three_segments() {
    let key = VapidKey::generate();
    let signer = VapidSigner::new(key);
    let jwt = signer.sign("https://example.org", "mailto:admin@example.org", 12 * 3600).unwrap();
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
    let jwt = signer.sign("https://fcm.googleapis.com", "mailto:a@b.com", 12 * 3600).unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    let claims_bytes = base64_url_no_pad_decode(parts[1]).unwrap();
    let claims: serde_json::Value = serde_json::from_slice(&claims_bytes).unwrap();
    assert_eq!(claims["aud"], "https://fcm.googleapis.com");
    assert_eq!(claims["sub"], "mailto:a@b.com");
    let exp = claims["exp"].as_i64().unwrap();
    let now = chrono::Utc::now().timestamp();
    assert!(exp > now && exp <= now + 12 * 3600 + 5, "exp must be ~12h in the future");
}

fn base64_url_no_pad_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s)
}
