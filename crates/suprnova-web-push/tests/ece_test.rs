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
