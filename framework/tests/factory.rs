//! Factory trait + FactoryBuilder + Sequence integration tests.
//!
//! Pins the Laravel-style fluent surface:
//!   `Factory::new().count(n).with(|m| ...).make_many()`
//!
//! These tests exercise only the in-memory paths. The persisted
//! variants (`create` / `create_many`) need a SeaORM connection and
//! land in a separate test file alongside the `Persistable` work.

use fake::{Fake, Faker};
use suprnova::factory::{Factory, Sequence};

#[derive(Debug, Clone)]
struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub admin: bool,
}

// Inline fake::Dummy implementation so this test file doesn't have
// to derive it — the integration test for the Factory derive macro
// in a later task does the derive-driven path. Here we want the
// trait surface itself under test, not the generator picks.
impl fake::Dummy<Faker> for User {
    fn dummy_with_rng<R: rand::Rng + ?Sized>(_: &Faker, rng: &mut R) -> Self {
        let id: i64 = (1..i64::MAX).fake_with_rng(rng);
        let name: String = fake::faker::name::en::Name().fake_with_rng(rng);
        let email: String = fake::faker::internet::en::SafeEmail().fake_with_rng(rng);
        Self {
            id,
            name,
            email,
            admin: false,
        }
    }
}

struct UserFactory;
impl Factory for UserFactory {
    type Model = User;
    fn definition() -> User {
        Faker.fake::<User>()
    }
}

#[test]
fn factory_make_produces_one_randomized_instance() {
    let user = UserFactory::new().make();
    assert!(!user.name.is_empty(), "name was populated");
    assert!(
        user.email.contains('@'),
        "email looks like an email: {}",
        user.email
    );
    assert!(user.id > 0, "id is positive: {}", user.id);
    assert!(!user.admin, "default value pulled through from Dummy impl");
}

#[test]
fn factory_count_then_make_many_returns_n_independently_random_instances() {
    let users = UserFactory::new().count(50).make_many();
    assert_eq!(users.len(), 50);

    // Independent randomness: emails should be (overwhelmingly) unique.
    // Use 50 instances so a collision is astronomically unlikely under
    // any reasonable fake-crate distribution; assert >= 40 unique so
    // the test is robust to the occasional natural-language collision.
    let unique_emails: std::collections::HashSet<_> =
        users.iter().map(|u| u.email.as_str()).collect();
    assert!(
        unique_emails.len() >= 40,
        "expected mostly-unique emails across 50 instances; got {}",
        unique_emails.len()
    );
}

#[test]
fn factory_with_runs_after_definition_and_clobbers_specified_fields() {
    let user = UserFactory::new()
        .with(|u| u.name = "Alice".into())
        .with(|u| u.admin = true)
        .make();
    assert_eq!(user.name, "Alice");
    assert!(user.admin);
    // Fields not touched by `with` keep their definition-time value.
    assert!(user.email.contains('@'));
}

#[test]
fn factory_with_overrides_compose_in_registration_order() {
    let user = UserFactory::new()
        .with(|u| u.name = "First".into())
        .with(|u| u.name = "Last".into())
        .make();
    assert_eq!(
        user.name, "Last",
        "later overrides clobber earlier ones (registration order matters)"
    );
}

#[test]
fn factory_with_applies_to_every_instance_in_make_many() {
    let users = UserFactory::new()
        .count(5)
        .with(|u| u.admin = true)
        .make_many();
    assert_eq!(users.len(), 5);
    assert!(
        users.iter().all(|u| u.admin),
        "override applied to every instance, not just the first"
    );
}

#[test]
fn sequence_returns_monotonic_values_starting_at_one() {
    let seq = Sequence::new();
    assert_eq!(seq.next(), 1);
    assert_eq!(seq.next(), 2);
    assert_eq!(seq.next(), 3);
}

#[test]
fn sequence_reset_restarts_at_one() {
    let seq = Sequence::new();
    seq.next();
    seq.next();
    seq.next();
    seq.reset();
    assert_eq!(seq.next(), 1, "after reset(), next() returns 1 again");
}

#[test]
fn sequence_under_concurrent_threads_returns_distinct_values() {
    use std::sync::Arc;
    use std::thread;

    // Each of N threads pulls M values; the union must be exactly N*M
    // distinct integers in 1..=N*M.
    const THREADS: usize = 8;
    const PER_THREAD: usize = 1000;

    let seq = Arc::new(Sequence::new());
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let seq = seq.clone();
            thread::spawn(move || (0..PER_THREAD).map(|_| seq.next()).collect::<Vec<_>>())
        })
        .collect();

    let mut all: Vec<i64> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    all.sort_unstable();

    let total = (THREADS * PER_THREAD) as i64;
    assert_eq!(all.len() as i64, total);
    assert_eq!(all[0], 1, "lowest value is 1");
    assert_eq!(all[all.len() - 1], total, "highest value is N*M");
    // No duplicates.
    for w in all.windows(2) {
        assert!(w[0] < w[1], "all values strictly increasing → distinct");
    }
}

/// `Sequence` is intended to be held as a `static` — pin that the
/// `const fn new()` works in const context and the resulting handle
/// is usable across threads via `&'static Sequence`.
#[test]
fn sequence_is_usable_as_a_static() {
    static IDS: Sequence = Sequence::new();
    let a = IDS.next();
    let b = IDS.next();
    assert_eq!(b, a + 1);
    IDS.reset();
    assert_eq!(IDS.next(), 1);
}
