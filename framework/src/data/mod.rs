//! Unified data-object surface. Implements the `#[derive(Data)]`
//! companion types: a tri-state `Field<T>` for PATCH endpoints, a
//! `RequestIncludeSet` task-local + middleware for `?include=` runtime
//! lazy resolution, and a default-deny allowlist registry.

mod error;
mod field;
mod include_set;
mod middleware;
pub mod registry;
pub mod route_params;

pub use error::IncludeError;
pub use field::Field;
pub use include_set::{current_include_set, RequestIncludeSet, REQUEST_INCLUDE_SET};
pub use middleware::IncludeMiddleware;
