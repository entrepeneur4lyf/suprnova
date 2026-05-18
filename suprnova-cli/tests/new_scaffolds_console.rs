//! Smoke test: `suprnova new` must scaffold a working console binary
//! into both flavors of generated project.
//!
//! Pins:
//!   - `src/bin/console.rs` exists and references
//!     `suprnova::console::dispatch_argv`
//!   - `src/commands/mod.rs` exists (empty stub, ready for
//!     `make:command` to append to)
//!   - `Cargo.toml` declares the `console` `[[bin]]` entry
//!   - `src/lib.rs` declares `pub mod commands;`
//!
//! These are file-shape assertions — we don't try to `cargo build` the
//! scaffolded project because it depends on the released `suprnova-rs`
//! crate from crates.io, which would either pull a stale version or
//! fail offline. The existing dogfood path (app/src/bin/console.rs)
//! already proves the wiring compiles and runs end-to-end against
//! HEAD framework code.

use std::process::Command;
use tempfile::TempDir;

fn run_new(cwd: &std::path::Path, name: &str, args: &[&str]) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_suprnova"));
    cmd.arg("new").arg(name).arg("--no-interaction").arg("--no-git");
    for a in args {
        cmd.arg(a);
    }
    let status = cmd
        .current_dir(cwd)
        .status()
        .expect("suprnova binary spawnable");
    assert!(status.success(), "`suprnova new {name}` should succeed");
}

fn read(p: impl AsRef<std::path::Path>) -> String {
    std::fs::read_to_string(p.as_ref())
        .unwrap_or_else(|e| panic!("read {}: {e}", p.as_ref().display()))
}

#[test]
fn inertia_starter_scaffolds_console_binary_and_commands_dir() {
    let tmp = TempDir::new().unwrap();
    run_new(tmp.path(), "smoke-inertia", &["--frontend", "svelte"]);
    let project = tmp.path().join("smoke-inertia");

    let console = project.join("src/bin/console.rs");
    assert!(console.exists(), "console binary written");
    let console_src = read(&console);
    assert!(
        console_src.contains("suprnova::console::dispatch_argv_with_init"),
        "console uses the lazy-bootstrap form so --help / --version skip DB init"
    );
    assert!(
        console_src.contains("suprnova::console::set_version(env!(\"CARGO_PKG_VERSION\"))"),
        "console registers the user's package version so --version works"
    );
    assert!(console_src.contains("smoke_inertia::bootstrap::register"));
    assert!(console_src.contains("tokio::main(flavor = \"current_thread\")"));

    let commands_mod = project.join("src/commands/mod.rs");
    assert!(commands_mod.exists(), "commands stub written");

    let cargo = read(project.join("Cargo.toml"));
    assert!(
        cargo.contains("name = \"console\""),
        "Cargo.toml declares the console [[bin]]: {cargo}"
    );
    assert!(cargo.contains("path = \"src/bin/console.rs\""));

    let lib = read(project.join("src/lib.rs"));
    assert!(
        lib.contains("pub mod commands;"),
        "lib.rs declares the commands module"
    );
}

#[test]
fn api_starter_scaffolds_console_binary_and_commands_dir() {
    let tmp = TempDir::new().unwrap();
    run_new(tmp.path(), "smoke-api", &["--api"]);
    let project = tmp.path().join("smoke-api");

    let console = project.join("src/bin/console.rs");
    assert!(console.exists(), "api console binary written");
    let console_src = read(&console);
    assert!(
        console_src.contains("suprnova::console::dispatch_argv_with_init"),
        "api console uses the lazy-bootstrap form"
    );
    assert!(
        console_src.contains("suprnova::console::set_version(env!(\"CARGO_PKG_VERSION\"))"),
        "api console registers the user's package version"
    );
    assert!(console_src.contains("smoke_api::bootstrap::register"));

    let commands_mod = project.join("src/commands/mod.rs");
    assert!(commands_mod.exists(), "api commands stub written");

    let cargo = read(project.join("Cargo.toml"));
    assert!(cargo.contains("name = \"console\""));
    assert!(cargo.contains("path = \"src/bin/console.rs\""));

    let lib = read(project.join("src/lib.rs"));
    assert!(lib.contains("pub mod commands;"));
}
