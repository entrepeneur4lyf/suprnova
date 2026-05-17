//! `UserFactory` — dogfood for the framework's Factory trait against
//! the auto-generated SeaORM `User` entity.
//!
//! The User schema is intentionally sparse (id + timestamps) — the
//! application's auth layer carries identity, not the SeaORM row.
//! That's enough to prove:
//!   1. `FactoryBuilder::create` / `create_many` flow through the
//!      blanket `Persistable for ModelTrait` impl
//!   2. SeaORM's auto-increment PK is honored (`NotSet` for `id` so
//!      SQLite assigns)
//!   3. timestamp columns can be populated by the factory
//!
//! We hand-write the `definition()` rather than going through
//! `Faker.fake::<User>()` + a `Dummy` impl because the auto-generated
//! `User` entity file is marked `DO NOT EDIT` — we can't add a
//! `#[derive(Dummy)]` upstream without `db:sync` clobbering it. The
//! manual path is the right pattern for auto-generated entities; the
//! `#[derive(Factory)]` macro is the right pattern for hand-written
//! models.

use chrono::Utc;
use suprnova::Factory;

use crate::models::users::User;

pub struct UserFactory;

impl Factory for UserFactory {
    type Model = User;

    fn definition() -> User {
        let now = Utc::now().to_rfc3339();
        User {
            // `0` here is a placeholder — the framework's blanket
            // `Persistable` impl flips primary-key columns to `NotSet`
            // before inserting so SQLite assigns the real id.
            id: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}
