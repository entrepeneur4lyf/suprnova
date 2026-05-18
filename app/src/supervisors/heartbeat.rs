//! LogHeartbeat — a dogfood [`Supervisor`] that emits a periodic INFO trace.
//!
//! In production this would tick every 60 seconds as a steady heartbeat
//! confirming the process is alive. The dogfood interval is 5 seconds so the
//! log is visible quickly in `cargo run --bin app` output.
//!
//! The supervisor never returns normally (`RestartPolicy::Always`), so if it
//! somehow exits it will be restarted by the framework.

use async_trait::async_trait;
use std::time::Duration;
use suprnova::supervisor::{RestartPolicy, Supervisor};
use suprnova::{FrameworkError, SupervisorEntry};

/// Emits `INFO supervisor heartbeat tick` every 5 seconds.
///
/// Replace `Duration::from_secs(5)` with `from_secs(60)` in production.
pub struct LogHeartbeat;

#[async_trait]
impl Supervisor for LogHeartbeat {
    fn name(&self) -> &'static str {
        "heartbeat"
    }

    async fn run(&self) -> Result<(), FrameworkError> {
        loop {
            tracing::info!("supervisor heartbeat tick");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        // Unreachable in normal operation — the loop above never returns.
        // The Always restart policy ensures the supervisor is restarted if
        // this somehow exits (e.g. due to a future code change).
        #[allow(unreachable_code)]
        Ok(())
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
