//! End-to-end console binary integration tests.
//!
//! Spawns the compiled `console` binary as a subprocess and asserts
//! on stdout / stderr / exit code. Complements
//! `app/tests/console_greet.rs` (which calls `dispatch_argv`
//! directly) by exercising everything around dispatch — the tokio
//! runtime flavor, env loading, ExitCode translation, and the
//! lazy-bootstrap fast-path for help.

use std::process::Command;
use tempfile::TempDir;

/// Path to the compiled `console` binary. `CARGO_BIN_EXE_console`
/// is set by Cargo at test compile time and points at the freshly
/// built binary for whichever target this test runs in.
const CONSOLE_BIN: &str = env!("CARGO_BIN_EXE_console");

#[test]
fn help_works_without_database_env() {
    // The lazy-bootstrap fast-path means --help should succeed even
    // when DATABASE_URL is unset — clap's parser short-circuits to
    // the help-print path before our bootstrap closure runs.
    // Use a temp dir as CWD so dotenvy can't reload DATABASE_URL
    // from the app's .env file.
    let tmp = TempDir::new().expect("tmpdir");
    let output = Command::new(CONSOLE_BIN)
        .arg("--help")
        .current_dir(tmp.path()) // no .env reachable from here
        .env_remove("DATABASE_URL")
        .output()
        .expect("console binary spawnable");

    assert!(
        output.status.success(),
        "exit code {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("greet"),
        "top-level help lists registered commands; got:\n{stdout}"
    );
    assert!(stdout.contains("db:seed"));
}

#[test]
fn greet_runs_and_prints_to_stdout() {
    let output = Command::new(CONSOLE_BIN)
        .arg("greet")
        .arg("--name")
        .arg("Alice")
        .output()
        .expect("console binary spawnable");

    assert!(output.status.success(), "exit {:?}", output.status.code());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "Hello, Alice!");
}

#[test]
fn greet_loud_flag_uppercases_prefix() {
    let output = Command::new(CONSOLE_BIN)
        .arg("greet")
        .arg("--name")
        .arg("Bob")
        .arg("--loud")
        .output()
        .expect("console binary spawnable");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "HELLO, Bob!");
}

#[test]
fn unknown_command_exits_nonzero_with_clap_formatted_stderr() {
    let output = Command::new(CONSOLE_BIN)
        .arg("does-not-exist")
        .output()
        .expect("console binary spawnable");

    assert!(
        !output.status.success(),
        "unknown command must fail; exit was {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does-not-exist") || stderr.contains("unrecognized"),
        "clap formats an error message naming the bad input; got:\n{stderr}"
    );
    // Single source of stderr — no redundant `error: ...` line
    // following clap's formatted block. The unique substring "error:"
    // appears in clap's own output (e.g. "error: unrecognized
    // subcommand"); we assert it shows up at most once.
    let error_lines: Vec<_> = stderr
        .lines()
        .filter(|l| l.trim_start().starts_with("error:"))
        .collect();
    assert!(
        error_lines.len() <= 1,
        "expected at most one `error:` line (no double-print); got {error_lines:?}"
    );
}

#[test]
fn per_command_help_works_without_database_env() {
    // Use a temp dir as CWD so dotenvy can't reload DATABASE_URL
    // from the app's .env file.
    let tmp = TempDir::new().expect("tmpdir");
    let output = Command::new(CONSOLE_BIN)
        .arg("greet")
        .arg("--help")
        .current_dir(tmp.path()) // no .env reachable from here
        .env_remove("DATABASE_URL")
        .output()
        .expect("console binary spawnable");

    assert!(
        output.status.success(),
        "per-command --help must succeed without DB env"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--name"),
        "per-command help lists the typed clap flags; got:\n{stdout}"
    );
    assert!(stdout.contains("--loud"));
}
