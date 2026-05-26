//! Seeder trait + registry integration tests.
//!
//! Pins the registry semantics: registration order is preserved
//! across `run_all`, re-registering a name slot in-place (last-write-
//! wins matching the Phase 5B factory registries), errors abort the
//! sweep without rolling back already-run seeders, `tracing::info!`
//! fires per seeder so observability tools can see which seeder is
//! mid-run.
//!
//! All tests are `#[serial]` — the seeder registry is a process-
//! global `RwLock<Option<IndexMap>>`. Each test calls
//! `suprnova::seed::clear()` first to drop any leakage from prior
//! tests in the binary.

use serial_test::serial;
use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::FrameworkError;
use suprnova::async_trait;
use suprnova::seed::{self, Seeder};
use tracing_test::traced_test;

// Each seeder uses its own counter so tests can assert WHICH ran and
// HOW MANY TIMES. Atomics are simpler than wrapping a mutable list in
// a Mutex for the test fixtures.
static A_RAN: AtomicUsize = AtomicUsize::new(0);
static B_RAN: AtomicUsize = AtomicUsize::new(0);
static C_RAN: AtomicUsize = AtomicUsize::new(0);
static FAILS: AtomicUsize = AtomicUsize::new(0);

fn reset_all() {
    seed::clear();
    A_RAN.store(0, Ordering::SeqCst);
    B_RAN.store(0, Ordering::SeqCst);
    C_RAN.store(0, Ordering::SeqCst);
    FAILS.store(0, Ordering::SeqCst);
}

struct SeederA;
#[async_trait]
impl Seeder for SeederA {
    fn name() -> &'static str {
        "SeederA"
    }
    async fn run() -> Result<(), FrameworkError> {
        A_RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct SeederB;
#[async_trait]
impl Seeder for SeederB {
    fn name() -> &'static str {
        "SeederB"
    }
    async fn run() -> Result<(), FrameworkError> {
        B_RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct SeederC;
#[async_trait]
impl Seeder for SeederC {
    fn name() -> &'static str {
        "SeederC"
    }
    async fn run() -> Result<(), FrameworkError> {
        C_RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct FailingSeeder;
#[async_trait]
impl Seeder for FailingSeeder {
    fn name() -> &'static str {
        "FailingSeeder"
    }
    async fn run() -> Result<(), FrameworkError> {
        FAILS.fetch_add(1, Ordering::SeqCst);
        Err(FrameworkError::internal("synthetic seeder failure"))
    }
}

/// Replacement for `SeederB` — registers under the same name to
/// exercise the last-write-wins contract.
struct SeederBStub;
#[async_trait]
impl Seeder for SeederBStub {
    fn name() -> &'static str {
        "SeederB"
    }
    async fn run() -> Result<(), FrameworkError> {
        // Decrement to make the "replacement actually ran, not the
        // original" assertion symmetric.
        B_RAN.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn registered_seeder_runs_via_run_all() {
    reset_all();

    seed::register::<SeederA>();
    seed::run_all().await.unwrap();

    assert_eq!(A_RAN.load(Ordering::SeqCst), 1, "SeederA ran exactly once");
    assert_eq!(seed::count(), 1, "registry has one entry");
}

#[tokio::test]
#[serial]
async fn run_all_runs_seeders_in_registration_order() {
    reset_all();

    // Use a single shared counter to record execution order: each
    // seeder records the counter's value before incrementing.
    static ORDER: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    ORDER.lock().unwrap().clear();

    struct OrderedA;
    #[async_trait]
    impl Seeder for OrderedA {
        fn name() -> &'static str {
            "OrderedA"
        }
        async fn run() -> Result<(), FrameworkError> {
            ORDER.lock().unwrap().push("A");
            Ok(())
        }
    }

    struct OrderedB;
    #[async_trait]
    impl Seeder for OrderedB {
        fn name() -> &'static str {
            "OrderedB"
        }
        async fn run() -> Result<(), FrameworkError> {
            ORDER.lock().unwrap().push("B");
            Ok(())
        }
    }

    struct OrderedC;
    #[async_trait]
    impl Seeder for OrderedC {
        fn name() -> &'static str {
            "OrderedC"
        }
        async fn run() -> Result<(), FrameworkError> {
            ORDER.lock().unwrap().push("C");
            Ok(())
        }
    }

    // Register in declared order: C first, then A, then B. The
    // run_all visit order must match — NOT alphabetical, NOT random.
    seed::register::<OrderedC>();
    seed::register::<OrderedA>();
    seed::register::<OrderedB>();

    seed::run_all().await.unwrap();

    let order: Vec<&'static str> = ORDER.lock().unwrap().clone();
    assert_eq!(
        order,
        vec!["C", "A", "B"],
        "registration order is the execution order"
    );
}

#[tokio::test]
#[serial]
async fn re_registering_same_name_replaces_in_place_last_write_wins() {
    reset_all();

    seed::register::<SeederA>();
    seed::register::<SeederB>(); // Original — increments B_RAN.
    seed::register::<SeederBStub>(); // Replacement — decrements B_RAN.
    seed::register::<SeederC>();

    assert_eq!(seed::count(), 3, "B's slot was overwritten, not duplicated");
    seed::run_all().await.unwrap();

    assert_eq!(A_RAN.load(Ordering::SeqCst), 1);
    assert_eq!(C_RAN.load(Ordering::SeqCst), 1);
    assert_eq!(
        B_RAN.load(Ordering::SeqCst) as i64,
        -1,
        "SeederBStub ran (decrementing) not the original SeederB (which increments)"
    );
}

#[tokio::test]
#[serial]
async fn run_all_aborts_on_first_error_without_rolling_back() {
    reset_all();

    seed::register::<SeederA>();
    seed::register::<FailingSeeder>();
    seed::register::<SeederC>();

    let err = seed::run_all().await.unwrap_err();
    assert!(format!("{err}").contains("synthetic seeder failure"));

    assert_eq!(
        A_RAN.load(Ordering::SeqCst),
        1,
        "SeederA ran (registered first; completed before the failure)"
    );
    assert_eq!(
        FAILS.load(Ordering::SeqCst),
        1,
        "FailingSeeder ran and errored"
    );
    assert_eq!(
        C_RAN.load(Ordering::SeqCst),
        0,
        "SeederC did NOT run — the loop aborts on the first error"
    );
}

#[tokio::test]
#[serial]
#[traced_test]
async fn run_all_logs_each_seeder_at_info_level() {
    reset_all();

    seed::register::<SeederA>();
    seed::register::<SeederB>();
    seed::run_all().await.unwrap();

    assert!(
        logs_contain("running seeder"),
        "info log per seeder must be emitted"
    );
    assert!(logs_contain("SeederA"), "seeder name on the log");
    assert!(logs_contain("SeederB"), "seeder name on the log");
}

#[tokio::test]
#[serial]
async fn clear_resets_the_registry() {
    reset_all();

    seed::register::<SeederA>();
    seed::register::<SeederB>();
    assert_eq!(seed::count(), 2);

    seed::clear();
    assert_eq!(seed::count(), 0);

    // Re-registering after clear works as if from scratch.
    seed::register::<SeederA>();
    assert_eq!(seed::count(), 1);
}
