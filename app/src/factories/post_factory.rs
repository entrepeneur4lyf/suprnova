//! `PostFactory` — dogfood for the `Faker.fake::<T>() + Dummy<Faker>`
//! path on a richer model.
//!
//! Phase 10A T11 polish broadened `Persistable` to cover Eloquent
//! structs directly, so the factory targets `Post` (the user-facing
//! struct) instead of the inner `post::Model`. The `Dummy<Faker>`
//! impl produces runtime values; the macro-emitted `Persistable for
//! Post` bridges through the inner Model on insert, running every
//! cast's `to_storage` along the way.

use suprnova::__fake::rand::Rng;
use suprnova::__fake::{Dummy, Fake, Faker};
use suprnova::Factory;

use crate::models::posts::Post;

impl Dummy<Faker> for Post {
    fn dummy_with_rng<R: Rng + ?Sized>(_: &Faker, rng: &mut R) -> Self {
        // Lorem-style fake content.
        let title: String =
            suprnova::__fake::faker::lorem::en::Sentence(3..7).fake_with_rng(rng);
        let body: String =
            suprnova::__fake::faker::lorem::en::Paragraph(3..6).fake_with_rng(rng);
        // Reference a "user" id in 1..=50 — matches the typical
        // UsersSeeder count.
        let author_id: i64 = (1..=50i64).fake_with_rng(rng);
        let now = chrono::Utc::now();

        Post {
            // PK placeholder — `persist_via_seaorm` flips to `NotSet`.
            id: 0,
            author_id,
            title,
            body,
            is_public: Faker.fake_with_rng::<bool, _>(rng),
            // Runtime shape — the auto-injected `AsDateTime` cast on
            // the macro converts to TEXT (RFC-3339) at the storage
            // boundary.
            created_at: now,
            updated_at: now,
        }
    }
}

pub struct PostFactory;

impl Factory for PostFactory {
    type Model = Post;

    fn definition() -> Post {
        Faker.fake::<Post>()
    }
}
