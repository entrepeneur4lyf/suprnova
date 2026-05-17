use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, Ordering};
use suprnova::bus::{Bus, Dispatched};
use suprnova::bus::command::{Command, Handler};
use suprnova::bus::testing::{assert_dispatched, install_fake};
use suprnova::{async_trait, FrameworkError};
use serial_test::serial;

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
    let results = Bus::chain(vec![
        AddCommand { a: 1, b: 1 },
        AddCommand { a: 2, b: 2 },
    ])
    .await;
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
