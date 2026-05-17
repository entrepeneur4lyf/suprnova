//! `greet` console command — integration test via `dispatch_argv`.
//!
//! Verifies that:
//!   - linking the app crate's `commands` module triggers the
//!     `#[command]` inventory submission so `console::find("greet")`
//!     resolves
//!   - `dispatch_argv(["console", "greet", ...])` routes correctly
//!   - the underlying fn is still callable directly (`greet(args)`)
//!     because `#[command]` preserves the original function
//!
//! Spawning the actual `console` binary as a subprocess is doable but
//! overkill for what this is — the framework's dispatch path is the
//! contract; the binary just hands argv to it. The framework's own
//! tests cover the binary-equivalent path.

use app::commands::greet::greet;
use suprnova::console;

#[tokio::test]
async fn greet_is_registered_via_inventory() {
    let entry = console::find("greet")
        .expect("greet must be registered when app::commands is linked");
    assert_eq!(entry.name, "greet");
    assert_eq!(entry.description, "Print a friendly greeting");
}

#[tokio::test]
async fn greet_runs_via_dispatch_argv() {
    let argv = vec![
        "console".to_string(),
        "greet".to_string(),
        "Suprnova".to_string(),
    ];
    console::dispatch_argv(argv)
        .await
        .expect("greet succeeds with one arg");
}

#[tokio::test]
async fn greet_handles_zero_one_and_many_args() {
    // Each arity exercises a different match arm in the handler.
    // The function preserved by #[command] is reachable directly so
    // we can assert behavior without capturing stdout from dispatch.
    greet(vec![]).await.expect("zero args returns Ok");
    greet(vec!["Alice".to_string()])
        .await
        .expect("one arg returns Ok");
    greet(vec!["Alice".to_string(), "Bob".to_string()])
        .await
        .expect("two args returns Ok");
    greet(vec![
        "Alice".to_string(),
        "Bob".to_string(),
        "Carol".to_string(),
    ])
    .await
    .expect("three args returns Ok");
}
