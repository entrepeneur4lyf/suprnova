//! Console — runtime CLI dispatch for user commands and framework
//! builtins.
//!
//! Each Suprnova project ships a `console` binary that calls
//! [`dispatch_argv`] after running its `bootstrap::register()`. The
//! framework looks up `argv[1]` against an inventory-collected registry
//! of [`CommandEntry`] records, then runs the matching handler.
//!
//! Commands are registered via the [`#[command]`](suprnova_macros::command)
//! attribute on an `async fn(Vec<String>) -> Result<(), FrameworkError>`.
//! Builtin commands like `db:seed` live in [`builtins`] and are submitted
//! by the framework itself.
//!
//! Why a per-project binary instead of a global CLI shell-out: a
//! global `suprnova` binary cannot statically load the user's app
//! types (seeders, commands, models) without either cargo-running the
//! project (slow, defeats the purpose) or dynamic loading (too much
//! complexity for v1). Per-project console matches Laravel's
//! `php artisan` model — same script, same process, same address
//! space.

use crate::error::FrameworkError;
use std::future::Future;
use std::pin::Pin;

pub mod builtins;

/// fn-pointer-compatible boxed-future returned by every command handler.
/// The argument vector contains the trailing argv after `argv[1]` (the
/// command name) — i.e. positional args that the command should parse.
pub type CommandHandler =
    fn(Vec<String>) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send>>;

/// Registry entry submitted by `#[command]`. Each entry carries the
/// invocation name (e.g. `"db:seed"`), a human-readable description,
/// and the boxed-future runner.
pub struct CommandEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub handler: CommandHandler,
}

inventory::collect!(CommandEntry);

/// Look up a registered command by name. Returns the first entry whose
/// `name` matches exactly. With duplicate registrations the order is
/// link-determined — don't rely on it; pick distinct names.
pub fn find(name: &str) -> Option<&'static CommandEntry> {
    inventory::iter::<CommandEntry>
        .into_iter()
        .find(|entry| entry.name == name)
}

/// All registered commands, sorted alphabetically by name. Used by the
/// built-in help output and by tooling that needs to enumerate the
/// registry. Allocates fresh on every call.
pub fn list() -> Vec<&'static CommandEntry> {
    let mut entries: Vec<&'static CommandEntry> =
        inventory::iter::<CommandEntry>.into_iter().collect();
    entries.sort_by_key(|entry| entry.name);
    entries
}

/// Dispatch the process's argv to a registered command. Pass
/// `std::env::args().collect::<Vec<_>>()` from the console binary.
/// `argv[0]` is the binary name (used for help output), `argv[1]` is
/// the command name, and `argv[2..]` are passed to the handler.
///
/// Special cases handled here so individual commands don't have to:
///
/// - empty (`argv.len() < 2`) or `--help` / `-h` / `help` → print the
///   help summary and return `Ok(())`
/// - unknown command → print an error + the available-command list
///   to stderr and return `Err(FrameworkError::internal(...))`
pub async fn dispatch_argv(argv: Vec<String>) -> Result<(), FrameworkError> {
    let binary_name = argv.first().map(String::as_str).unwrap_or("console");
    let cmd = argv.get(1).map(String::as_str).unwrap_or("");

    if cmd.is_empty() || cmd == "--help" || cmd == "-h" || cmd == "help" {
        print_help(binary_name);
        return Ok(());
    }

    match find(cmd) {
        Some(entry) => {
            let args: Vec<String> = argv.into_iter().skip(2).collect();
            (entry.handler)(args).await
        }
        None => {
            eprintln!("error: unknown command '{cmd}'");
            eprintln!();
            print_command_list(&mut std::io::stderr());
            Err(FrameworkError::internal(format!(
                "unknown console command: '{cmd}'"
            )))
        }
    }
}

fn print_help(binary_name: &str) {
    println!("Usage: {binary_name} <command> [args...]");
    println!();
    print_command_list(&mut std::io::stdout());
}

fn print_command_list<W: std::io::Write>(out: &mut W) {
    let entries = list();
    if entries.is_empty() {
        let _ = writeln!(out, "  (no commands registered)");
        return;
    }
    let _ = writeln!(out, "Available commands:");
    let name_width = entries
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0);
    for entry in entries {
        let _ = writeln!(
            out,
            "  {name:width$}  {desc}",
            name = entry.name,
            width = name_width,
            desc = entry.description,
        );
    }
}
