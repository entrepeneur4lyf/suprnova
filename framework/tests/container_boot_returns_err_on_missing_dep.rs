//! Regression: HIGH audit finding `container` #290 — boot must return
//! `Err(FrameworkError)` when a singleton's dependency is genuinely
//! missing (or its dependency graph is cyclic), instead of panicking
//! inside the registration closure.
//!
//! This file is a separate integration test binary so the broken
//! `SingletonEntry` we install via `inventory::submit!` doesn't
//! pollute the other container test binaries' inventories. Each
//! `tests/*.rs` file becomes its own binary, so the broken entry is
//! scoped to this test.

use suprnova::App;
use suprnova::container::provider::SingletonEntry;

/// A type that is intentionally never `App::singleton`-installed. The
/// broken-entry registration below tries to `App::resolve` it; that
/// resolve will always fail, so the fixed-point loop must terminate
/// with `Err` rather than spinning forever or panicking.
#[derive(Clone)]
struct NeverRegistered;

/// Broken `SingletonEntry` — its registration closure resolves a type
/// that we never install. With the fix in place, the fixed-point loop
/// makes one no-progress pass and returns `Err(FrameworkError::internal)`
/// naming this entry.
fn broken_register() -> Result<(), String> {
    let _ = App::resolve::<NeverRegistered>().map_err(|e| e.to_string())?;
    Ok(())
}

::suprnova::inventory::submit! {
    SingletonEntry {
        register: broken_register,
        name: "NeverRegistered__test_broken",
    }
}

#[test]
fn boot_returns_err_with_descriptive_message_when_dep_unresolvable() {
    App::init();
    let err = App::boot_services().expect_err("boot must Err when a singleton's dep is missing");

    let msg = format!("{err}");

    // Error should name the broken entry so the operator can find it.
    assert!(
        msg.contains("NeverRegistered__test_broken"),
        "boot error should name the failing entry; got: {msg}"
    );

    // Error should mention the missing dep so the operator can diagnose.
    // The macro-generated message embeds the type name; here we don't go
    // through the macro (we use a hand-written closure), but the chained
    // FrameworkError display includes the inner reason — which mentions
    // "could not be booted" and the underlying "no such service"
    // message from `App::resolve`.
    assert!(
        msg.contains("could not be booted"),
        "boot error should explain the loop ended without progress; got: {msg}"
    );
}
