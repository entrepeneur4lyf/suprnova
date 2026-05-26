//! `#[command]` macro integration tests.
//!
//! Exercises the attribute macro by registering commands in the test
//! binary, then dispatching to them via `console::dispatch_argv`. This
//! pins the full path: macro expansion → fn-pointer adapter →
//! inventory submission → registry lookup → handler invocation.
//!
//! Inventory entries from this file are scoped to this test binary —
//! they do not pollute other tests in the workspace.

use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::{FrameworkError, command, console};

static HELLO_RAN: AtomicUsize = AtomicUsize::new(0);
static HELLO_LAST_ARG_COUNT: AtomicUsize = AtomicUsize::new(0);
static NO_DESC_RAN: AtomicUsize = AtomicUsize::new(0);
static ECHO_ARGS: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();

fn echo_args() -> &'static std::sync::Mutex<Vec<String>> {
    ECHO_ARGS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

#[command(name = "macro:hello", description = "Increments a counter")]
async fn hello(args: Vec<String>) -> Result<(), FrameworkError> {
    HELLO_RAN.fetch_add(1, Ordering::SeqCst);
    HELLO_LAST_ARG_COUNT.store(args.len(), Ordering::SeqCst);
    Ok(())
}

#[command(name = "macro:no-desc")]
async fn no_desc(_args: Vec<String>) -> Result<(), FrameworkError> {
    NO_DESC_RAN.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[command(name = "macro:echo", description = "Captures its args for the test")]
async fn echo(args: Vec<String>) -> Result<(), FrameworkError> {
    let mut buf = echo_args().lock().unwrap();
    buf.clear();
    buf.extend(args);
    Ok(())
}

#[command(name = "macro:explodes", description = "Always errors")]
async fn explodes(_args: Vec<String>) -> Result<(), FrameworkError> {
    Err(FrameworkError::internal("kaboom"))
}

#[tokio::test]
async fn macro_registers_command_with_provided_name_and_description() {
    let entry = console::find("macro:hello").expect("macro:hello is registered");
    assert_eq!(entry.name, "macro:hello");
    assert_eq!(entry.description, "Increments a counter");
}

#[tokio::test]
async fn macro_omits_description_when_attribute_absent() {
    let entry = console::find("macro:no-desc").expect("macro:no-desc is registered");
    assert_eq!(
        entry.description, "",
        "missing description defaults to empty string"
    );
}

#[tokio::test]
async fn macro_handler_runs_via_dispatch_and_receives_args() {
    HELLO_RAN.store(0, Ordering::SeqCst);
    HELLO_LAST_ARG_COUNT.store(0, Ordering::SeqCst);

    let argv = vec![
        "console".to_string(),
        "macro:hello".to_string(),
        "alice".to_string(),
        "bob".to_string(),
        "carol".to_string(),
    ];
    console::dispatch_argv(argv).await.expect("dispatch ok");

    assert_eq!(HELLO_RAN.load(Ordering::SeqCst), 1);
    assert_eq!(HELLO_LAST_ARG_COUNT.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn macro_forwards_args_in_order() {
    let argv = vec![
        "console".to_string(),
        "macro:echo".to_string(),
        "one".to_string(),
        "two".to_string(),
        "three".to_string(),
    ];
    console::dispatch_argv(argv).await.expect("dispatch ok");

    let captured = echo_args().lock().unwrap().clone();
    assert_eq!(captured, vec!["one", "two", "three"]);
}

#[tokio::test]
async fn macro_propagates_handler_errors() {
    let argv = vec!["console".to_string(), "macro:explodes".to_string()];
    let err = console::dispatch_argv(argv).await.unwrap_err();
    assert!(format!("{err}").contains("kaboom"));
}

#[tokio::test]
async fn macro_function_still_callable_directly() {
    // The macro preserves the original function so unit tests can
    // exercise the handler body without going through dispatch_argv.
    NO_DESC_RAN.store(0, Ordering::SeqCst);
    no_desc(vec![]).await.expect("direct call ok");
    assert_eq!(NO_DESC_RAN.load(Ordering::SeqCst), 1);
}
