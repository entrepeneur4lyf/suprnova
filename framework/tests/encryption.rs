//! Integration tests for the `Crypt` facade.
//!
//! The encryption key lives in a process-wide `OnceLock`, so all tests
//! in this file share one key. We install it lazily under a mutex and
//! serialize the suite for deterministic ordering.

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use suprnova::{Crypt, EncryptionKey};

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
    let wire = Crypt::encrypt_string("hello, world").unwrap();
    assert_ne!(wire, "hello, world");
    let plain = Crypt::decrypt_string(&wire).unwrap();
    assert_eq!(plain, "hello, world");
}

#[test]
fn tamper_detection() {
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    let wire = Crypt::encrypt_string("don't touch me").unwrap();
    let mut bytes = wire.into_bytes();
    let idx = bytes.len() - 1;
    // Flip one ASCII character to something else in the base64 alphabet
    bytes[idx] = if bytes[idx] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(bytes).unwrap();
    assert!(Crypt::decrypt_string(&tampered).is_err());
}

#[test]
fn url_safe_no_padding() {
    let _g = TEST_LOCK.lock().unwrap();
    ensure_key();
    // Encrypt enough data that the base64 output covers multiple
    // alphabet positions; padding would show up at the end.
    let wire =
        Crypt::encrypt_string("the quick brown fox jumps over the lazy dog -- multiple times")
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
    let wire = Crypt::encrypt(&payload).unwrap();
    let decoded: Payload = Crypt::decrypt(&wire).unwrap();
    assert_eq!(decoded, payload);
}
