//! Laravel-named facade aliases on [`App`].
//!
//! Laravel exposes `$container->instance($abstract, $instance)` and
//! `$container->bound($abstract)` directly. Suprnova's primary names —
//! `App::singleton` / `App::has` / `App::has_binding` — are idiomatic for
//! Rust, but the Laravel parity sweep also ships the Laravel-named
//! variants so code migrating from a Laravel codebase reads fluently and
//! search-and-replace migrations don't have to translate names.
//!
//! These are pure facade aliases over the existing primitives. The tests
//! pin both halves of the contract:
//! 1. `App::instance(value)` registers a shared singleton that
//!    `App::get::<T>()` can resolve — equivalent to `App::singleton`.
//! 2. `App::bound::<T>()` reports the presence of a concrete singleton —
//!    equivalent to `App::has::<T>()`.
//! 3. `App::bound_binding::<dyn Trait>()` reports the presence of a
//!    trait binding — equivalent to `App::has_binding::<dyn Trait>()`.
//!
//! Each test uses a distinct, test-local marker type so the writes to
//! the process-global `APP_CONTAINER` (which is what `App::instance`
//! targets — `TestContainer::scope` would write to task-local instead)
//! can't collide across tests running in parallel within the same
//! binary. Trait-binding tests run under `TestContainer::scope` to
//! confine the bind to task-local storage.

use std::sync::Arc;
use suprnova::App;
use suprnova::container::testing::TestContainer;

// Distinct types per test so the global-APP_CONTAINER writes can't
// collide across parallel tests in the same binary.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct InstanceMarker {
    value: u32,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct BoundMarker;

#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct DistinctMarker;

trait Greeter: Send + Sync + 'static {
    fn greet(&self) -> &'static str;
}

struct Hello;
impl Greeter for Hello {
    fn greet(&self) -> &'static str {
        "hello"
    }
}

// `greet` is intentionally unused — the bound_binding test only needs
// the trait to be a registrable abstract, not actually invoked.
#[allow(dead_code)]
trait DistinctGreeter: Send + Sync + 'static {
    fn greet(&self) -> &'static str;
}
struct Hola;
impl DistinctGreeter for Hola {
    fn greet(&self) -> &'static str {
        "hola"
    }
}

#[test]
fn instance_registers_a_resolvable_singleton() {
    // App::instance writes to the process-global APP_CONTAINER — same
    // path as App::singleton. We use a marker type unique to this test
    // so the registration cannot bleed into a parallel test.
    App::instance(InstanceMarker { value: 42 });

    let resolved = App::get::<InstanceMarker>().expect("InstanceMarker must be resolvable");
    assert_eq!(
        resolved,
        InstanceMarker { value: 42 },
        "App::instance must register the same shared singleton as App::singleton"
    );
}

#[test]
fn bound_returns_true_after_instance_register_false_otherwise() {
    // Negative case must precede registration. We use a marker type
    // unique to this test so a sibling test's global write cannot
    // satisfy the assertion accidentally.
    assert!(
        !App::bound::<BoundMarker>(),
        "App::bound must return false before registration"
    );

    App::instance(BoundMarker);

    assert!(
        App::bound::<BoundMarker>(),
        "App::bound must return true after instance registration"
    );
}

#[tokio::test]
async fn bound_binding_returns_true_after_bind_false_otherwise() {
    // Trait bindings via TestContainer::bind land on the task-local
    // container, so wrapping in scope() gives the negative-case
    // assertion clean isolation even across parallel runs.
    TestContainer::scope(async {
        assert!(
            !App::bound_binding::<dyn Greeter>(),
            "App::bound_binding must return false before registration"
        );

        TestContainer::bind::<dyn Greeter>(Arc::new(Hello));

        assert!(
            App::bound_binding::<dyn Greeter>(),
            "App::bound_binding must return true after bind"
        );

        // Sanity — the binding is reachable and is the one we installed.
        let g = App::make::<dyn Greeter>().expect("Greeter must resolve");
        assert_eq!(g.greet(), "hello");
    })
    .await;
}

#[tokio::test]
async fn bound_and_bound_binding_address_distinct_storage_pools() {
    // App::bound::<T> queries the concrete-type pool;
    // App::bound_binding::<dyn Trait> queries the trait-binding pool.
    // Registering a concrete must NOT satisfy a trait-binding query for
    // the same impl type, and vice versa — they're keyed under
    // distinct TypeIds (T vs Arc<T>).
    TestContainer::scope(async {
        TestContainer::singleton(DistinctMarker);
        TestContainer::bind::<dyn DistinctGreeter>(Arc::new(Hola));

        assert!(App::bound::<DistinctMarker>());
        assert!(App::bound_binding::<dyn DistinctGreeter>());

        // Concrete-pool query for Hola (the dyn DistinctGreeter impl)
        // must NOT see the trait binding.
        assert!(
            !App::bound::<Hola>(),
            "concrete-pool query for Hola must NOT see the dyn DistinctGreeter binding"
        );
    })
    .await;
}
