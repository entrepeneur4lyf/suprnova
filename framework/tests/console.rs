//! Console registry + dispatch integration tests.
//!
//! Exercises `dispatch_argv` against fixtures registered via raw
//! `inventory::submit!`. Tests pin:
//!   - successful dispatch invokes the matching handler with `argv[2..]`
//!   - unknown command returns an Err whose message names the missing
//!     command
//!   - help/empty argv returns Ok without invoking any handler
//!   - `list()` returns entries sorted by name
//!
//! `inventory` registrations are link-time and cannot be cleared between
//! tests — fixtures here use distinct command names to avoid collisions
//! with other tests in the binary.

use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::console::{self, CommandEntry};
use suprnova::FrameworkError;

static GREET_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static LAST_GREET_ARG_LEN: AtomicUsize = AtomicUsize::new(0);

fn run_test_greet(
    args: Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), FrameworkError>> + Send>> {
    Box::pin(async move {
        GREET_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
        LAST_GREET_ARG_LEN.store(args.len(), Ordering::SeqCst);
        Ok(())
    })
}

fn run_test_fail(
    _args: Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), FrameworkError>> + Send>> {
    Box::pin(async move { Err(FrameworkError::internal("intentional test failure")) })
}

inventory::submit! {
    CommandEntry {
        name: "test:greet",
        description: "fixture: increments a counter",
        handler: run_test_greet,
    }
}

inventory::submit! {
    CommandEntry {
        name: "test:fail",
        description: "fixture: always errors",
        handler: run_test_fail,
    }
}

#[tokio::test]
async fn dispatch_invokes_registered_handler_with_trailing_args() {
    GREET_INVOCATIONS.store(0, Ordering::SeqCst);
    LAST_GREET_ARG_LEN.store(0, Ordering::SeqCst);

    let argv = vec![
        "console".to_string(),
        "test:greet".to_string(),
        "alice".to_string(),
        "bob".to_string(),
    ];

    console::dispatch_argv(argv).await.expect("dispatch ok");

    assert_eq!(GREET_INVOCATIONS.load(Ordering::SeqCst), 1);
    assert_eq!(
        LAST_GREET_ARG_LEN.load(Ordering::SeqCst),
        2,
        "argv[2..] is forwarded to the handler"
    );
}

#[tokio::test]
async fn dispatch_propagates_handler_errors() {
    let argv = vec!["console".to_string(), "test:fail".to_string()];

    let err = console::dispatch_argv(argv).await.unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("intentional test failure"),
        "handler error surfaces through dispatch: {msg}"
    );
}

#[tokio::test]
async fn dispatch_returns_err_for_unknown_command() {
    let argv = vec![
        "console".to_string(),
        "test:does-not-exist".to_string(),
    ];

    let err = console::dispatch_argv(argv).await.unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("unknown console command") && msg.contains("test:does-not-exist"),
        "unknown-command error names the bad command: {msg}"
    );
}

#[tokio::test]
async fn dispatch_with_only_binary_name_prints_help_and_returns_ok() {
    // argv has only argv[0]; dispatch should treat that as help.
    let argv = vec!["console".to_string()];
    console::dispatch_argv(argv)
        .await
        .expect("empty argv prints help and returns Ok");
}

#[tokio::test]
async fn dispatch_with_help_flag_returns_ok() {
    for flag in ["help", "--help", "-h"] {
        let argv = vec!["console".to_string(), flag.to_string()];
        console::dispatch_argv(argv)
            .await
            .unwrap_or_else(|_| panic!("'{flag}' should be treated as help"));
    }
}

#[test]
fn list_returns_entries_sorted_by_name() {
    let entries = console::list();
    let names: Vec<&str> = entries.iter().map(|e| e.name).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        names, sorted,
        "list() must return entries sorted by name for stable help output"
    );
}

#[test]
fn find_locates_registered_command_by_exact_name() {
    let entry = console::find("test:greet").expect("test:greet is registered");
    assert_eq!(entry.name, "test:greet");
    assert_eq!(entry.description, "fixture: increments a counter");
}

#[test]
fn find_returns_none_for_unknown_command() {
    assert!(console::find("nonexistent:command").is_none());
}
