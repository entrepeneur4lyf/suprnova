//! Integration tests for the `Crypt` facade.
//!
//! The encryption key lives in a process-wide `OnceLock`, so all tests
//! in this file share one key. We install it lazily under a mutex and
//! serialize the suite for deterministic ordering.

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use suprnova::{Crypt, CryptPurpose, EncryptionKey};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static INSTALLED: OnceLock<()> = OnceLock::new();

fn ensure_key() {
    INSTALLED.get_or_init(|| {
        // Install a deterministic-but-test-only key once.
        let key = EncryptionKey::generate();
        // Ignore the bool return — another test in another file might
        // have already installed a key; either way `encrypt_string`
        // works on whatever is installed.
        let _ = suprnova::crypto::_test_install_key(key);
    });
}

#[test]
fn round_trip_string() {
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let wire = Crypt::encrypt_string(CryptPurpose::Cookie, "hello, world").unwrap();
    assert_ne!(wire, "hello, world");
    let plain = Crypt::decrypt_string(CryptPurpose::Cookie, &wire).unwrap();
    assert_eq!(plain, "hello, world");
}

#[test]
fn tamper_detection() {
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let wire = Crypt::encrypt_string(CryptPurpose::Cookie, "don't touch me").unwrap();
    let mut bytes = wire.into_bytes();
    let idx = bytes.len() - 1;
    // Flip one ASCII character to something else in the base64 alphabet
    bytes[idx] = if bytes[idx] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(bytes).unwrap();
    assert!(Crypt::decrypt_string(CryptPurpose::Cookie, &tampered).is_err());
}

#[test]
fn url_safe_no_padding() {
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    // Encrypt enough data that the base64 output covers multiple
    // alphabet positions; padding would show up at the end.
    let wire = Crypt::encrypt_string(
        CryptPurpose::Cookie,
        "the quick brown fox jumps over the lazy dog -- multiple times",
    )
    .unwrap();
    assert!(!wire.contains('+'));
    assert!(!wire.contains('/'));
    assert!(!wire.contains('='));
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Payload {
    user_id: i64,
    role: String,
}

#[test]
fn encrypt_t_round_trip() {
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let payload = Payload {
        user_id: 7,
        role: "admin".to_string(),
    };
    let wire = Crypt::encrypt(CryptPurpose::Cookie, &payload).unwrap();
    let decoded: Payload = Crypt::decrypt(CryptPurpose::Cookie, &wire).unwrap();
    assert_eq!(decoded, payload);
}

#[test]
fn cross_purpose_ciphertext_is_rejected() {
    // The domain-separation guarantee: ciphertext minted under one
    // purpose must NOT decrypt under another, even with the same key.
    // This is the property that blocks cross-surface ciphertext replay
    // (e.g. forging a cookie out of a stolen cursor payload, or
    // injecting a 2FA recovery blob into a cookie slot).
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let cookie_wire = Crypt::encrypt_string(CryptPurpose::Cookie, "session-id-42").unwrap();
    // Same wire, every other purpose — must reject.
    for foreign in [
        CryptPurpose::Cursor,
        CryptPurpose::TwoFactorSecret,
        CryptPurpose::TwoFactorRecovery,
        CryptPurpose::Cast,
    ] {
        let err = Crypt::decrypt_string(foreign, &cookie_wire).unwrap_err();
        assert!(
            format!("{err}").contains("AEAD decrypt failed"),
            "{:?} should reject cookie ciphertext with AEAD failure, got: {err}",
            foreign
        );
    }
    // Sanity: original purpose still decrypts.
    let plain = Crypt::decrypt_string(CryptPurpose::Cookie, &cookie_wire).unwrap();
    assert_eq!(plain, "session-id-42");
}

#[test]
fn two_factor_secret_and_recovery_are_distinct_domains() {
    // Recovery codes and TOTP secret live in the same row but distinct
    // columns. Distinct purposes mean an attacker with write access to
    // one column cannot replay it into the other and have the read
    // path silently succeed.
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let secret_wire =
        Crypt::encrypt_string(CryptPurpose::TwoFactorSecret, "JBSWY3DPEHPK3PXP").unwrap();
    assert!(Crypt::decrypt_string(CryptPurpose::TwoFactorRecovery, &secret_wire).is_err());
    let recovery_wire =
        Crypt::encrypt_string(CryptPurpose::TwoFactorRecovery, "code-a\ncode-b\ncode-c").unwrap();
    assert!(Crypt::decrypt_string(CryptPurpose::TwoFactorSecret, &recovery_wire).is_err());
}

#[test]
fn appears_encrypted_matches_real_ciphertext() {
    // A real `Crypt::encrypt_string` output is always recognised by
    // `appears_encrypted`. Mirrors the contract Laravel relies on in
    // its `EncryptCookies` middleware to skip already-encrypted
    // cookies on the egress pass.
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let wire = Crypt::encrypt_string(CryptPurpose::Cookie, "hello").unwrap();
    assert!(Crypt::appears_encrypted(&wire));
    // Even the empty plaintext, encrypted, is recognised — the
    // ciphertext still carries nonce + tag.
    let wire_empty = Crypt::encrypt_string(CryptPurpose::Cookie, "").unwrap();
    assert!(Crypt::appears_encrypted(&wire_empty));
}

#[test]
fn appears_encrypted_rejects_plaintext_and_short_payloads() {
    // No Crypt::init required — `appears_encrypted` is a static
    // shape check, never touches the keyring.
    assert!(!Crypt::appears_encrypted("plain text with spaces"));
    assert!(!Crypt::appears_encrypted(""));
    // Valid base64 but too short to be a nonce+tag (28 bytes min).
    assert!(!Crypt::appears_encrypted("YWJj")); // "abc" — 3 bytes
    // Non-base64-url characters.
    assert!(!Crypt::appears_encrypted("not/valid+base64="));
}

#[test]
fn previous_key_count_accessors_agree() {
    // The bool/usize accessors must always agree. We can't assert
    // exact zero because other test files in the same process may
    // have installed a keyring with previous keys.
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let n = Crypt::previous_key_count();
    assert_eq!(Crypt::has_previous_keys(), n > 0);
}
