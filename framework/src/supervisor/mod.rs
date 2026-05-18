//! Supervised long-running tasks.
//!
//! A [`Supervisor`] is a user-defined daemon that the framework spawns at boot
//! and keeps alive according to its [`RestartPolicy`]. Typical use cases:
//! background pollers, scheduled aggregators, presence reconcilers, heartbeat
//! emitters — anything that should "always be running" during the lifetime of
//! the process.
//!
//! # Quick start
//!
//! 1. Implement [`Supervisor`] on your struct.
//! 2. Register it via `inventory::submit!` at the bottom of the same file.
//! 3. Call [`SupervisorRegistry::start_all`] once at app boot (the dogfood app
//!    does this in `bootstrap::register()`).
//!
//! ```rust,ignore
//! use async_trait::async_trait;
//! use suprnova::{FrameworkError, supervisor::{RestartPolicy, Supervisor, SupervisorEntry}};
//! use std::time::Duration;
//! use tokio_util::sync::CancellationToken;
//!
//! pub struct MyPoller;
//!
//! #[async_trait]
//! impl Supervisor for MyPoller {
//!     fn name(&self) -> &'static str { "my_poller" }
//!
//!     async fn run(&self, cancel: CancellationToken) -> Result<(), FrameworkError> {
//!         loop {
//!             tokio::select! {
//!                 _ = cancel.cancelled() => return Ok(()),
//!                 _ = tokio::time::sleep(Duration::from_secs(30)) => {
//!                     // do work
//!                 }
//!             }
//!         }
//!     }
//!
//!     fn restart_policy(&self) -> RestartPolicy { RestartPolicy::Always }
//! }
//!
//! inventory::submit!(SupervisorEntry { factory: || Box::new(MyPoller) });
//! ```
//!
//! # Restart policies
//!
//! | Policy | Behaviour |
//! |--------|-----------|
//! | `OnError` (default) | Restart only when `run()` returns `Err`. An `Ok` return means the task finished cleanly — don't restart. |
//! | `Always` | Restart on both `Ok` and `Err`. Use for daemons that should never return. |
//! | `Never` | One-shot. Run once; never restart regardless of outcome. |
//!
//! # Panic handling
//!
//! Each call to `run()` is wrapped in a dedicated `tokio::spawn`. If the task
//! panics, the join handle captures the panic as a `JoinError` and the restart
//! loop treats it as an `Err` — the supervisor is restarted with exponential
//! backoff rather than dying silently.
//!
//! # Backoff
//!
//! Restarts start at 100 ms and double on each subsequent failure, capped at
//! 60 seconds. A successful run (for `Always`) resets nothing — the backoff
//! accumulates across the entire lifetime of the supervisor task.
//!
//! # Shutdown
//!
//! [`SupervisorRegistry::start_all`] initializes a per-process
//! [`SUPERVISOR_TASKS`] JoinSet and a shared [`SUPERVISOR_CANCEL`]
//! CancellationToken. Every supervisor task is spawned into the JoinSet.
//! On Ctrl-C / SIGTERM, `Server::run` cancels the token and drains the
//! JoinSet with a 5-second grace window, then `abort_all` for any
//! stragglers — the same pattern used by the WebSocket task drain.
//!
//! Supervisors that `tokio::select!` on `cancel.cancelled()` exit cleanly
//! within the grace window. Supervisors that ignore the token get aborted
//! after the deadline.

use std::sync::{Arc, OnceLock};
use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use crate::error::FrameworkError;

pub mod registry;
pub use registry::SupervisorEntry;

// ── Per-process statics ───────────────────────────────────────────────────────

/// JoinSet of all active supervisor restart-loop tasks.
///
/// Initialized by [`SupervisorRegistry::start_all`]. `Server::run` drains
/// this on shutdown (5-second grace window + `abort_all` fallback).
static SUPERVISOR_TASKS: OnceLock<TokioMutex<JoinSet<()>>> = OnceLock::new();

/// Shared cancellation token broadcast to every supervisor's `run()`.
///
/// `Server::run` calls `.cancel()` at shutdown; supervisor implementations
/// that `tokio::select!` on `cancel.cancelled()` exit cleanly within the
/// grace window.
static SUPERVISOR_CANCEL: OnceLock<CancellationToken> = OnceLock::new();

// ── Public accessors (used by Server::run) ────────────────────────────────────

/// Returns a reference to the supervisor task JoinSet, if initialized.
///
/// `None` before [`SupervisorRegistry::start_all`] has been called.
pub fn supervisor_tasks() -> Option<&'static TokioMutex<JoinSet<()>>> {
    SUPERVISOR_TASKS.get()
}

/// Returns a reference to the shared cancellation token, if initialized.
///
/// `None` before [`SupervisorRegistry::start_all`] has been called.
pub fn supervisor_cancel_token() -> Option<&'static CancellationToken> {
    SUPERVISOR_CANCEL.get()
}

// ── Trait ────────────────────────────────────────────────────────────────────

/// A framework-managed long-running background task.
///
/// Implement this trait and register your concrete type via
/// `inventory::submit!(SupervisorEntry { factory: || Box::new(MyType) })`.
/// The framework will spawn your `run()` in a restart loop at boot.
///
/// The `cancel` token is shared across all restarts of this supervisor
/// instance. When `Server::run` initiates shutdown it calls `.cancel()`,
/// signalling every running supervisor to stop. Supervisors should
/// `tokio::select!` on `cancel.cancelled()` so they exit cleanly within
/// the 5-second drain window. Supervisors that do not honor the token are
/// aborted by the JoinSet after the deadline.
#[async_trait]
pub trait Supervisor: Send + Sync + 'static {
    /// Human-readable identifier used in log output. Must be `'static`.
    fn name(&self) -> &'static str;

    /// The body of the supervised task.
    ///
    /// Return `Err` on failure (triggers a restart under `OnError` /
    /// `Always` policies). Return `Ok(())` to signal natural completion
    /// (only makes sense for `Never` / `OnError` one-shot supervisors).
    ///
    /// The `cancel` token is cancelled by the framework when the server
    /// shuts down. Use `tokio::select!` to watch it:
    ///
    /// ```rust,ignore
    /// tokio::select! {
    ///     _ = cancel.cancelled() => return Ok(()),
    ///     _ = do_work() => {}
    /// }
    /// ```
    async fn run(&self, cancel: CancellationToken) -> Result<(), FrameworkError>;

    /// How the framework reacts when `run()` returns (or panics).
    ///
    /// Defaults to [`RestartPolicy::OnError`].
    fn restart_policy(&self) -> RestartPolicy {
        RestartPolicy::OnError
    }
}

// ── Policy ───────────────────────────────────────────────────────────────────

/// Controls when a supervisor is restarted after `run()` returns or panics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Restart only when `run()` returns `Err` (or panics).
    /// An `Ok` return means the task finished cleanly — do not restart.
    OnError,
    /// Always restart: on `Err`, on panic, *and* on `Ok`.
    /// Use for daemons that are never expected to return normally.
    Always,
    /// Never restart. Run `run()` exactly once regardless of the outcome.
    Never,
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// Zero-sized handle to the compile-time supervisor registry.
///
/// Use [`SupervisorRegistry::start_all`] at boot to spawn every registered
/// supervisor into its own restart-loop task.
pub struct SupervisorRegistry;

impl SupervisorRegistry {
    /// Spawn every supervisor that was registered via `inventory::submit!` at
    /// compile time.
    ///
    /// Each supervisor runs in its own restart-loop task spawned into the
    /// per-process [`SUPERVISOR_TASKS`] JoinSet. The shared
    /// [`SUPERVISOR_CANCEL`] token is passed into every `run()` call so
    /// supervisors can exit cleanly on shutdown.
    ///
    /// Call this once at application boot, e.g. inside `bootstrap::register`.
    /// Subsequent calls are idempotent — the statics are `OnceLock`s.
    pub async fn start_all() {
        // OnceLock::set silently fails if already initialized — idempotent.
        let _ = SUPERVISOR_TASKS.set(TokioMutex::new(JoinSet::new()));
        let cancel = SUPERVISOR_CANCEL.get_or_init(CancellationToken::new).clone();

        let mut tasks_guard = SUPERVISOR_TASKS.get().unwrap().lock().await;
        for entry in inventory::iter::<SupervisorEntry> {
            let supervisor: Arc<dyn Supervisor> = Arc::from((entry.factory)());
            let name = supervisor.name();
            let cancel_clone = cancel.clone();
            tasks_guard.spawn(run_with_restart(supervisor, cancel_clone));
            tracing::info!(supervisor = name, "supervisor started");
        }
    }
}

// ── Restart loop ─────────────────────────────────────────────────────────────

/// Run the supervisor in a restart loop with exponential backoff.
///
/// Each call to `run()` is wrapped in a fresh `tokio::spawn` so that panics
/// are caught via [`tokio::task::JoinHandle`] instead of propagating to the
/// caller.
///
/// The backoff starts at 100 ms and doubles on each restart, capped at 60 s.
/// Backoff applies on every restart path (both `Err` and `Always`-on-`Ok`).
///
/// The `cancel` token is shared across all restarts. If it is cancelled at
/// the top of the loop (or during the backoff sleep), the restart loop exits
/// immediately without spawning another run.
async fn run_with_restart(supervisor: Arc<dyn Supervisor>, cancel: CancellationToken) {
    let mut backoff_ms: u64 = 100;
    loop {
        let sv = Arc::clone(&supervisor);
        let cancel_for_run = cancel.clone();
        let handle = tokio::spawn(async move { sv.run(cancel_for_run).await });

        let outcome: Result<(), String> = match handle.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{e}")),
            Err(join_err) if join_err.is_panic() => {
                Err(format!("panic: {:?}", join_err))
            }
            Err(join_err) => Err(format!("join error: {:?}", join_err)),
        };

        // Decide whether to restart. Never / OnError+Ok return early here.
        match (supervisor.restart_policy(), &outcome) {
            (RestartPolicy::Never, _) => {
                // One-shot — never restart regardless of outcome.
                return;
            }
            (RestartPolicy::OnError, Ok(())) => {
                // Finished cleanly; don't restart.
                tracing::debug!(
                    supervisor = supervisor.name(),
                    "supervisor finished (OnError policy); not restarting"
                );
                return;
            }
            (RestartPolicy::OnError, Err(e)) | (RestartPolicy::Always, Err(e)) => {
                tracing::error!(
                    supervisor = supervisor.name(),
                    error = %e,
                    backoff_ms,
                    "supervisor errored; restarting after backoff"
                );
            }
            (RestartPolicy::Always, Ok(())) => {
                tracing::warn!(
                    supervisor = supervisor.name(),
                    backoff_ms,
                    "supervisor returned Ok under Always policy; restarting"
                );
            }
        }

        // If cancel fired while run() was executing (or was already set when
        // we reach this point), don't restart — exit cleanly.
        if cancel.is_cancelled() {
            tracing::info!(
                supervisor = supervisor.name(),
                "supervisor shutdown requested; not restarting"
            );
            return;
        }

        // Wait for the backoff delay, but abort early if cancel fires.
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(supervisor = supervisor.name(), "supervisor shutdown during backoff; exiting");
                return;
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
        }
        backoff_ms = (backoff_ms * 2).min(60_000); // cap at 60 s
    }
}

/// Public entry point for integration tests that need to exercise the restart
/// loop directly without going through the inventory registry.
///
/// Passes a fresh [`CancellationToken`] that is never cancelled, so existing
/// tests that don't need cancel-token behaviour continue to work unchanged.
///
/// Exposed as `pub` only because it is needed by `framework/tests/supervisor_lifecycle.rs`.
/// Application code should use [`SupervisorRegistry::start_all`] instead.
pub async fn run_with_restart_for_testing(supervisor: Arc<dyn Supervisor>) {
    let cancel = CancellationToken::new();
    run_with_restart(supervisor, cancel).await
}

/// Variant of [`run_with_restart_for_testing`] that accepts an explicit
/// [`CancellationToken`], enabling tests that verify graceful shutdown.
///
/// Exposed as `pub` only for `framework/tests/supervisor_lifecycle.rs`.
pub async fn run_with_restart_for_testing_with_cancel(
    supervisor: Arc<dyn Supervisor>,
    cancel: CancellationToken,
) {
    run_with_restart(supervisor, cancel).await
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct OneShotSupervisor {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Supervisor for OneShotSupervisor {
        fn name(&self) -> &'static str {
            "one_shot"
        }
        async fn run(&self, _cancel: CancellationToken) -> Result<(), FrameworkError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn restart_policy(&self) -> RestartPolicy {
            RestartPolicy::Never
        }
    }

    #[tokio::test]
    async fn never_policy_runs_exactly_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let sv: Arc<dyn Supervisor> = Arc::new(OneShotSupervisor {
            counter: counter.clone(),
        });
        run_with_restart(sv, CancellationToken::new()).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1, "Never should run exactly once");
    }

    struct PanickingSupervisor {
        counter: Arc<AtomicUsize>,
        max_runs: usize,
    }

    #[async_trait]
    impl Supervisor for PanickingSupervisor {
        fn name(&self) -> &'static str {
            "panicking"
        }
        async fn run(&self, _cancel: CancellationToken) -> Result<(), FrameworkError> {
            let n = self.counter.fetch_add(1, Ordering::SeqCst);
            if n < self.max_runs - 1 {
                panic!("deliberate test panic");
            }
            Ok(())
        }
        fn restart_policy(&self) -> RestartPolicy {
            RestartPolicy::OnError
        }
    }

    #[tokio::test]
    async fn panic_is_caught_and_restarts() {
        let counter = Arc::new(AtomicUsize::new(0));
        let sv: Arc<dyn Supervisor> = Arc::new(PanickingSupervisor {
            counter: counter.clone(),
            max_runs: 2,
        });

        // Wrap in a separate spawn so the panic handling in run_with_restart
        // can work correctly.
        let handle = tokio::spawn(run_with_restart(sv, CancellationToken::new()));

        // Wait generously for 2 runs (1 panic + 1 ok): 100 ms backoff after first.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        handle.abort();

        let count = counter.load(Ordering::SeqCst);
        assert!(count >= 2, "expected >= 2 runs after panic restart; got {count}");
    }
}
