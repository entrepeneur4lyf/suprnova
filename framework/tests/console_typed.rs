//! Typed console command integration tests.
//!
//! Exercises the `#[derive(Command)]` + `TypedCommand` path
//! end-to-end: clap's `Parser` derive describes the args, our
//! derive macro wires the inventory + adapter, the trait impl
//! provides the body. Tests pin:
//!
//!   - typed args parsed by clap reach the handler as struct fields
//!   - `#[arg]` flags (short/long, default values) work as expected
//!   - missing required args yield a clap parse error → dispatch
//!     returns Err
//!   - `<command> --help` prints the per-command help block (clap
//!     auto-generates from the struct + attribute) and returns Ok
//!
//! Tests share three statics that record what the `typed:greet`
//! handler saw on its last run, so they must be `#[serial]`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::sync::Mutex;

use async_trait::async_trait;
use clap::Parser;
use serial_test::serial;
use suprnova::{console, Command, FrameworkError, TypedCommand};

static GREET_RAN: AtomicUsize = AtomicUsize::new(0);
static LAST_GREET_TARGET: OnceLock<Mutex<String>> = OnceLock::new();
static LAST_GREET_LOUD: AtomicUsize = AtomicUsize::new(0); // 0 = unset, 1 = false, 2 = true

fn target_slot() -> &'static Mutex<String> {
    LAST_GREET_TARGET.get_or_init(|| Mutex::new(String::new()))
}

#[derive(Parser, Command, Debug)]
#[console(name = "typed:greet", description = "Greet someone (typed)")]
struct TypedGreet {
    #[arg(short, long, default_value = "world")]
    name: String,

    #[arg(long, default_value_t = false)]
    loud: bool,
}

#[async_trait]
impl TypedCommand for TypedGreet {
    async fn run(self) -> Result<(), FrameworkError> {
        GREET_RAN.fetch_add(1, Ordering::SeqCst);
        *target_slot().lock().unwrap() = self.name;
        LAST_GREET_LOUD.store(if self.loud { 2 } else { 1 }, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Parser, Command, Debug)]
#[console(name = "typed:require", description = "Requires a positional arg")]
struct TypedRequire {
    /// Required positional that has no default — missing it forces
    /// a clap parse error.
    #[arg(value_name = "TARGET")]
    target: String,
}

#[async_trait]
impl TypedCommand for TypedRequire {
    async fn run(self) -> Result<(), FrameworkError> {
        // Body unreachable for the missing-arg test; the parse step
        // fails before this runs.
        Ok(())
    }
}

#[tokio::test]
async fn typed_command_is_registered_via_derive() {
    let entry =
        console::find("typed:greet").expect("derive(Command) auto-registered typed:greet");
    assert_eq!(entry.name, "typed:greet");
    assert_eq!(entry.description, "Greet someone (typed)");
}

#[tokio::test]
#[serial]
async fn typed_command_parses_and_forwards_args() {
    GREET_RAN.store(0, Ordering::SeqCst);
    *target_slot().lock().unwrap() = String::new();
    LAST_GREET_LOUD.store(0, Ordering::SeqCst);

    let argv = vec![
        "console".to_string(),
        "typed:greet".to_string(),
        "--name".to_string(),
        "alice".to_string(),
        "--loud".to_string(),
    ];
    console::dispatch_argv(argv).await.expect("dispatch ok");

    assert_eq!(GREET_RAN.load(Ordering::SeqCst), 1);
    assert_eq!(*target_slot().lock().unwrap(), "alice");
    assert_eq!(
        LAST_GREET_LOUD.load(Ordering::SeqCst),
        2,
        "--loud flag parsed as true"
    );
}

#[tokio::test]
#[serial]
async fn typed_command_uses_clap_defaults_when_args_omitted() {
    GREET_RAN.store(0, Ordering::SeqCst);
    *target_slot().lock().unwrap() = String::new();
    LAST_GREET_LOUD.store(0, Ordering::SeqCst);

    let argv = vec!["console".to_string(), "typed:greet".to_string()];
    console::dispatch_argv(argv).await.expect("dispatch ok");

    assert_eq!(GREET_RAN.load(Ordering::SeqCst), 1);
    assert_eq!(*target_slot().lock().unwrap(), "world");
    assert_eq!(
        LAST_GREET_LOUD.load(Ordering::SeqCst),
        1,
        "--loud absent ⇒ default false"
    );
}

#[tokio::test]
#[serial]
async fn typed_command_uses_short_flag_alias() {
    GREET_RAN.store(0, Ordering::SeqCst);
    *target_slot().lock().unwrap() = String::new();

    let argv = vec![
        "console".to_string(),
        "typed:greet".to_string(),
        "-n".to_string(),
        "bob".to_string(),
    ];
    console::dispatch_argv(argv).await.expect("dispatch ok");

    assert_eq!(*target_slot().lock().unwrap(), "bob");
}

#[tokio::test]
async fn typed_command_missing_required_arg_returns_err() {
    let argv = vec!["console".to_string(), "typed:require".to_string()];
    let err = console::dispatch_argv(argv)
        .await
        .expect_err("missing required positional ⇒ clap parse error ⇒ Err");
    // Clap formatted the error to stderr inside dispatch (the user sees
    // it via the binary). The returned Err is silent so the binary's
    // main doesn't double-print — same contract that
    // `dispatch_returns_err_for_unknown_command` pins in tests/console.rs.
    assert!(
        err.is_silent(),
        "clap-reported missing-arg errors are silent"
    );
}

#[tokio::test]
async fn typed_command_help_flag_returns_ok() {
    // Per-subcommand --help should print and resolve cleanly.
    let argv = vec![
        "console".to_string(),
        "typed:greet".to_string(),
        "--help".to_string(),
    ];
    console::dispatch_argv(argv)
        .await
        .expect("'<typed-cmd> --help' prints help and returns Ok");
}
