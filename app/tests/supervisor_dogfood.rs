//! Dogfood test: verifies that `LogHeartbeat` is actually registered in the
//! `app` crate's link surface via `inventory::submit!`.
//!
//! This test compiles into the app crate's test binary — the closest mirror to
//! the real `app` binary's link surface — so it confirms the inventory static
//! is retained by the linker.

/// The heartbeat supervisor must appear in the inventory after the app crate
/// loads, because `app/src/supervisors/heartbeat.rs` uses `inventory::submit!`
/// at the module level.
#[test]
fn heartbeat_supervisor_is_registered() {
    // Force the supervisors module to be included — same guarantee the
    // binary gets from `lib.rs` declaring `pub mod supervisors`.
    let _touch = std::hint::black_box(
        app::supervisors::heartbeat::LogHeartbeat,
    );

    let names: Vec<&str> = suprnova::inventory::iter::<suprnova::SupervisorEntry>()
        .map(|e| (e.factory)().name())
        .collect();

    assert!(
        names.contains(&"heartbeat"),
        "expected 'heartbeat' supervisor in inventory; got {:?}",
        names
    );
}
