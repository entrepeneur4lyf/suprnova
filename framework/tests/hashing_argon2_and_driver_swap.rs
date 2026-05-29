//! Integration tests for the Argon2 drivers + driver-swap semantics.
//!
//! Covers Module 7 hashing-parity sweep:
//!   - Argon2id round-trips through the explicit-driver API
//!   - Argon2i round-trips
//!   - argon hashes verify via the algorithm-agnostic facade
//!   - `is_hashed` recognises both bcrypt and argon hashes
//!   - `needs_rehash` rotates across algorithms
//!   - `info()` parses both families
//!   - The bcrypt 72-byte ceiling does NOT apply to argon drivers

use suprnova::hashing::{
    self, Argon2Options, Argon2iHasher, Argon2idHasher, BcryptHasher, BcryptOptions,
    MAX_BCRYPT_PASSWORD_BYTES, hash_with, info, is_hashed, needs_rehash_with, verify_with,
};

#[test]
fn argon2id_minimal_round_trip() {
    // Use minimum-cost params so the test runs fast (real prod defaults
    // are exercised elsewhere; here we want speed, not strength).
    let driver = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");

    let pw = "correct horse battery staple";
    let h = hash_with(&driver, pw).expect("hash");
    assert!(h.starts_with("$argon2id$"));
    assert!(verify_with(&driver, pw, &h).expect("verify"));
    assert!(!verify_with(&driver, "wrong", &h).expect("verify"));
}

#[test]
fn argon2i_minimal_round_trip() {
    let driver = Argon2iHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");

    let pw = "argon2i works too";
    let h = hash_with(&driver, pw).expect("hash");
    assert!(h.starts_with("$argon2i$"));
    assert!(verify_with(&driver, pw, &h).expect("verify"));
}

#[test]
fn argon2id_accepts_arbitrary_length_password() {
    // The 72-byte ceiling is bcrypt-specific. Argon2 must accept this.
    let driver = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");

    let long = "x".repeat(MAX_BCRYPT_PASSWORD_BYTES + 500);
    let h = hash_with(&driver, &long).expect("argon2 hashes arbitrary-length");
    assert!(verify_with(&driver, &long, &h).expect("verify"));
}

#[test]
fn is_hashed_recognises_both_bcrypt_and_argon() {
    let bcrypt_driver = BcryptHasher::new(BcryptOptions { rounds: 4 });
    let argon_driver = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");

    let bcrypt_hash = hash_with(&bcrypt_driver, "test").expect("hash");
    let argon_hash = hash_with(&argon_driver, "test").expect("hash");

    assert!(is_hashed(&bcrypt_hash), "bcrypt hash must register");
    assert!(is_hashed(&argon_hash), "argon hash must register");
    assert!(!is_hashed("plaintext"), "plaintext must not register");
    assert!(!is_hashed(""), "empty must not register");
}

#[test]
fn needs_rehash_rotates_across_algorithms() {
    let bcrypt_driver = BcryptHasher::new(BcryptOptions { rounds: 12 });
    let argon_driver = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");

    let bcrypt_hash = hash_with(&bcrypt_driver, "test").expect("hash");
    let argon_hash = hash_with(&argon_driver, "test").expect("hash");

    // bcrypt driver sees the argon hash → needs rehash.
    assert!(needs_rehash_with(&bcrypt_driver, &argon_hash));
    // argon driver sees the bcrypt hash → needs rehash.
    assert!(needs_rehash_with(&argon_driver, &bcrypt_hash));
    // Each driver sees its own hash at current params → no rehash.
    assert!(!needs_rehash_with(&bcrypt_driver, &bcrypt_hash));
    assert!(!needs_rehash_with(&argon_driver, &argon_hash));
}

#[test]
fn info_parses_both_families() {
    let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 4 });
    let argon = Argon2idHasher::new(Argon2Options {
        memory: 16,
        time: 2,
        threads: 1,
    })
    .expect("ctor");

    let bh = hash_with(&bcrypt, "test").expect("hash");
    let ah = hash_with(&argon, "test").expect("hash");

    let bi = info(&bh);
    assert_eq!(bi.algo.as_str(), "bcrypt");
    assert_eq!(bi.rounds, Some(4));
    // bcrypt crate emits $2b$ as canonical.
    assert_eq!(bi.bcrypt_variant, Some("2b"));

    let ai = info(&ah);
    assert_eq!(ai.algo.as_str(), "argon2id");
    assert_eq!(ai.memory, Some(16));
    assert_eq!(ai.time, Some(2));
    assert_eq!(ai.threads, Some(1));
}

#[test]
fn verify_algorithm_gate_rejects_cross_algo() {
    let bcrypt_strict = BcryptHasher::new(BcryptOptions { rounds: 4 }).with_verify_algorithm(true);
    let argon = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");

    let argon_hash = hash_with(&argon, "the password").expect("hash");
    // Strict bcrypt rejects an argon hash even if the password matches.
    assert!(
        !verify_with(&bcrypt_strict, "the password", &argon_hash).expect("verify"),
        "verify_algorithm=true must reject cross-algorithm hashes"
    );

    // And in the other direction.
    let argon_strict = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor")
    .with_verify_algorithm(true);
    let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 4 });
    let bcrypt_hash = hash_with(&bcrypt, "the password").expect("hash");
    assert!(
        !verify_with(&argon_strict, "the password", &bcrypt_hash).expect("verify"),
        "verify_algorithm=true must reject cross-algorithm hashes (other direction)"
    );
}

#[test]
fn bcrypt_legacy_variants_need_rehash() {
    let driver = BcryptHasher::new(BcryptOptions { rounds: 12 });
    // Hand-craft a $2a$ prefix on a real $2b$ hash to test the legacy rotation.
    let h = hashing::hash_with_cost("test", 12).expect("hash");
    assert!(h.starts_with("$2b$"));
    for variant in ["$2a$", "$2x$", "$2y$"] {
        let mut legacy = String::from(variant);
        legacy.push_str(&h[4..]);
        assert!(
            needs_rehash_with(&driver, &legacy),
            "legacy {variant} must trigger rehash"
        );
    }
}

#[tokio::test]
async fn async_facade_dispatches_through_default_driver() {
    // No env mutation — just confirm the default path doesn't panic and
    // round-trips on whichever driver is configured (bcrypt under the
    // default workspace test profile).
    let pw = "another good password";
    let h = hashing::hash_async(pw).await.expect("hash_async");
    assert!(hashing::verify_async(pw, &h).await.expect("verify_async"));
    assert!(!hashing::verify_async("wrong", &h).await.expect("verify"));
}

#[test]
fn facade_verify_with_argon_driver_still_verifies_legacy_bcrypt() {
    // This is the critical migration property. After `HASH_DRIVER=argon2id`
    // is set, EXISTING bcrypt hashes must STILL verify so users can log in
    // and the auth flow can rotate the stored hash via `needs_rehash` on
    // success. Pre-fix the facade dispatched through the configured
    // driver's `verify`, which is single-algorithm — so every legacy
    // bcrypt user would get locked out the instant the env flipped.
    use suprnova::hashing::verify_with;

    // Simulate the "freshly flipped to argon2id" world: configured driver
    // is argon2id, but the stored hash is legacy bcrypt.
    let configured = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");

    let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 4 });
    let legacy_hash = hash_with(&bcrypt, "the_users_password").expect("hash");

    // CORRECT password against bcrypt hash, through argon-configured facade
    // → must verify true. This is what lets users log in and rotate.
    assert!(
        verify_with(&configured, "the_users_password", &legacy_hash).expect("verify"),
        "argon-configured facade must still verify a bcrypt hash with the correct password"
    );

    // Wrong password still fails.
    assert!(
        !verify_with(&configured, "wrong_password", &legacy_hash).expect("verify"),
        "wrong password must NOT verify even against a legacy bcrypt hash"
    );
}

#[test]
fn facade_verify_with_bcrypt_driver_verifies_argon_hash() {
    // The reverse direction — useful if a deployment ever migrates back
    // or runs mixed bcrypt+argon hashes during a partial rollout.
    use suprnova::hashing::verify_with;

    let configured = BcryptHasher::new(BcryptOptions { rounds: 4 });
    let argon = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor");
    let argon_hash = hash_with(&argon, "user_password").expect("hash");

    assert!(
        verify_with(&configured, "user_password", &argon_hash).expect("verify"),
        "bcrypt-configured facade must still verify an argon hash with the correct password"
    );
}

#[test]
fn facade_hash_verify_gate_rejects_cross_algo_when_enabled() {
    // With HASH_VERIFY=true, the facade refuses to verify a stored hash
    // whose algorithm differs from the configured driver — strict mode
    // for deployments past the rotation window.
    use suprnova::hashing::verify_with;

    let configured = Argon2idHasher::new(Argon2Options {
        memory: 8,
        time: 1,
        threads: 1,
    })
    .expect("ctor")
    .with_verify_algorithm(true);

    let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 4 });
    let legacy_hash = hash_with(&bcrypt, "user_password").expect("hash");

    // Strict mode + correct password against bcrypt hash through argon
    // driver → REJECTED at the facade. Caller's auth flow surfaces the
    // standard "invalid credentials" response.
    assert!(
        !verify_with(&configured, "user_password", &legacy_hash).expect("verify"),
        "HASH_VERIFY=true must reject cross-algorithm hashes even with correct password"
    );
}
