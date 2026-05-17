//! Database seeders for the dogfood app — Phase 6A T7.
//!
//! Mirrors Laravel's `database/seeders/` directory. Each seeder is a
//! zero-sized type implementing `Seeder`; the framework's
//! `seed::register` slots them into an ordered process-global
//! registry visited by `seed::run_all`.
//!
//! Phase 6B will add the `suprnova db:seed` console command that
//! drives `run_all` from the CLI; until then, calls into
//! `seed::run_all()` from tests or one-off scripts exercise this
//! surface.

pub mod base_seeder;

pub use base_seeder::BaseSeeder;
