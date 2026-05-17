//! Smoke test: `suprnova make:command <name>` generates a runnable
//! `#[command]` stub at `src/commands/<snake>.rs` and registers the
//! module in `src/commands/mod.rs`.
//!
//! Each test runs in a fresh tempdir with the minimum directory
//! structure the generator expects (`src/commands/` is auto-created
//! when missing).

use std::process::Command;
use tempfile::TempDir;

fn run_make_command(cwd: &std::path::Path, name: &str) {
    let status = Command::new(env!("CARGO_BIN_EXE_suprnova"))
        .arg("make:command")
        .arg(name)
        .current_dir(cwd)
        .status()
        .expect("suprnova binary spawnable");
    assert!(status.success(), "`suprnova make:command {name}` should succeed");
}

fn read(p: impl AsRef<std::path::Path>) -> String {
    std::fs::read_to_string(p.as_ref())
        .unwrap_or_else(|e| panic!("read {}: {e}", p.as_ref().display()))
}

#[test]
fn make_command_with_simple_name_emits_kebab_case() {
    let tmp = TempDir::new().unwrap();
    run_make_command(tmp.path(), "clean-cache");

    let file = tmp.path().join("src/commands/clean_cache.rs");
    assert!(file.exists(), "command file at {}", file.display());

    let content = read(&file);
    assert!(content.contains("#[console(name = \"clean-cache\""));
    assert!(content.contains("pub struct CleanCache"));
    assert!(content.contains("impl TypedCommand for CleanCache"));
    assert!(content.contains("use suprnova::{Command, FrameworkError, TypedCommand};"));

    let mod_content = read(tmp.path().join("src/commands/mod.rs"));
    assert!(mod_content.contains("pub mod clean_cache;"));
}

#[test]
fn make_command_pascal_case_input_becomes_kebab_case() {
    let tmp = TempDir::new().unwrap();
    run_make_command(tmp.path(), "CleanCache");

    let content = read(tmp.path().join("src/commands/clean_cache.rs"));
    assert!(
        content.contains("name = \"clean-cache\""),
        "PascalCase input becomes kebab-case command name: {content}"
    );
    assert!(content.contains("pub struct CleanCache"));
}

#[test]
fn make_command_with_colon_namespace_preserved_verbatim() {
    let tmp = TempDir::new().unwrap();
    run_make_command(tmp.path(), "mail:send");

    let file = tmp.path().join("src/commands/mail_send.rs");
    assert!(file.exists());
    let content = read(&file);
    // Colon namespace preserved exactly as written.
    assert!(content.contains("name = \"mail:send\""));
    assert!(content.contains("pub struct MailSend"));
}

#[test]
fn make_command_creates_commands_dir_if_missing() {
    let tmp = TempDir::new().unwrap();
    // No src/commands/ pre-created. Generator should `mkdir -p`.
    run_make_command(tmp.path(), "greet");

    assert!(tmp.path().join("src/commands").is_dir());
    assert!(tmp.path().join("src/commands/greet.rs").exists());
    assert!(tmp.path().join("src/commands/mod.rs").exists());
}

#[test]
fn make_command_appends_to_existing_mod_rs() {
    let tmp = TempDir::new().unwrap();
    run_make_command(tmp.path(), "first");
    run_make_command(tmp.path(), "second");

    let mod_content = read(tmp.path().join("src/commands/mod.rs"));
    assert!(mod_content.contains("pub mod first;"));
    assert!(mod_content.contains("pub mod second;"));
}
