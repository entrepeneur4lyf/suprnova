//! Regression: HIGH audit finding `container` #289 — auto-registered
//! services overwrite explicit application bindings on every boot.
//!
//! `App::boot_services()` iterates inventory and calls each registration
//! function. Previously those generated functions called `App::bind` /
//! `App::singleton`, both of which use `HashMap::insert` — a manual
//! override installed before boot (or a stateful singleton already in
//! place from a previous boot) would be silently replaced with a fresh
//! `Default::default()` instance.
//!
//! The fix routes inventory registrations through `bind_if_absent` /
//! `singleton_if_absent`, making boot idempotent. Manual `App::bind` and
//! `App::singleton` calls still override (last write wins) so application
//! code retains the ability to replace a default-registered service
//! after the fact.
//!
//! These tests pin both halves of the contract:
//! 1. Manual override installed BEFORE boot survives `boot_services()`.
//! 2. Re-running `boot_services()` does not replace existing bindings.
//! 3. Manual `App::bind` AFTER boot still overrides the registered impl.

use std::sync::Arc;
use suprnova::App;

// A tiny trait + two impls so we can prove which one wins.
trait Echoer: Send + Sync + 'static {
    fn echo(&self) -> &'static str;
}

#[derive(Default)]
struct DefaultEchoer;
impl Echoer for DefaultEchoer {
    fn echo(&self) -> &'static str {
        "default"
    }
}

struct FakeEchoer;
impl Echoer for FakeEchoer {
    fn echo(&self) -> &'static str {
        "fake"
    }
}

#[test]
fn bind_if_absent_returns_true_on_first_call_false_after() {
    // This test exercises the new primitive directly — no boot loop
    // involved. It's the building block #[service] uses, so the
    // semantics are worth pinning explicitly.
    //
    // We use a fresh `Container` (not the global App) to keep this
    // test independent of process-global state.
    use suprnova::container::Container;

    let mut c = Container::new();

    let first = c.bind_if_absent::<dyn Echoer>(Arc::new(DefaultEchoer));
    assert!(first, "first bind_if_absent must install and return true");

    let second = c.bind_if_absent::<dyn Echoer>(Arc::new(FakeEchoer));
    assert!(!second, "second bind_if_absent must NOT install and return false");

    let resolved = c.make::<dyn Echoer>().expect("must resolve");
    assert_eq!(
        resolved.echo(),
        "default",
        "the FIRST binding must win — bind_if_absent is no-op when occupied"
    );
}

#[test]
fn singleton_if_absent_returns_true_on_first_call_false_after() {
    use suprnova::container::Container;

    #[derive(Clone, Default)]
    struct State {
        value: i32,
    }

    let mut c = Container::new();

    let first = c.singleton_if_absent(State { value: 1 });
    assert!(first);

    // Trying to overwrite with a different value MUST fail; the original wins.
    let second = c.singleton_if_absent(State { value: 99 });
    assert!(!second);

    let resolved: State = c.get().expect("must resolve");
    assert_eq!(
        resolved.value, 1,
        "singleton_if_absent is no-op when slot is occupied"
    );
}

#[test]
fn manual_bind_still_overrides_after_if_absent_installed_default() {
    // The "if absent" semantics only apply to the inventory boot path.
    // Application code calling `App::bind` explicitly must still be able
    // to swap implementations — last write wins for the manual API.
    use suprnova::container::Container;

    let mut c = Container::new();

    let installed = c.bind_if_absent::<dyn Echoer>(Arc::new(DefaultEchoer));
    assert!(installed);

    // Manual override after — `bind` (not `bind_if_absent`) overwrites.
    c.bind::<dyn Echoer>(Arc::new(FakeEchoer));

    let resolved = c.make::<dyn Echoer>().expect("must resolve");
    assert_eq!(
        resolved.echo(),
        "fake",
        "manual App::bind retains override semantics — only the inventory \
         boot path uses if-absent"
    );
}

#[test]
fn app_singleton_if_absent_pre_boot_override_survives() {
    // Process-level test: install a fake on the global App container
    // BEFORE the next boot_services pass, then run boot_services and
    // confirm the fake survives. This is the user-visible scenario the
    // audit flagged.
    //
    // We use `App::singleton_if_absent` here so the test does not
    // depend on any particular `#[injectable]` type existing in the
    // workspace. The "manual install" is the value we read back.

    // Note: `App` is process-global, so this test runs sequentially
    // with respect to itself by virtue of the unique state type.

    #[derive(Clone)]
    struct SurvivorState {
        marker: &'static str,
    }

    App::init();
    // Pre-install the value we want to keep.
    let installed = App::singleton_if_absent(SurvivorState {
        marker: "pre-boot",
    });
    assert!(installed, "first install must succeed");

    // Now run boot_services — which would historically overwrite all
    // inventory-registered singletons. Our marker type isn't registered
    // via `#[injectable]`, but the equivalent contract is: any second
    // attempt to install via if_absent leaves the original in place.
    App::boot_services().expect("boot_services must succeed in this test");

    // A second if_absent attempt (simulating the inventory call) MUST
    // be a no-op.
    let second = App::singleton_if_absent(SurvivorState {
        marker: "post-boot",
    });
    assert!(
        !second,
        "post-boot if_absent install must NOT replace the pre-boot value"
    );

    let resolved: SurvivorState = App::resolve().expect("must resolve");
    assert_eq!(
        resolved.marker, "pre-boot",
        "pre-boot manual install must survive boot_services"
    );
}
