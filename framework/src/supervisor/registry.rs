//! Inventory-based compile-time registry for supervised tasks.
//!
//! Each supervised task registers itself via [`inventory::submit!`] with a
//! [`SupervisorEntry`] that carries a factory function returning a boxed
//! [`Supervisor`]. The registry is collected at link time; no runtime
//! registration calls are needed.
//!
//! # Registration
//!
//! ```rust,ignore
//! use suprnova::supervisor::{SupervisorEntry, Supervisor};
//!
//! inventory::submit!(SupervisorEntry {
//!     factory: || Box::new(MySupervisor),
//! });
//! ```

use super::Supervisor;

/// A compile-time registry entry for a supervised task.
///
/// Submitted via `inventory::submit!` — typically at the bottom of the file
/// that defines the [`Supervisor`] implementation:
///
/// ```rust,ignore
/// inventory::submit!(SupervisorEntry {
///     factory: || Box::new(MyPollerSupervisor),
/// });
/// ```
pub struct SupervisorEntry {
    /// Factory function that constructs a fresh instance of the supervisor.
    /// Called once per `SupervisorRegistry::start_all()` invocation.
    pub factory: fn() -> Box<dyn Supervisor>,
}

inventory::collect!(SupervisorEntry);
