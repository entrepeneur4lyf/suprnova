//! `PostFactory` — dogfood for the `Faker.fake::<T>() + Dummy<Faker>`
//! path on a richer model.
//!
//! Phase 10A T11 migrated `Post` to `#[suprnova::model]`. The factory
//! targets the SeaORM inner Model (`crate::models::posts::post::Model`)
//! so the blanket `Persistable for ModelTrait` impl handles persistence
//! transparently — the same path the pre-T11 dogfood relied on.

use suprnova::__fake::rand::Rng;
use suprnova::__fake::{Dummy, Fake, Faker};
use suprnova::Factory;

use crate::models::posts::post::Model as PostRow;

impl Dummy<Faker> for PostRow {
    fn dummy_with_rng<R: Rng + ?Sized>(_: &Faker, rng: &mut R) -> Self {
        // Lorem-style fake content.
        let title: String =
            suprnova::__fake::faker::lorem::en::Sentence(3..7).fake_with_rng(rng);
        let body: String =
            suprnova::__fake::faker::lorem::en::Paragraph(3..6).fake_with_rng(rng);
        // Reference a "user" id in 1..=50 — matches the typical
        // UsersSeeder count.
        let author_id: i64 = (1..=50i64).fake_with_rng(rng);

        PostRow {
            // PK placeholder — `Persistable` flips to `NotSet`.
            id: 0,
            author_id,
            title,
            body,
            is_public: Faker.fake_with_rng::<bool, _>(rng),
            // AsDateTime storage shape (RFC-3339 string).
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

pub struct PostFactory;

impl Factory for PostFactory {
    type Model = PostRow;

    fn definition() -> PostRow {
        Faker.fake::<PostRow>()
    }
}
