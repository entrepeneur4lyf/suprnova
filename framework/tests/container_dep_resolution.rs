//! Regression: HIGH audit finding `container` #290 — `#[injectable]`
//! dependency resolution depends on inventory iteration order and
//! panics on missing dependencies.
//!
//! Before the fix, `provider::bootstrap` registered all
//! `SingletonEntry` values in the order yielded by `inventory::iter`,
//! and the macro-generated registration closure called
//! `App::resolve::<DepType>().expect(...)` — so if inventory yielded a
//! consumer before its producer, the boot would panic. Inventory order
//! is implementation-defined, so the problem was order-dependent
//! flakiness.
//!
//! The fix:
//! 1. Macros emit `App::resolve(...).map_err(...)?` rather than
//!    `.expect(...)`, returning the failure to the boot loop instead of
//!    panicking.
//! 2. `register_singletons` runs a fixed-point loop: each pass tries
//!    every still-pending entry; successes drop out; loop until empty
//!    (success) or no progress (Err naming the unresolved entry).
//! 3. `App::boot_services()` returns `Result<(), FrameworkError>` so
//!    boot-time failures propagate cleanly to `Server::from_config`.
//!
//! Test strategy: declare two `#[injectable]` types where Consumer has
//! an `#[inject]` field of type Producer. Boot must resolve both —
//! regardless of which one inventory yields first. We assert both
//! resolve cleanly and the dependency was wired through.

use suprnova::App;
use suprnova_macros::injectable;

/// Producer — no dependencies, just a simple unit struct.
#[injectable]
pub struct DepResolutionProducer;

impl DepResolutionProducer {
    pub fn tag(&self) -> &'static str {
        "produced"
    }
}

/// Consumer — has an `#[inject]` field referring to Producer. The macro
/// generates a registration closure that calls
/// `App::resolve::<DepResolutionProducer>()`. If inventory yields
/// Consumer before Producer, the first pass of the boot loop will Err;
/// the fix's fixed-point loop must retry on the next iteration once
/// Producer has registered.
#[injectable]
pub struct DepResolutionConsumer {
    #[inject]
    producer: DepResolutionProducer,
}

impl DepResolutionConsumer {
    pub fn producer_tag(&self) -> &'static str {
        self.producer.tag()
    }
}

#[test]
fn boot_resolves_injectable_dependencies_regardless_of_inventory_order() {
    // Boot must succeed even if inventory iteration order is
    // unfavourable. The fixed-point loop in `register_singletons`
    // ensures that consumers retry after their producers have
    // registered.
    App::init();
    App::boot_services().expect("boot must resolve dep graph");

    // Both injectables must be resolvable.
    let consumer: DepResolutionConsumer =
        App::resolve().expect("Consumer must be registered post-boot");
    assert_eq!(
        consumer.producer_tag(),
        "produced",
        "Consumer must have received its injected Producer via the boot loop"
    );

    let producer: DepResolutionProducer =
        App::resolve().expect("Producer must be registered post-boot");
    assert_eq!(producer.tag(), "produced");
}

#[test]
fn second_boot_is_idempotent_for_dep_graph() {
    // Boot a second time after the first test — should be a no-op for
    // already-registered singletons (verifies #289's idempotency
    // interlocks with #290's loop: the second pass's `singleton_if_absent`
    // is a no-op, the closure returns Ok, the loop terminates).
    App::init();
    App::boot_services().expect("first boot");
    App::boot_services().expect("second boot must also succeed");

    // Still resolvable.
    let _: DepResolutionConsumer = App::resolve().expect("Consumer still present");
}
