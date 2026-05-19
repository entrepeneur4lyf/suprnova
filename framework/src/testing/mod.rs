//! Testing utilities for suprnova framework
//!
//! Provides Jest-like testing helpers including:
//! - `expect!` macro for fluent assertions with clear expected/received output
//! - `describe!` and `test!` macros for test organization
//! - `TestDatabase` for isolated database tests
//! - `TestContainer` for dependency injection in tests
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::{describe, test, expect};
//! use suprnova::testing::TestDatabase;
//!
//! describe!("UserService", {
//!     test!("creates a user", async fn(db: TestDatabase) {
//!         let service = UserService::new();
//!         let user = service.create("test@example.com").await.unwrap();
//!
//!         expect!(user.email).to_equal("test@example.com".to_string());
//!     });
//! });
//! ```

mod expect;

pub use crate::container::testing::{TestContainer, TestContainerGuard};
pub use crate::database::testing::TestDatabase;
pub use expect::{set_current_test_name, Expect};

use crate::crypto::EncryptionKey;

/// Install a deterministic encryption key for tests. Idempotent — the
/// underlying `Crypt` facade is `OnceLock`-backed, so the second call is
/// a no-op and safe to call from every test that touches an encrypted
/// cast. The chosen key is a fixed 32-byte zero key encoded as URL-safe
/// base64 (no padding), giving deterministic ciphertext behaviour
/// across runs.
///
/// **Test-only.** Bypasses the production APP_KEY validation path.
/// Production code must go through `Crypt::init` from
/// `Server::from_config` instead.
pub fn install_test_encryption_key() {
    // 43 chars × 6 bits = 258 bits ≈ 32 bytes (the trailing 2 bits are
    // ignored). URL_SAFE_NO_PAD decode of "A" * 43 yields 32 zero bytes.
    const TEST_KEY_B64: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let key = EncryptionKey::from_base64(TEST_KEY_B64)
        .expect("32-byte test key parses from canonical zero-base64");
    // `_test_install_key` (in `crate::crypto`) returns false if a key
    // was already installed — we ignore the return because idempotent
    // installation is the contract.
    let _ = crate::crypto::_test_install_key(key);
}
