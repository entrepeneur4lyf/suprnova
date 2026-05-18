//! LogHeartbeat — a dogfood [`Supervisor`] that emits a periodic INFO trace.
//!
//! In production this would tick every 60 seconds as a steady heartbeat
//! confirming the process is alive. The dogfood interval is 5 seconds so the
//! log is visible quickly in `cargo run --bin app` output.
//!
//! The supervisor uses `tokio::select!` on the cancel token so it exits
//! cleanly during graceful shutdown instead of being force-aborted.
//! Without the select the `RestartPolicy::Always` loop would keep the
//! process alive past the 5-second drain deadline.

use async_trait::async_trait;
use std::time::Duration;
use suprnova::supervisor::{RestartPolicy, Supervisor};
use suprnova::{FrameworkError, SupervisorEntry};
use tokio_util::sync::CancellationToken;

/// Emits `INFO supervisor heartbeat tick` every 5 seconds.
///
/// Replace `Duration::from_secs(5)` with `from_secs(60)` in production.
pub struct LogHeartbeat;

#[async_trait]
impl Supervisor for LogHeartbeat {
    fn name(&self) -> &'static str {
        "heartbeat"
    }

    async fn run(&self, cancel: CancellationToken) -> Result<(), FrameworkError> {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("heartbeat shutdown");
                    return Ok(());
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    tracing::info!("supervisor heartbeat tick");
                }
            }
        }
    }

    fn restart_policy(&self) -> RestartPolicy {
        RestartPolicy::Always
    }
}

// Register with the compile-time supervisor registry so
// `SupervisorRegistry::start_all()` picks it up at boot.
// We go through `suprnova::inventory` (the re-exported crate) since `inventory`
// is not a direct dependency of the app crate.
suprnova::inventory::submit!(SupervisorEntry {
    factory: || Box::new(LogHeartbeat),
});
