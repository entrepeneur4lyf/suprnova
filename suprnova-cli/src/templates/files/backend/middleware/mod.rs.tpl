//! Application middleware
//!
//! Each middleware has its own dedicated file following the framework convention.

pub mod authenticate;
mod logging;

pub use logging::LoggingMiddleware;
