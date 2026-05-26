//! Logging — structured `tracing`-based output with env-driven config.
//!
//! Suprnova wraps the `tracing` ecosystem behind a Laravel-shaped
//! `Log::*` facade (added in later tasks). For now this module exposes
//! the configuration shape that drives the global subscriber.

pub mod config;
pub mod init;
pub mod request_id;

pub use config::{LogConfig, LogFormat};
pub use init::init_subscriber;
pub use request_id::{REQUEST_ID, RequestId, RequestIdMiddleware, current_request_id};
