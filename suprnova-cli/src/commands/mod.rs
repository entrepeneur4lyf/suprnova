pub mod cargo_meta;
pub mod db_sync;
pub mod dev_tls;
pub mod docker_compose;
pub mod docker_init;
pub mod generate_routes;
pub mod generate_types;
pub mod key_generate;
pub mod make_action;
pub mod make_command;
pub mod make_controller;
pub mod make_error;
pub mod make_inertia;
pub mod make_middleware;
pub mod make_migration;
pub mod make_task;
pub mod migrate;
pub mod migrate_fresh;
pub mod migrate_rollback;
pub mod migrate_status;
pub mod new;
pub mod schedule_list;
pub mod schedule_run;
pub mod schedule_work;
pub mod serve;
pub mod ssr_check;
pub mod ssr_start;
pub mod web_run;
pub mod workflow_install;
pub mod workflow_work;

/// Map a `Command::status()` result to `Result<(), String>` with a uniform
/// error shape for the subcommand-spawning CLI entry points.
///
/// `action` is a short human label for the operation ("migrate", "web:run",
/// "schedule:list", ...) used in the error messages.
///
/// `tolerate_interrupt` controls long-running daemon commands (web:run,
/// schedule:work, workflow:work): when set, a child exit code of 130
/// (SIGINT, the Ctrl+C convention) is treated as a clean shutdown rather
/// than an error.
pub(crate) fn interpret_cargo_status(
    spawned: std::io::Result<std::process::ExitStatus>,
    action: &str,
    tolerate_interrupt: bool,
) -> Result<(), String> {
    let status =
        spawned.map_err(|e| format!("Failed to execute `cargo run` for `{action}`: {e}"))?;

    if status.success() {
        return Ok(());
    }

    if tolerate_interrupt && status.code() == Some(130) {
        return Ok(());
    }

    let code = status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string());
    Err(format!("`{action}` failed (exit {code})"))
}

#[cfg(test)]
mod tests {
    use super::interpret_cargo_status;
    use std::io::{Error as IoError, ErrorKind};
    use std::process::Command;

    #[test]
    fn spawn_error_returns_err_with_action_label() {
        // This is the regression that closes the panic→Result conversion:
        // a spawn failure used to .expect("Failed to execute cargo command"),
        // crashing the CLI. Now it surfaces as a structured Err.
        let spawned: std::io::Result<std::process::ExitStatus> =
            Err(IoError::new(ErrorKind::NotFound, "cargo-not-found"));

        let result = interpret_cargo_status(spawned, "migrate", false);
        let err = result.expect_err("spawn failures must be Err, not panic");
        assert!(
            err.contains("migrate"),
            "error should name the action; got: {err}"
        );
        assert!(
            err.contains("cargo-not-found"),
            "error should preserve the underlying io error; got: {err}"
        );
    }

    #[test]
    fn successful_child_is_ok() {
        // `true` exits with code 0 on every POSIX-like host the CLI runs on.
        let status = Command::new("true").status();
        assert!(interpret_cargo_status(status, "migrate", false).is_ok());
    }

    #[test]
    fn nonzero_child_without_tolerance_is_err() {
        // `false` exits non-zero. Without interrupt tolerance, that surfaces.
        let status = Command::new("false").status();
        let err = interpret_cargo_status(status, "migrate", false)
            .expect_err("non-zero child must be Err");
        assert!(err.contains("migrate"), "error should name action: {err}");
    }

    #[test]
    fn nonzero_child_with_tolerance_still_errors_when_not_sigint() {
        // tolerate_interrupt only swallows 130 (SIGINT). A different
        // failure code must still propagate.
        let status = Command::new("false").status();
        assert!(interpret_cargo_status(status, "web:run", true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn sigint_exit_is_tolerated_when_requested() {
        use std::os::unix::process::ExitStatusExt;
        // ExitStatusExt::from_raw expects the wait(2)-encoded status. The
        // low byte is the signal/coredump bits; the next byte is the exit
        // code. Encoding 130 as the exit code → (130 << 8).
        let raw_status = std::process::ExitStatus::from_raw(130 << 8);
        // Verify our encoding produced the bare exit code we expect before
        // we trust the tolerance branch.
        assert_eq!(raw_status.code(), Some(130));

        let spawned: std::io::Result<std::process::ExitStatus> = Ok(raw_status);
        assert!(
            interpret_cargo_status(spawned, "web:run", true).is_ok(),
            "SIGINT (130) must be tolerated for daemon commands"
        );

        let spawned: std::io::Result<std::process::ExitStatus> = Ok(raw_status);
        assert!(
            interpret_cargo_status(spawned, "migrate", false).is_err(),
            "SIGINT must NOT be tolerated when caller did not opt in"
        );
    }
}
