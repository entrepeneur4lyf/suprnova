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
        tracing::warn!("db:seed: no seeders registered — nothing to run");
        return Ok(());
    }
    seed::run_all().await
}
