//! `suprnova key:generate` — print a fresh 32-byte AES-256 key,
//! base64-URL-safe-no-pad encoded. Identical wire format to
//! `suprnova::EncryptionKey::to_base64()` so the framework's loader
//! accepts it directly via the `APP_KEY` env var.
//!
//! The four lines that mint the key are duplicated here intentionally
//! (`getrandom::fill([0u8; 32])` + base64 encode) instead of pulling
//! `suprnova` into the CLI binary, which would drag tokio-full,
//! reqwest, and SeaORM into the scaffolder.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

/// Generate a random 32-byte key, encoded URL-safe base64 (no padding).
pub fn generate_app_key() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS RNG must be available to mint an AES-256 key");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Run the `key:generate` command.
///
/// - When `show=true`, prints only the key (just the base64 string,
///   newline-terminated). Good for `APP_KEY=$(suprnova key:generate
///   --show)`.
/// - When `show=false` (default), prints the key with a friendly
///   hint about wiring it into `.env`.
pub fn run(show: bool) {
    let key = generate_app_key();

    if show {
        println!("{}", key);
        return;
    }

    use crate::ui;
    ui::info("Generated a new APP_KEY (AES-256, base64 URL-safe, no padding):");
    println!();
    println!("    {}", key);
    println!();
    ui::hint("Add it to your .env (or your secrets manager):");
    println!();
    println!("    APP_KEY={}", key);
    println!();
    ui::hint("Or in one shot:");
    println!();
    println!("    echo \"APP_KEY=$(suprnova key:generate --show)\" >> .env");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base64 of 32 bytes URL-safe-no-pad is always 43 chars.
    #[test]
    fn generated_key_is_43_chars_base64() {
        let k = generate_app_key();
        assert_eq!(k.len(), 43, "expected 43-char base64-url-no-pad, got {k:?}");
        assert!(!k.contains('+'));
        assert!(!k.contains('/'));
        assert!(!k.contains('='));
    }

    /// Round-trip through the framework's loader to prove byte-for-byte
    /// compatibility with `EncryptionKey::from_base64`.
    #[test]
    fn generated_key_loads_through_framework() {
        let k = generate_app_key();
        let parsed =
            suprnova::EncryptionKey::from_base64(&k).expect("framework should accept our key");
        // Encode the parsed key back and compare — must round-trip.
        assert_eq!(parsed.to_base64(), k);
    }

    /// Two consecutive calls must produce different keys (32 bytes
    /// from /dev/urandom — collision probability is astronomical).
    #[test]
    fn two_calls_return_different_keys() {
        let a = generate_app_key();
        let b = generate_app_key();
        assert_ne!(a, b);
    }
}
