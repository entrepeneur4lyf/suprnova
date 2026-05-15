//! Unified data-object surface. Implements the `#[derive(Data)]`
//! companion types: a tri-state `Field<T>` for PATCH endpoints, a
//! `RequestIncludeSet` task-local + middleware for `?include=` runtime
//! lazy resolution, and a default-deny allowlist registry.

mod field;

pub use field::Field;
