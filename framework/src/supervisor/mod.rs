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
//!
//! pub struct MyPoller;
//!
//! #[async_trait]
//! impl Supervisor for MyPoller {
//!     fn name(&self) -> &'static str { "my_poller" }
//!
//!     async fn run(&self) -> Result<(), FrameworkError> {
//!         loop {
//!             // do work
//!             tokio::time::sleep(Duration::from_secs(30)).await;
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
//! v1 does not drain supervisors on graceful shutdown. `start_all` detaches
//! every spawned task. Graceful shutdown lands alongside the WebSocket task
//! drain pattern in a future release.

use std::sync::Arc;
use async_trait::async_trait;
use crate::error::FrameworkError;

pub mod registry;
pub use registry::SupervisorEntry;

// ── Trait ────────────────────────────────────────────────────────────────────

/// A framework-managed long-running background task.
///
/// Implement this trait and register your concrete type via
/// `inventory::submit!(SupervisorEntry { factory: || Box::new(MyType) })`.
/// The framework will spawn your `run()` in a restart loop at boot.
#[async_trait]
pub trait Supervisor: Send + Sync + 'static {
    /// Human-readable identifier used in log output. Must be `'static`.
    fn name(&self) -> &'static str;

    /// The body of the supervised task. Return `Err` on failure (triggers a
    /// restart under `OnError` / `Always` policies). Return `Ok(())` to
    /// signal natural completion (only makes sense for `Never` / `OnError`
    /// one-shot supervisors).
    async fn run(&self) -> Result<(), FrameworkError>;

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
    /// Each supervisor runs in its own `tokio::spawn` restart loop. The
    /// spawned tasks are detached — they run for the lifetime of the process
    /// with no handle retained by the caller (v1 does not implement graceful
    /// shutdown for supervisors).
    ///
    /// Call this once at application boot, e.g. inside `bootstrap::register`.
    pub async fn start_all() {
        for entry in inventory::iter::<SupervisorEntry> {
            let supervisor: Arc<dyn Supervisor> = Arc::from((entry.factory)());
            let name = supervisor.name();
            tokio::spawn(run_with_restart(supervisor));
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
async fn run_with_restart(supervisor: Arc<dyn Supervisor>) {
    let mut backoff_ms: u64 = 100;
    loop {
        let sv = Arc::clone(&supervisor);
        let handle = tokio::spawn(async move { sv.run().await });

        let outcome: Result<(), String> = match handle.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{e}")),
            Err(join_err) if join_err.is_panic() => {
                Err(format!("panic: {:?}", join_err))
            }
            Err(join_err) => Err(format!("join error: {:?}", join_err)),
        };

        match (supervisor.restart_policy(), outcome) {
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
            (RestartPolicy::OnError, Err(ref e)) | (RestartPolicy::Always, Err(ref e)) => {
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

        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(60_000); // cap at 60 s
    }
}

/// Public entry point for integration tests that need to exercise the restart
/// loop directly without going through the inventory registry.
///
/// Exposed as `pub` only because it is needed by `framework/tests/supervisor_lifecycle.rs`.
/// Application code should use [`SupervisorRegistry::start_all`] instead.
pub async fn run_with_restart_for_testing(supervisor: Arc<dyn Supervisor>) {
    run_with_restart(supervisor).await
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
        async fn run(&self) -> Result<(), FrameworkError> {
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
        run_with_restart(sv).await;
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
        async fn run(&self) -> Result<(), FrameworkError> {
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
        let handle = tokio::spawn(run_with_restart(sv));

        // Wait generously for 2 runs (1 panic + 1 ok): 100 ms backoff after first.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        handle.abort();

        let count = counter.load(Ordering::SeqCst);
        assert!(count >= 2, "expected >= 2 runs after panic restart; got {count}");
    }
}
