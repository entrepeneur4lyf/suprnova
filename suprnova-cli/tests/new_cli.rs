//! #381b — `suprnova new`, run interactively in a non-interactive shell
//! (stdin not a TTY / EOF), reports a clean user-facing error
//! (`ui::error` + exit 1) instead of panicking on the `dialoguer`
//! `.interact*()` unwrap.
//!
//! Closes the new.rs portion of the Codex final-review "Low: CLI commands
//! still panic instead of returning user-facing errors". The fix matches the
//! convention new.rs already uses for its fatal paths (the
//! validate-project-name / create-project failures): `ui::error` +
//! `std::process::exit(1)`.
//!
//! Teeth: against the pre-#381b code, the first prompt's
//! `.interact_text().unwrap()` aborted with `thread 'main' panicked at ...`
//! when stdin could not be read. The assertions require a non-zero exit, a
//! human-readable message, AND the absence of `"panicked"`.

use std::process::{Command, Stdio};

use tempfile::tempdir;

const BIN: &str = env!("CARGO_BIN_EXE_suprnova");

#[test]
fn new_with_unreadable_stdin_reports_clean_error_not_panic() {
    let dir = tempdir().expect("create tempdir");

    // No name argument and no --no-interaction => `new` prompts for the
    // project name; a closed stdin makes that first dialoguer prompt fail.
    let out = Command::new(BIN)
        .arg("new")
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .output()
        .expect("spawn suprnova binary");

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));

    assert_eq!(
        out.status.code(),
        Some(1),
        "a failed prompt must exit 1; output: {text}"
    );
    assert!(
        text.contains("Failed to read the project name"),
        "must print a user-facing prompt error; got: {text}"
    );
    assert!(
        !text.contains("panicked"),
        "the failed prompt must NOT panic; got: {text}"
    );
}
