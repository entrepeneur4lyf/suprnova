//! Phase 10B Task 1 — `EagerLoadCache` storage type for eager-loaded
//! relation rows + aggregates.

use suprnova::EagerLoadCache;

#[derive(Debug, Clone, PartialEq)]
struct FakePost {
    id: i64,
    title: String,
}

#[test]
fn eager_cache_stores_and_retrieves_by_relation_name() {
    let mut cache = EagerLoadCache::new();
    let posts = vec![
        FakePost {
            id: 1,
            title: "a".into(),
        },
        FakePost {
            id: 2,
            title: "b".into(),
        },
    ];
    cache.set_many("posts", posts.clone());

    let got: &[FakePost] = cache.get_many::<FakePost>("posts");
    assert_eq!(got, posts.as_slice());
}

#[test]
fn eager_cache_stores_option_for_has_one() {
    let mut cache = EagerLoadCache::new();
    cache.set_one(
        "profile",
        Some(FakePost {
            id: 1,
            title: "profile-like".into(),
        }),
    );

    let got: Option<&FakePost> = cache.get_one::<FakePost>("profile");
    assert_eq!(got.unwrap().id, 1);
}

#[test]
fn eager_cache_stores_counts_separately_from_rows() {
    let mut cache = EagerLoadCache::new();
    cache.set_count("posts", 42);

    assert_eq!(cache.get_count("posts"), Some(42));
}

#[test]
fn eager_cache_missing_get_one_returns_none() {
    let cache = EagerLoadCache::new();
    let got: Option<&FakePost> = cache.get_one::<FakePost>("nope");
    assert!(got.is_none());
}

#[test]
#[should_panic(expected = "was not eager-loaded")]
fn eager_cache_get_many_panics_with_clear_message() {
    // Spec: "panics with a clear 'this relation wasn't eager-loaded'
    // message if accessed without a prior with(...)".
    let cache = EagerLoadCache::new();
    let _: &[FakePost] = cache.get_many::<FakePost>("posts");
}

#[test]
fn eager_cache_has_returns_truth() {
    let mut cache = EagerLoadCache::new();
    assert!(!cache.has("posts"));
    cache.set_many(
        "posts",
        vec![FakePost {
            id: 1,
            title: "x".into(),
        }],
    );
    assert!(cache.has("posts"));
}

#[test]
fn eager_cache_clone_is_deep() {
    // Required because models implement Clone — the cache must clone
    // too, not share state.
    let mut a = EagerLoadCache::new();
    a.set_many(
        "posts",
        vec![FakePost {
            id: 1,
            title: "x".into(),
        }],
    );

    let b = a.clone();
    a.set_many(
        "posts",
        vec![FakePost {
            id: 2,
            title: "y".into(),
        }],
    );

    let from_a: &[FakePost] = a.get_many("posts");
    let from_b: &[FakePost] = b.get_many("posts");
    assert_eq!(from_a[0].id, 2);
    assert_eq!(from_b[0].id, 1, "clone must not share state");
}

#[test]
fn eager_cache_stores_aggregate_values() {
    // `with_sum` / `with_avg` go through the type-erased Aggregate
    // cell so both `f64` and `i64` aggregates round-trip without a
    // per-numeric-kind cell variant.
    let mut cache = EagerLoadCache::new();
    cache.set_aggregate::<f64>("orders_sum", 123.45);
    assert_eq!(cache.get_aggregate::<f64>("orders_sum"), Some(&123.45));

    cache.set_aggregate::<i64>("orders_min", 7);
    assert_eq!(cache.get_aggregate::<i64>("orders_min"), Some(&7));

    // Wrong-type read returns None (rather than panicking) so the
    // aggregate kind discriminator on the model side can dispatch
    // cleanly.
    assert_eq!(cache.get_aggregate::<i64>("orders_sum"), None);
}
