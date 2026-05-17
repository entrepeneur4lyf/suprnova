//! Factory definitions for the dogfood app — Phase 6A T7.
//!
//! Mirrors Laravel's `database/factories/` directory. Each factory is
//! a zero-sized marker type with an `impl Factory` that knows how to
//! produce a randomized model. The framework's `FactoryBuilder` adds
//! `.count(n).with(...).create_many()` on top.
//!
//! See the per-factory module for the field-by-field generator picks.

pub mod post_factory;
pub mod user_factory;

pub use post_factory::PostFactory;
pub use user_factory::UserFactory;
