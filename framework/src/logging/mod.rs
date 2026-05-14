//! Logging — structured `tracing`-based output with env-driven config.
//!
//! Suprnova wraps the `tracing` ecosystem behind a Laravel-shaped
//! `Log::*` facade (added in later tasks). For now this module exposes
//! the configuration shape that drives the global subscriber.

pub mod config;
pub mod request_id;

pub use config::{LogConfig, LogFormat};
pub use request_id::{current_request_id, RequestId, RequestIdMiddleware, REQUEST_ID};
