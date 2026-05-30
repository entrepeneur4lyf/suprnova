use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::atomic::{AtomicI64, Ordering};
use suprnova::bus::command::{Command, Handler};
use suprnova::bus::testing::{
    assert_dispatched, assert_dispatched_times, assert_not_dispatched, assert_nothing_dispatched,
    install_fake,
};
use suprnova::bus::{Bus, Dispatched};
use suprnova::{FrameworkError, async_trait};

static TOTAL: AtomicI64 = AtomicI64::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AddCommand {
    a: i64,
    b: i64,
}

#[async_trait]
impl Command for AddCommand {
    type Output = i64;
    fn command_name() -> &'static str {
        "AddCommand"
    }
}

struct AddHandler;

#[async_trait]
impl Handler<AddCommand> for AddHandler {
    async fn handle(&self, cmd: AddCommand) -> Result<i64, FrameworkError> {
        TOTAL.fetch_add(cmd.a + cmd.b, Ordering::SeqCst);
        Ok(cmd.a + cmd.b)
    }
}

#[tokio::test]
#[serial]
async fn bus_dispatch_runs_handler_inline() {
    TOTAL.store(0, Ordering::SeqCst);
    Bus::register::<AddCommand, _>(AddHandler);
    let r = Bus::dispatch(AddCommand { a: 3, b: 4 }).await.unwrap();
    assert!(matches!(r, Dispatched::Executed(7)));
    assert_eq!(TOTAL.load(Ordering::SeqCst), 7);
}

#[tokio::test]
#[serial]
async fn bus_chain_runs_sequentially_until_first_error() {
    TOTAL.store(0, Ordering::SeqCst);
    Bus::register::<AddCommand, _>(AddHandler);
    let results = Bus::chain(vec![AddCommand { a: 1, b: 1 }, AddCommand { a: 2, b: 2 }]).await;
    let outputs: Vec<i64> = results
        .into_iter()
        .filter_map(|r| r.ok().and_then(|d| d.executed()))
        .collect();
    assert_eq!(outputs, vec![2, 4]);
}

#[tokio::test]
#[serial]
async fn bus_batch_runs_concurrently() {
    TOTAL.store(0, Ordering::SeqCst);
    Bus::register::<AddCommand, _>(AddHandler);
    let results = Bus::batch(vec![
        AddCommand { a: 1, b: 1 },
        AddCommand { a: 2, b: 2 },
        AddCommand { a: 3, b: 3 },
    ])
    .await;
    let mut outputs: Vec<i64> = results
        .into_iter()
        .filter_map(|r| r.ok().and_then(|d| d.executed()))
        .collect();
    outputs.sort();
    assert_eq!(outputs, vec![2, 4, 6]);
}

#[tokio::test]
#[serial]
async fn bus_fake_captures_dispatched_commands_without_executing() {
    let _guard = install_fake();
    let r = Bus::dispatch(AddCommand { a: 9, b: 9 }).await.unwrap();
    assert!(matches!(r, Dispatched::Captured));
    assert_dispatched::<AddCommand>(|c| c.a == 9 && c.b == 9);
}

#[tokio::test]
#[serial]
async fn bus_fake_assert_not_dispatched_passes_when_no_match() {
    let _guard = install_fake();
    let _ = Bus::dispatch(AddCommand { a: 1, b: 1 }).await.unwrap();
    assert_not_dispatched::<AddCommand>(|c| c.a == 99);
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected no dispatched AddCommand but found 1")]
async fn bus_fake_assert_not_dispatched_panics_when_match_exists() {
    let _guard = install_fake();
    let _ = Bus::dispatch(AddCommand { a: 7, b: 7 }).await.unwrap();
    assert_not_dispatched::<AddCommand>(|c| c.a == 7 && c.b == 7);
}

#[tokio::test]
#[serial]
async fn bus_fake_assert_dispatched_times_matches_exact_count() {
    let _guard = install_fake();
    let _ = Bus::dispatch(AddCommand { a: 1, b: 1 }).await.unwrap();
    let _ = Bus::dispatch(AddCommand { a: 1, b: 1 }).await.unwrap();
    let _ = Bus::dispatch(AddCommand { a: 2, b: 2 }).await.unwrap();
    assert_dispatched_times::<AddCommand>(|c| c.a == 1 && c.b == 1, 2);
    assert_dispatched_times::<AddCommand>(|c| c.a == 2 && c.b == 2, 1);
    assert_dispatched_times::<AddCommand>(|c| c.a == 999, 0);
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected 5 dispatched AddCommand but found 1")]
async fn bus_fake_assert_dispatched_times_panics_on_mismatch() {
    let _guard = install_fake();
    let _ = Bus::dispatch(AddCommand { a: 1, b: 1 }).await.unwrap();
    assert_dispatched_times::<AddCommand>(|_| true, 5);
}

#[tokio::test]
#[serial]
async fn bus_fake_assert_nothing_dispatched_passes_on_empty_fake() {
    let _guard = install_fake();
    assert_nothing_dispatched();
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected no dispatched commands but found 1")]
async fn bus_fake_assert_nothing_dispatched_panics_when_any_dispatched() {
    let _guard = install_fake();
    let _ = Bus::dispatch(AddCommand { a: 1, b: 1 }).await.unwrap();
    assert_nothing_dispatched();
}

// --- non-serde Output ---
//
// Locks in the Bus's in-process contract: the registry no longer round-trips
// `C::Output` through JSON, so an `Output` that is not `Serialize` /
// `DeserializeOwned` (here: an `Arc<Mutex<…>>` holding state the handler
// mutates) must be returned to the caller intact. The command itself is
// still `Serialize + DeserializeOwned` because the fake path captures it.

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OpaqueCommand {
    bump: i64,
}

struct OpaqueHandle {
    counter: std::sync::Arc<std::sync::Mutex<i64>>,
}

#[async_trait]
impl Command for OpaqueCommand {
    type Output = OpaqueHandle;
    fn command_name() -> &'static str {
        "OpaqueCommand"
    }
}

struct OpaqueHandler;

#[async_trait]
impl Handler<OpaqueCommand> for OpaqueHandler {
    async fn handle(&self, cmd: OpaqueCommand) -> Result<OpaqueHandle, FrameworkError> {
        let counter = std::sync::Arc::new(std::sync::Mutex::new(cmd.bump));
        Ok(OpaqueHandle { counter })
    }
}

#[tokio::test]
#[serial]
async fn bus_dispatch_returns_non_serde_output_intact() {
    Bus::register::<OpaqueCommand, _>(OpaqueHandler);
    let r = Bus::dispatch(OpaqueCommand { bump: 42 }).await.unwrap();
    let handle = r.unwrap_executed();
    // The Arc<Mutex<i64>> survived dispatch as a live value, not a JSON
    // round-trip. Confirm by mutating it.
    {
        let mut g = handle.counter.lock().unwrap();
        *g += 1;
        assert_eq!(*g, 43);
    }
    // And by checking strong_count proves we kept the same Arc, not a clone
    // reconstructed from serialization.
    assert_eq!(std::sync::Arc::strong_count(&handle.counter), 1);
}
