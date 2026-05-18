//! Console — runtime CLI dispatch for user commands and framework
//! builtins.
//!
//! Each Suprnova project ships a `console` binary that calls
//! [`dispatch_argv`] after running its `bootstrap::register()`. Every
//! registered command contributes a [`clap::Command`] subcommand to a
//! single parser tree, so per-command `--help`, typed args, value
//! parsing, and error messages all come from clap rather than being
//! reinvented here.
//!
//! Two registration shapes feed the same registry:
//!
//! - `#[command(name = "...", description = "...")]` on an
//!   `async fn(Vec<String>) -> Result<(), FrameworkError>` — the
//!   simple path; clap captures the trailing positional args via
//!   `trailing_var_arg` and hands them to the handler verbatim.
//! - `#[derive(Command)]` on a `clap::Parser`-deriving struct that
//!   implements [`TypedCommand`] — the typed path; clap parses
//!   the struct fields, the dispatcher calls `parsed.run().await`.
//!
//! Why a per-project console binary instead of a global CLI shell-out:
//! a global `suprnova` binary can't statically link user types
//! (seeders, commands, models) without either cargo-running the
//! project (slow, defeats the purpose) or dynamic loading (too much
//! complexity for v1). Per-project console matches Laravel's
//! `php artisan` model — same script, same process, same address
//! space.

use crate::error::FrameworkError;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

pub mod builtins;
mod typed;

pub use typed::TypedCommand;

/// fn-pointer-compatible boxed-future returned by every command
/// handler. Receives the per-subcommand `ArgMatches` clap parsed
/// from argv.
pub type CommandHandler = fn(
    &clap::ArgMatches,
) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send>>;

/// Registry entry submitted by `#[command]` / `#[derive(Command)]`.
/// Each entry carries the invocation name, a human-readable
/// description, a clap subcommand builder, and the boxed-future
/// runner.
pub struct CommandEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub clap_builder: fn() -> clap::Command,
    pub handler: CommandHandler,
}

inventory::collect!(CommandEntry);

/// Version string surfaced via `--version` and in `--help` output.
/// Set once at app boot via [`set_version`]; not set ⇒ clap omits
/// the `--version` flag entirely (typing it errors as an unknown
/// argument, which is the honest behavior when no version was
/// declared).
static VERSION: OnceLock<&'static str> = OnceLock::new();

/// Register the version string the console exposes via `--version`
/// and in its top-level `--help` output. Call once at the start of
/// the app's `console` binary `main`, typically with
/// `env!("CARGO_PKG_VERSION")` so the value reflects the user's
/// project, not the framework.
///
/// Subsequent calls are silently ignored (`OnceLock` semantics) —
/// the first registration wins. Tests and programmatic callers that
/// don't call this just get no `--version` support, which is fine.
pub fn set_version(version: &'static str) {
    let _ = VERSION.set(version);
}

/// Look up a registered command by name.
pub fn find(name: &str) -> Option<&'static CommandEntry> {
    inventory::iter::<CommandEntry>
        .into_iter()
        .find(|entry| entry.name == name)
}

/// All registered commands, sorted alphabetically by name.
pub fn list() -> Vec<&'static CommandEntry> {
    let mut entries: Vec<&'static CommandEntry> =
        inventory::iter::<CommandEntry>.into_iter().collect();
    entries.sort_by_key(|entry| entry.name);
    entries
}

/// Build the top-level `clap::Command` with every registered
/// subcommand attached. Name is the static literal "console" —
/// help output reads "Usage: console <COMMAND>" regardless of where
/// the binary lives on disk. Clap won't accept a runtime-owned
/// `String` here because `clap::builder::Str` only converts from
/// `&'static str` or `Box<str>`, and we'd rather not leak per call.
fn build_root() -> clap::Command {
    let mut root = clap::Command::new("console")
        .about("Suprnova console — per-project command dispatch")
        .arg_required_else_help(true)
        .subcommand_required(false);
    if let Some(v) = VERSION.get() {
        root = root.version(*v);
    }
    for entry in list() {
        root = root.subcommand((entry.clap_builder)());
    }
    root
}

/// Dispatch the process's argv to a registered command. Same as
/// [`dispatch_argv_with_init`] but with a no-op init callback —
/// convenient for tests and programmatic callers that don't need
/// lazy bootstrapping.
pub async fn dispatch_argv(argv: Vec<String>) -> Result<(), FrameworkError> {
    dispatch_argv_with_init(argv, || async {}).await
}

/// Dispatch the process's argv to a registered command, running
/// `lazy_init` between clap's argv parse and the matched handler.
///
/// `lazy_init` runs only when clap matches a real registered
/// subcommand — help, version, missing-subcommand, and parse-error
/// paths all skip it. The typical use is to defer expensive
/// bootstrap (DB connect, queue init, event listener wiring) so
/// `console --help` doesn't require `DATABASE_URL` to be set.
///
/// The full clap tree (every registered subcommand) is built each
/// call; clap then parses argv and routes to the right entry.
/// Help flags (`--help`, `-h`, missing subcommand) are clap's
/// responsibility — handled via `handle_clap_error` which prints
/// formatted output and returns Ok (for help/version) or a silent
/// Err (for parse failures) so `main` doesn't double-print.
pub async fn dispatch_argv_with_init<F, Fut>(
    argv: Vec<String>,
    lazy_init: F,
) -> Result<(), FrameworkError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let root = build_root();
    let matches = match root.try_get_matches_from(argv) {
        Ok(m) => m,
        Err(e) => return handle_clap_error(e),
    };

    if let Some((name, sub_matches)) = matches.subcommand() {
        if let Some(entry) = find(name) {
            lazy_init().await;
            let result = (entry.handler)(sub_matches).await;
            if let Err(ref e) = result && !e.is_silent() {
                eprintln!("error: {}", e.message());
            }
            return result;
        }
        // Unreachable by construction: `build_root()` adds a subcommand
        // for every entry returned by `inventory::iter::<CommandEntry>`,
        // and `find(name)` searches the same iterator. Clap therefore
        // cannot match a name that `find` then misses unless those two
        // call sites disagree about the registry — a contract violation,
        // not a runtime condition. Panic so the breakage surfaces
        // immediately rather than silently exiting non-zero.
        unreachable!(
            "clap matched subcommand '{name}' but the inventory registry has no entry \
             by that name — build_root() and find() are out of sync"
        );
    }

    Ok(())
}

/// Translate a clap parse/help error into the right
/// `Result<(), FrameworkError>` shape. Help-shaped clap errors
/// (`--help`, `--version`, missing-subcommand) print to stdout and
/// resolve to `Ok(())`. Real parse errors print to stderr and
/// resolve to `Err(FrameworkError::internal(...))` so the binary's
/// `main` returns the right exit code.
fn handle_clap_error(err: clap::Error) -> Result<(), FrameworkError> {
    use clap::error::ErrorKind;
    // Clap formats the error / help / version output and writes it
    // to the right stream (stdout for help, stderr for errors). We
    // never let `main` add a redundant second print — for clap-shaped
    // failures the returned Err carries an empty message; the binary
    // skips its own eprintln and just translates to a non-zero
    // ExitCode.
    let _ = err.print();
    match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => Ok(()),
        _ => Err(FrameworkError::silent()),
    }
}

/// Helper for `#[command]` macro expansion — extracts the trailing
/// positional args (clap parsed via `trailing_var_arg`) into a
/// `Vec<String>` for the legacy raw-fn handler shape.
#[doc(hidden)]
pub fn collect_trailing_args(matches: &clap::ArgMatches) -> Vec<String> {
    matches
        .get_many::<String>("__suprnova_trailing_args")
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

/// Helper for `#[command]` macro expansion — builds the clap
/// subcommand for a raw `fn(Vec<String>)` handler. The single
/// trailing-var-arg captures every positional after the command
/// name; `.allow_hyphen_values(true)` lets users pass `-x` style
/// flags through to the handler without clap intercepting them.
#[doc(hidden)]
pub fn raw_clap_builder(name: &'static str, description: &'static str) -> clap::Command {
    clap::Command::new(name)
        .about(description)
        .arg(
            clap::Arg::new("__suprnova_trailing_args")
                .action(clap::ArgAction::Append)
                .num_args(0..)
                .trailing_var_arg(true)
                .allow_hyphen_values(true),
        )
}
