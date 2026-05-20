//! Phase 10B T8 — `MorphTypeEntry` inventory + lookups.
//!
//! Pins:
//! - Every `#[suprnova::model(morph_type = "...")]` struct registers a
//!   `MorphTypeEntry` accessible via `find_morph_type` /
//!   `find_morph_type_by_id` / `morph_types()`.
//! - Plain `#[suprnova::model]` structs without the attribute stay out
//!   of the registry — the test isolating this is the discriminator
//!   that confirms the macro's `morph_type.is_none()` early-return is
//!   correct.
//! - The `fn() -> TypeId` thunk threads through `inventory::submit!`'s
//!   const-initialiser slot without losing identity (`TypeId::of::<T>`
//!   round-trip).

use std::any::TypeId;
use suprnova::{find_morph_type, find_morph_type_by_id, morph_types, MorphTypeEntry};

#[suprnova::model(table = "mr_posts", morph_type = "post")]
pub struct MrPost {
    pub id: i64,
    pub title: String,
}

#[suprnova::model(table = "mr_videos", morph_type = "video")]
pub struct MrVideo {
    pub id: i64,
    pub url: String,
}

#[suprnova::model(table = "mr_no_morph")]
pub struct MrNoMorph {
    pub id: i64,
    pub name: String,
}

#[test]
fn morph_type_registered_for_post() {
    let entry = find_morph_type("post").expect("post morph type should be registered");
    assert_eq!(entry.morph_type, "post");
    assert_eq!(entry.type_name, "MrPost");
    assert_eq!(entry.table, "mr_posts");
}

#[test]
fn morph_type_registered_for_video() {
    let entry = find_morph_type("video").expect("video morph type should be registered");
    assert_eq!(entry.type_name, "MrVideo");
    assert_eq!(entry.table, "mr_videos");
}

#[test]
fn morph_type_not_registered_for_non_morph_models() {
    // MrNoMorph has no `morph_type` attribute — must not appear in the
    // registry. This is the discriminator that confirms the macro's
    // `morph_type.is_none()` early-return is wired correctly.
    assert!(morph_types().all(|e| e.type_name != "MrNoMorph"));
}

#[test]
fn unknown_morph_type_returns_none() {
    assert!(find_morph_type("legacy_thing").is_none());
}

#[test]
fn morph_types_iter_walks_all() {
    let types: Vec<&'static str> = morph_types().map(|e| e.morph_type).collect();
    assert!(types.contains(&"post"));
    assert!(types.contains(&"video"));
}

#[test]
fn morph_type_reverse_lookup_by_type_id() {
    let post_id = TypeId::of::<MrPost>();
    let entry = find_morph_type_by_id(post_id).expect("MrPost should be findable by TypeId");
    assert_eq!(entry.morph_type, "post");
    assert_eq!(entry.type_name, "MrPost");
}

#[test]
fn morph_type_entry_is_copy_and_debug() {
    // The Copy + Debug bounds are surfaced on the public type. Holding
    // both shape obligations under a compile-time + dbg!()-friendly use.
    fn assert_copy<T: Copy>() {}
    fn assert_debug<T: std::fmt::Debug>() {}
    assert_copy::<MorphTypeEntry>();
    assert_debug::<MorphTypeEntry>();
}
