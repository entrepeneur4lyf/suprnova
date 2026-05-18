//! Console registry + dispatch integration tests.
//!
//! Exercises `dispatch_argv` against fixtures registered via raw
//! `inventory::submit!`. Tests pin:
//!   - successful dispatch invokes the matching handler and forwards
//!     argv past the command name as the trailing var arg
//!   - unknown command returns Err that names the missing command
//!   - help / empty argv / `--help` paths return Ok (clap prints the
//!     help text; dispatch resolves cleanly)
//!   - `list()` returns entries sorted by name
//!
//! `inventory` registrations are link-time and cannot be cleared
//! between tests — fixtures here use distinct command names to
//! avoid collisions with other tests in the binary.

use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::console::{self, raw_clap_builder, collect_trailing_args, CommandEntry};
use suprnova::FrameworkError;

static GREET_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static LAST_GREET_ARG_LEN: AtomicUsize = AtomicUsize::new(0);

fn build_test_greet() -> clap::Command {
    raw_clap_builder("test:greet", "fixture: increments a counter")
}

fn run_test_greet(
    matches: &clap::ArgMatches,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), FrameworkError>> + Send>> {
    let args = collect_trailing_args(matches);
    Box::pin(async move {
        GREET_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
        LAST_GREET_ARG_LEN.store(args.len(), Ordering::SeqCst);
        Ok(())
    })
}

fn build_test_fail() -> clap::Command {
    raw_clap_builder("test:fail", "fixture: always errors")
}

fn run_test_fail(
    _matches: &clap::ArgMatches,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), FrameworkError>> + Send>> {
    Box::pin(async move { Err(FrameworkError::internal("intentional test failure")) })
}

inventory::submit! {
    CommandEntry {
        name: "test:greet",
        description: "fixture: increments a counter",
        clap_builder: build_test_greet,
        handler: run_test_greet,
    }
}

inventory::submit! {
    CommandEntry {
        name: "test:fail",
        description: "fixture: always errors",
        clap_builder: build_test_fail,
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
        "argv[2..] is forwarded to the handler via trailing_var_arg"
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

    // Clap printed the formatted error to stderr inside dispatch
    // (with "did you mean ..." suggestions). The returned Err
    // carries no message — main must not double-print.
    assert!(
        err.is_silent(),
        "clap-reported errors are silent so main doesn't double-print"
    );
}

#[tokio::test]
async fn dispatch_with_only_binary_name_returns_ok() {
    // argv has only argv[0]; clap's `arg_required_else_help(true)`
    // prints help to stdout and our handler maps the resulting
    // DisplayHelpOnMissingArgumentOrSubcommand to Ok(()).
    let argv = vec!["console".to_string()];
    console::dispatch_argv(argv)
        .await
        .expect("empty argv prints help and returns Ok");
}

#[tokio::test]
async fn dispatch_with_help_flag_returns_ok() {
    // Clap intercepts `--help` and `-h` at parse time and yields
    // DisplayHelp; our handler treats those as Ok.
    for flag in ["--help", "-h"] {
        let argv = vec!["console".to_string(), flag.to_string()];
        console::dispatch_argv(argv)
            .await
            .unwrap_or_else(|_| panic!("'{flag}' should be treated as help"));
    }
}

#[tokio::test]
async fn dispatch_with_help_subcommand_returns_ok() {
    // Clap auto-generates a `help` subcommand; invoking it prints
    // top-level help and yields DisplayHelp.
    let argv = vec!["console".to_string(), "help".to_string()];
    console::dispatch_argv(argv)
        .await
        .expect("'help' subcommand prints help and returns Ok");
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

#[tokio::test]
async fn dispatch_handler_errors_preserve_message_for_programmatic_callers() {
    // The handler returns Err(FrameworkError::internal("intentional test failure")).
    // Dispatch propagates the Err with its message intact so programmatic
    // callers can inspect. (Stderr-print behavior is covered end-to-end
    // by the binary integration tests in Task 5.)
    let argv = vec!["console".to_string(), "test:fail".to_string()];
    let err = console::dispatch_argv(argv).await.unwrap_err();
    assert_eq!(
        err.message(),
        "intentional test failure",
        "handler error message preserved on the returned Err"
    );
}
