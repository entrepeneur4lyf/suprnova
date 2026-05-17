//! Queue subsystem: facade, drivers, envelope, worker.

pub mod envelope;
pub mod job;
pub(crate) mod driver;

pub use envelope::{Envelope, EnvelopeError, CURRENT_SCHEMA_VERSION};
pub use job::{BackoffSchedule, Job};
