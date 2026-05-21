//! Phase 10C Task 5a — `Collection<T>` generic Laravel surface.
//!
//! Covers the ~25 type-parameter-only methods that work for any `T`
//! (map/filter/reduce/each/group_by_with/chunk/unique/diff/intersect/
//! sort_with/random/...) plus `Deref<Target = [T]>` and the borrowed
//! `IntoIterator`. Model-aware methods (string-keyed pluck/group_by/
//! sort_by/where_eq/sum/avg/min/max) ship in Task 5b on top of this
//! surface.

use std::collections::HashSet;
use suprnova::eloquent::Collection;

// ─── Plan baseline (9 tests) ───────────────────────────────────────────

#[test]
fn collection_empty_methods() {
    let c: Collection<i32> = Collection::new();
    assert!(c.is_empty());
    assert!(!c.is_not_empty());
    assert_eq!(c.len(), 0);
    assert!(c.first().is_none());
    assert!(c.last().is_none());
}

#[test]
fn collection_into_vec_roundtrip() {
    let c = Collection::from_vec(vec![1, 2, 3]);
    let v = c.into_vec();
    assert_eq!(v, vec![1, 2, 3]);
}

#[test]
fn collection_iter_via_deref() {
    let c = Collection::from_vec(vec![1, 2, 3]);
    let sum: i32 = c.iter().sum();
    assert_eq!(sum, 6);
}

#[test]
fn collection_map_filter_reduce() {
    let c = Collection::from_vec(vec![1, 2, 3, 4, 5]);
    let doubled = c.map(|x| x * 2);
    assert_eq!(doubled.into_vec(), vec![2, 4, 6, 8, 10]);

    let c = Collection::from_vec(vec![1, 2, 3, 4, 5]);
    let even = c.filter(|x| x % 2 == 0);
    assert_eq!(even.into_vec(), vec![2, 4]);

    let c = Collection::from_vec(vec![1, 2, 3, 4, 5]);
    let sum = c.reduce(0, |acc, x| acc + x);
    assert_eq!(sum, 15);
}

#[test]
fn collection_each_returns_self_for_chaining() {
    let mut seen = vec![];
    let c = Collection::from_vec(vec![10, 20, 30])
        .each(|x| seen.push(*x))
        .map(|x| x + 1);
    assert_eq!(seen, vec![10, 20, 30]);
    assert_eq!(c.into_vec(), vec![11, 21, 31]);
}

#[test]
fn collection_group_by_with_returns_hashmap_of_collections() {
    let c = Collection::from_vec(vec![1, 2, 3, 4, 5, 6]);
    let groups = c.group_by_with(|n| n % 2);
    assert_eq!(groups.get(&0).unwrap().len(), 3);
    assert_eq!(groups.get(&1).unwrap().len(), 3);
}

#[test]
fn collection_chunk_splits_into_n_size_pieces() {
    let c = Collection::from_vec(vec![1, 2, 3, 4, 5, 6, 7]);
    let chunks = c.chunk(3);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].len(), 3);
    assert_eq!(chunks[1].len(), 3);
    assert_eq!(chunks[2].len(), 1);
    assert_eq!(chunks[2].clone().into_vec(), vec![7]);
}

#[test]
fn collection_unique_dedupes() {
    let c = Collection::from_vec(vec![1, 2, 2, 3, 3, 3, 4]);
    assert_eq!(c.unique().into_vec(), vec![1, 2, 3, 4]);
}

#[test]
fn collection_diff_intersect() {
    let a = Collection::from_vec(vec![1, 2, 3, 4]);
    let b = Collection::from_vec(vec![3, 4, 5, 6]);
    assert_eq!(a.clone().diff(b.clone()).into_vec(), vec![1, 2]);
    assert_eq!(a.intersect(b).into_vec(), vec![3, 4]);
}

// ─── Extended coverage (no method ships untested) ──────────────────────

#[test]
fn collection_from_into_traits() {
    let v = vec![1, 2, 3];
    let c: Collection<i32> = v.clone().into();
    assert_eq!(c.clone().into_vec(), v);

    let back: Vec<i32> = c.into_iter().collect();
    assert_eq!(back, v);
}

#[test]
fn collection_borrowed_into_iter_does_not_consume() {
    let c = Collection::from_vec(vec![1, 2, 3]);
    let sum: i32 = (&c).into_iter().sum();
    assert_eq!(sum, 6);
    // Still usable.
    assert_eq!(c.len(), 3);
}

#[test]
fn collection_first_last_where() {
    let c = Collection::from_vec(vec![1, 2, 3, 4, 5]);
    assert_eq!(c.first_where(|&&x| x > 2), Some(&3));
    assert_eq!(c.last_where(|&&x| x > 2), Some(&5));
    assert!(c.first_where(|&&x| x > 100).is_none());
}

#[test]
fn collection_reject_is_inverse_of_filter() {
    let c = Collection::from_vec(vec![1, 2, 3, 4, 5]);
    assert_eq!(c.reject(|&x| x % 2 == 0).into_vec(), vec![1, 3, 5]);
}

#[test]
fn collection_map_to_map_collects_into_hashmap() {
    let c = Collection::from_vec(vec!["a", "bb", "ccc"]);
    let m = c.map_to_map(|s| (s.len(), s.to_string()));
    assert_eq!(m.get(&1).map(String::as_str), Some("a"));
    assert_eq!(m.get(&2).map(String::as_str), Some("bb"));
    assert_eq!(m.get(&3).map(String::as_str), Some("ccc"));
}

#[test]
fn collection_key_by_with_indexes_by_closure_key() {
    #[derive(Clone, PartialEq, Debug)]
    struct User {
        id: i64,
        name: &'static str,
    }
    let c = Collection::from_vec(vec![
        User { id: 1, name: "a" },
        User { id: 2, name: "b" },
    ]);
    let by_id = c.key_by_with(|u| u.id);
    assert_eq!(by_id.get(&1).unwrap().name, "a");
    assert_eq!(by_id.get(&2).unwrap().name, "b");
}

#[test]
fn collection_pluck_by_borrows_and_extracts() {
    #[derive(Clone)]
    struct Row {
        id: i64,
    }
    let c = Collection::from_vec(vec![Row { id: 10 }, Row { id: 20 }, Row { id: 30 }]);
    let ids: Vec<i64> = c.pluck_by(|r| r.id).into_vec();
    assert_eq!(ids, vec![10, 20, 30]);
    // c untouched — pluck_by takes &self
    assert_eq!(c.len(), 3);
}

#[test]
fn collection_sort_with_orders_in_place_and_returns_self() {
    let c = Collection::from_vec(vec![3, 1, 4, 1, 5, 9, 2, 6]);
    let sorted = c.sort_with(|a, b| a.cmp(b));
    assert_eq!(sorted.into_vec(), vec![1, 1, 2, 3, 4, 5, 6, 9]);
}

#[test]
fn collection_unique_by_uses_closure_key() {
    #[derive(Clone, Debug, PartialEq)]
    struct Row {
        id: i64,
        bucket: i32,
    }
    let c = Collection::from_vec(vec![
        Row { id: 1, bucket: 1 },
        Row { id: 2, bucket: 2 },
        Row { id: 3, bucket: 1 }, // duplicate bucket
        Row { id: 4, bucket: 3 },
    ]);
    let ids: Vec<i64> = c.unique_by(|r| r.bucket).pluck_by(|r| r.id).into_vec();
    assert_eq!(ids, vec![1, 2, 4]);
}

#[test]
fn collection_contains_where_predicate() {
    let c = Collection::from_vec(vec![1, 2, 3]);
    assert!(c.contains_where(|&x| x == 2));
    assert!(!c.contains_where(|&x| x == 99));
}

#[test]
fn collection_take_skip_slice_reverse() {
    let c = Collection::from_vec(vec![1, 2, 3, 4, 5]);
    assert_eq!(c.clone().take(2).into_vec(), vec![1, 2]);
    assert_eq!(c.clone().skip(2).into_vec(), vec![3, 4, 5]);
    assert_eq!(c.clone().slice(1, 3).into_vec(), vec![2, 3, 4]);
    assert_eq!(c.reverse().into_vec(), vec![5, 4, 3, 2, 1]);
}

#[test]
fn collection_take_skip_handle_overshoot() {
    let c = Collection::from_vec(vec![1, 2, 3]);
    assert_eq!(c.clone().take(100).into_vec(), vec![1, 2, 3]);
    assert_eq!(c.skip(100).into_vec(), Vec::<i32>::new());
}

#[test]
fn collection_chunk_zero_returns_empty() {
    let c = Collection::from_vec(vec![1, 2, 3]);
    let chunks = c.chunk(0);
    assert!(chunks.is_empty());
}

#[test]
fn collection_concat_and_merge_are_aliases() {
    let a = Collection::from_vec(vec![1, 2]);
    let b = Collection::from_vec(vec![3, 4]);
    assert_eq!(a.clone().concat(b.clone()).into_vec(), vec![1, 2, 3, 4]);
    assert_eq!(a.merge(b).into_vec(), vec![1, 2, 3, 4]);
}

#[test]
fn collection_shuffle_preserves_multiset() {
    let original = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let c = Collection::from_vec(original.clone());
    let shuffled = c.shuffle().into_vec();
    assert_eq!(shuffled.len(), original.len());
    let shuffled_set: HashSet<i32> = shuffled.into_iter().collect();
    let original_set: HashSet<i32> = original.into_iter().collect();
    assert_eq!(shuffled_set, original_set);
}

#[test]
fn collection_random_picks_an_element_when_non_empty() {
    let c = Collection::from_vec(vec![42; 5]);
    let pick = c.random();
    assert_eq!(pick, Some(&42));

    let empty: Collection<i32> = Collection::new();
    assert!(empty.random().is_none());
}

#[test]
fn collection_random_n_returns_n_elements_from_source() {
    let c = Collection::from_vec(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let picked = c.random_n(3).into_vec();
    assert_eq!(picked.len(), 3);
    let original: HashSet<i32> = (1..=10).collect();
    for v in &picked {
        assert!(original.contains(v), "value {v} not from source");
    }
}

#[test]
fn collection_default_is_empty() {
    let c: Collection<i32> = Collection::default();
    assert!(c.is_empty());
}
