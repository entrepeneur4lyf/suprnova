//! `db:seed` — runs every registered seeder via
//! [`crate::seed::run_all`].
//!
//! On an empty seeder registry this emits a single
//! `tracing::warn!` and returns `Ok(())` — that's the correct product
//! behavior for "user ran the command before registering anything"
//! and it makes the command safe to invoke from test suites that
//! haven't seeded anything specific.

use crate::error::FrameworkError;
use crate::seed;
use suprnova_macros::command;

#[command(name = "db:seed", description = "Run all registered seeders")]
async fn db_seed(_args: Vec<String>) -> Result<(), FrameworkError> {
    if seed::count() == 0 {
        // Two channels by design: eprintln so the user actually
        // sees feedback in the absence of a configured tracing
        // subscriber; tracing::warn so observability tools still
        // pick it up in production.
        eprintln!("db:seed: no seeders registered — nothing to run");
        tracing::warn!("db:seed: no seeders registered — nothing to run");
        return Ok(());
    }
    seed::run_all().await
}
