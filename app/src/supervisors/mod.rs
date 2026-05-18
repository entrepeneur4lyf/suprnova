//! Application supervisors — framework-managed long-running background tasks.
//!
//! Each module here defines a [`suprnova::supervisor::Supervisor`] implementation
//! and registers it via `inventory::submit!` so that
//! `SupervisorRegistry::start_all()` (called from `bootstrap::register()`)
//! spawns it at boot.
//!
//! See [`suprnova::supervisor`] for the full trait and restart-policy docs.

pub mod heartbeat;
