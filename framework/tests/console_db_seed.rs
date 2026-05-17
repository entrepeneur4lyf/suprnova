//! `db:seed` builtin integration tests.
//!
//! Exercises the framework-provided `db:seed` command via the same
//! public surface a Suprnova app would: `console::dispatch_argv`.
//! Each test mutates the global seeder registry, so they all run
//! `#[serial]`.

use serial_test::serial;
use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::async_trait;
use suprnova::console;
use suprnova::seed::{self, Seeder};
use suprnova::FrameworkError;
use tracing_test::traced_test;

static SEEDER_RAN: AtomicUsize = AtomicUsize::new(0);
static FAILING_RAN: AtomicUsize = AtomicUsize::new(0);

struct RecordingSeeder;
#[async_trait]
impl Seeder for RecordingSeeder {
    fn name() -> &'static str {
        "RecordingSeeder"
    }
    async fn run() -> Result<(), FrameworkError> {
        SEEDER_RAN.fetch_add(1, Ordering::SeqCst);
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
        FAILING_RAN.fetch_add(1, Ordering::SeqCst);
        Err(FrameworkError::internal("FailingSeeder fails on purpose"))
    }
}

#[tokio::test]
#[serial]
async fn db_seed_runs_every_registered_seeder() {
    seed::clear();
    SEEDER_RAN.store(0, Ordering::SeqCst);
    seed::register::<RecordingSeeder>();

    let argv = vec!["console".to_string(), "db:seed".to_string()];
    console::dispatch_argv(argv).await.expect("db:seed succeeds");

    assert_eq!(
        SEEDER_RAN.load(Ordering::SeqCst),
        1,
        "RecordingSeeder ran exactly once via db:seed"
    );
    seed::clear();
}

#[tokio::test]
#[serial]
#[traced_test]
async fn db_seed_on_empty_registry_warns_and_returns_ok() {
    seed::clear();

    let argv = vec!["console".to_string(), "db:seed".to_string()];
    console::dispatch_argv(argv)
        .await
        .expect("empty registry is not an error");

    assert!(
        logs_contain("no seeders registered"),
        "tracing::warn fired on empty registry"
    );
}

#[tokio::test]
#[serial]
async fn db_seed_propagates_seeder_errors() {
    seed::clear();
    FAILING_RAN.store(0, Ordering::SeqCst);
    seed::register::<FailingSeeder>();

    let argv = vec!["console".to_string(), "db:seed".to_string()];
    let err = console::dispatch_argv(argv).await.unwrap_err();

    assert!(
        format!("{err}").contains("FailingSeeder fails on purpose"),
        "seeder error surfaces through dispatch: {err}"
    );
    assert_eq!(FAILING_RAN.load(Ordering::SeqCst), 1);
    seed::clear();
}

#[tokio::test]
async fn db_seed_appears_in_console_registry() {
    // Linking the framework should auto-register the builtin via
    // inventory::submit! — no opt-in step.
    let entry = console::find("db:seed").expect("db:seed registered by the framework");
    assert_eq!(entry.name, "db:seed");
    assert_eq!(entry.description, "Run all registered seeders");
}
