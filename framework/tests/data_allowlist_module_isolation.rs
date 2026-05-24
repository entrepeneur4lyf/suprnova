//! Regression: HIGH audit finding `data` #336 (second half) — include
//! allowlist registry was keyed by bare struct name, so DTOs with the
//! same identifier in different modules collided nondeterministically.
//!
//! Before the fix, both `module_a::UserDto` and `module_b::UserDto`
//! registered under the key `"UserDto"`. Whichever inventory entry
//! drained second won; the loser silently lost its allowlist.
//!
//! Post-fix the key is `concat!(module_path!(), "::", stringify!(...))`,
//! resolved at compile time to a unique `&'static str` literal per
//! module path. Two same-named DTOs in different modules retain their
//! own allowlists.

use suprnova::data::registry;

// The DTOs below are only used for their derive side effects (inventory
// registration). The fields themselves are never read, hence
// `#[allow(dead_code)]`.

mod module_a {
    #[derive(suprnova::Data, validator::Validate)]
    #[allow(dead_code)]
    pub struct UserDto {
        pub id: i64,
        #[data(allow_include)]
        pub profile: Option<serde_json::Value>,
    }
}

mod module_b {
    #[derive(suprnova::Data, validator::Validate)]
    #[allow(dead_code)]
    pub struct UserDto {
        pub id: i64,
        // Different allowlist field on purpose — if the registry keyed
        // on bare names, one of these would silently overwrite the
        // other.
        #[data(allow_include)]
        pub team: Option<serde_json::Value>,
    }
}

#[test]
fn two_same_named_dtos_in_different_modules_keep_distinct_allowlists() {
    // Each DTO registers under its fully-qualified module path. Build
    // the same key shape the macro emits and verify each module's
    // allowlist is preserved.
    let key_a = ::std::concat!(::std::module_path!(), "::module_a::UserDto");
    let key_b = ::std::concat!(::std::module_path!(), "::module_b::UserDto");

    // Module A's allowlist: only `profile`.
    assert!(
        registry::is_allowed(key_a, "profile"),
        "module_a::UserDto must keep its `profile` allowlist entry; \
         tried key `{key_a}`"
    );
    assert!(
        !registry::is_allowed(key_a, "team"),
        "module_a::UserDto must NOT have inherited module_b's `team` \
         entry — that would be the collision the audit flagged"
    );

    // Module B's allowlist: only `team`.
    assert!(
        registry::is_allowed(key_b, "team"),
        "module_b::UserDto must keep its `team` allowlist entry; \
         tried key `{key_b}`"
    );
    assert!(
        !registry::is_allowed(key_b, "profile"),
        "module_b::UserDto must NOT have inherited module_a's `profile` \
         entry — that would be the collision the audit flagged"
    );

    // The bare "UserDto" key must NOT match either — the registry no
    // longer accepts unqualified names.
    assert!(
        !registry::is_allowed("UserDto", "profile"),
        "bare struct names must not match the registry — the audit fix \
         requires fully-qualified keys"
    );
    assert!(!registry::is_allowed("UserDto", "team"));
}
