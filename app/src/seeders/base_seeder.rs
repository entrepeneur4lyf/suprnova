//! `BaseSeeder` — dogfood for the framework's Seeder trait.
//!
//! Creates 50 users via [`UserFactory`] and 200 posts via
//! [`PostFactory`]. Order matters: posts reference user ids in
//! 1..=50, so users must land first. The framework's `seed::run_all`
//! preserves registration order via `IndexMap`, so as long as
//! `bootstrap.rs` registers `BaseSeeder` once (and BaseSeeder runs
//! its sub-steps in declared sequence), the dependency is satisfied.
//!
//! Combining both factories into a single seeder is the standard
//! Laravel pattern — `DatabaseSeeder::run` orchestrates the per-
//! model seeds. We follow that here rather than expose two
//! independent seeders that the bootstrap must order correctly.

use suprnova::FrameworkError;
use suprnova::async_trait;
use suprnova::{Factory, Seeder};

use crate::factories::{PostFactory, UserFactory};

pub struct BaseSeeder;

#[async_trait]
impl Seeder for BaseSeeder {
    fn name() -> &'static str {
        "BaseSeeder"
    }

    async fn run() -> Result<(), FrameworkError> {
        // 50 users first — the post factory generates author_id in
        // 1..=50, so the references resolve.
        UserFactory::new().count(50).create_many().await?;

        // 200 posts referencing the user ids above.
        PostFactory::new().count(200).create_many().await?;

        Ok(())
    }
}
