use suprnova_web_push::{ContentEncoding, Payload};

const RECEIVER_P256DH_B64URL: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const RECEIVER_AUTH_B64URL: &str = "BTBZMqHH6r4Tts7J_aSIgg";

#[test]
fn encrypt_produces_aes128gcm_block() {
    let plaintext = b"hello world";
    let payload = Payload::encrypt(
        plaintext,
        RECEIVER_P256DH_B64URL,
        RECEIVER_AUTH_B64URL,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap();

    assert!(
        payload.body().len() >= 21 + 17,
        "encrypted payload must include header + body"
    );
    assert_eq!(payload.content_encoding(), ContentEncoding::Aes128Gcm);
}

#[test]
fn encrypt_rejects_payload_above_max_size() {
    let huge = vec![0u8; 4096];
    let err = Payload::encrypt(
        &huge,
        RECEIVER_P256DH_B64URL,
        RECEIVER_AUTH_B64URL,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("too large") || s.contains("size"),
        "expected size-rejection error, got: {s}"
    );
}

#[test]
fn encrypt_with_bad_p256dh_returns_error() {
    let err = Payload::encrypt(
        b"hi",
        "not-base64url!",
        RECEIVER_AUTH_B64URL,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("base64") || s.contains("p256") || s.contains("Encryption"),
        "expected key-decode error, got: {s}"
    );
}

// ---------------------------------------------------------------------------
// Key-length validation — RFC 8291 fixes p256dh at a 65-byte uncompressed
// SEC1 point and auth at a 16-byte secret. A short / long / wrong-tag value
// must surface a framework-typed Encryption error with the offending size or
// tag in the message, not get passed to `ece::encrypt` (which would surface
// a generic crypto error from a layer below).
// ---------------------------------------------------------------------------

use base64::Engine;

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[test]
fn encrypt_rejects_short_p256dh() {
    // 64 bytes instead of 65 — one short of an uncompressed P-256 point.
    let short = b64url(&[0x04u8; 64]);
    let err = Payload::encrypt(
        b"hi",
        &short,
        RECEIVER_AUTH_B64URL,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("p256dh"),
        "expected p256dh-length error, got: {s}"
    );
    assert!(
        s.contains("65"),
        "error must cite the expected 65-byte length, got: {s}"
    );
}

#[test]
fn encrypt_rejects_long_p256dh() {
    // 66 bytes — one byte too many.
    let long = b64url(&[0x04u8; 66]);
    let err = Payload::encrypt(
        b"hi",
        &long,
        RECEIVER_AUTH_B64URL,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("p256dh"),
        "expected p256dh-length error, got: {s}"
    );
    assert!(
        s.contains("65"),
        "error must cite the expected length, got: {s}"
    );
}

#[test]
fn encrypt_rejects_compressed_sec1_point() {
    // 65 bytes is the right length, but the leading tag must be 0x04
    // (uncompressed). A compressed point starts with 0x02 / 0x03.
    let mut bad = vec![0x02u8; 65];
    bad[0] = 0x02;
    let s_b64 = b64url(&bad);
    let err = Payload::encrypt(
        b"hi",
        &s_b64,
        RECEIVER_AUTH_B64URL,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("uncompressed") || s.contains("0x04") || s.contains("0x02"),
        "expected tag error, got: {s}"
    );
}

#[test]
fn encrypt_rejects_short_auth_secret() {
    // 15 bytes — one short of the 16-byte auth secret.
    let short = b64url(&[0xAAu8; 15]);
    let err = Payload::encrypt(
        b"hi",
        RECEIVER_P256DH_B64URL,
        &short,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("auth"), "expected auth-length error, got: {s}");
    assert!(
        s.contains("16"),
        "error must cite the expected 16-byte length, got: {s}"
    );
}

#[test]
fn encrypt_rejects_long_auth_secret() {
    // 17 bytes — one byte too many.
    let long = b64url(&[0xAAu8; 17]);
    let err = Payload::encrypt(
        b"hi",
        RECEIVER_P256DH_B64URL,
        &long,
        ContentEncoding::Aes128Gcm,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("auth"), "expected auth-length error, got: {s}");
    assert!(
        s.contains("16"),
        "error must cite the expected length, got: {s}"
    );
}
