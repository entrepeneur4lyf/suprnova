//! `PostFactory` — dogfood for the `Faker.fake::<T>() + Dummy<Faker>`
//! path on a richer SeaORM entity.
//!
//! Posts have title/body/is_public/author_id, so this factory shows
//! the `fake`-driven generator picks per field (lorem sentences for
//! title, paragraphs for body, range-pick for the foreign key). We
//! hand-write the `Dummy` impl in THIS file rather than relying on
//! `#[derive(Dummy)]` upstream because the SeaORM entity is
//! auto-generated and we can't add the derive there.
//!
//! For a non-auto-generated model (a hand-written struct), the
//! one-liner `#[derive(Dummy, Factory)]` collapses both the `Dummy`
//! impl and the factory marker into the model itself — see
//! `framework/tests/factory_derive.rs` for that path.

use chrono::Utc;
// Reach fake (which re-exports rand internally) through suprnova so
// the app crate doesn't need a direct fake / rand dep. `bool` is
// generated through `Faker.fake_with_rng` to avoid pinning on the
// specific rand `Rng` trait location, which moved across rand 0.9/0.10.
use suprnova::__fake::rand::Rng;
use suprnova::__fake::{Dummy, Fake, Faker};
use suprnova::Factory;

use crate::models::entities::posts::Model as Post;

impl Dummy<Faker> for Post {
    fn dummy_with_rng<R: Rng + ?Sized>(_: &Faker, rng: &mut R) -> Self {
        let now = Utc::now().to_rfc3339();
        // Lorem-style fake content.
        let title: String =
            suprnova::__fake::faker::lorem::en::Sentence(3..7).fake_with_rng(rng);
        let body: String =
            suprnova::__fake::faker::lorem::en::Paragraph(3..6).fake_with_rng(rng);
        // Reference a "user" id in 1..=50 — matches the typical
        // UsersSeeder count. A foreign-key seeded factory is the
        // pattern Laravel calls `Relationship factories`; this is
        // the simplest version (no enforced FK lookup).
        let author_id: i32 = (1..=50).fake_with_rng(rng);

        Post {
            // PK placeholder — `Persistable` flips to `NotSet`.
            id: 0,
            author_id,
            title,
            body,
            // Generate a bool through Faker rather than rand's `gen_bool`
            // — Faker.fake_with_rng::<bool, _>(rng) picks uniformly random
            // booleans without depending on the unstable Rng method path.
            is_public: Faker.fake_with_rng::<bool, _>(rng),
            created_at: now.clone(),
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
