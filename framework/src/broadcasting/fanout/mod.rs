//! Cross-process broadcasting fanout via sea-streamer.
//!
//! This module is only compiled when the `broadcasting-fanout` feature is
//! enabled. Apps that don't need multi-process fanout depend on `suprnova`
//! without this feature and pay no sea-streamer cost.
//!
//! # Usage
//!
//! ```toml
//! # Cargo.toml
//! suprnova = { version = "...", features = ["broadcasting-fanout"] }
//! ```
//!
//! ```rust,no_run
//! use suprnova::broadcasting::fanout::SeaStreamerBroadcastHub;
//! use suprnova::broadcasting::BroadcastHub;
//! use std::sync::Arc;
//!
//! # async fn ex() {
//! let hub = Arc::new(
//!     SeaStreamerBroadcastHub::new("stdio://", "my-app-broadcast")
//!         .await
//!         .expect("connect"),
//! );
//! // Register hub in the container so handlers receive it via injection.
//! # }
//! ```

mod sea_streamer;
pub use sea_streamer::SeaStreamerBroadcastHub;
