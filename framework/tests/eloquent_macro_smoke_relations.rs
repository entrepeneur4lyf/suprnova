//! Phase 10B T1 — smoke test for the macro's relation infrastructure
//! auto-injection.
//!
//! Verifies that every `#[suprnova::model]` struct gains the two
//! private fields the eager loader (T9) and BelongsToMany loader (T4)
//! depend on:
//!
//! - `__eager: EagerLoadCache` — relation-row storage.
//! - `__pivot: Option<Arc<dyn Any + Send + Sync>>` — m2m pivot slot.
//!
//! Both are `#[serde(skip)]` (not surfaced in JSON) and have a
//! `Default` impl (so `From<inner::Model>` / `replicate_with` work
//! without changes at every call site).

use std::sync::Arc;
use suprnova::{model, EagerLoadCache};

#[model(table = "smoke_users", relations = {})]
pub struct SmokeUser {
    pub id: i64,
    pub name: String,
}

#[test]
fn macro_injects_eager_field() {
    let u = SmokeUser {
        id: 1,
        name: "Alice".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };
    assert_eq!(u.id, 1);
    assert!(!u.__eager.has("anything"));
    assert!(u.__pivot.is_none());
}

#[test]
fn macro_injects_pivot_field() {
    // The user can read __pivot directly — it's the storage that
    // `pivot::<P>()` reads from. The accessor lives in T4
    // (BelongsToMany); T1 only guarantees the field exists.
    let u = SmokeUser {
        id: 2,
        name: "Bob".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };
    assert!(u.__pivot.is_none());
}

#[test]
fn default_initialises_eager_and_pivot() {
    // `Default for SmokeUser` was emitted by the macro and must
    // initialise the auto-injected fields the same way the manual
    // literal above does. Without this, factories /
    // `replicate_with` / `from_attrs_unsaved` (which all build via
    // `Self::default()`) would leave the slots uninitialised.
    let u = <SmokeUser as Default>::default();
    assert_eq!(u.id, 0);
    assert!(u.name.is_empty());
    assert!(!u.__eager.has("anything"));
    assert!(u.__pivot.is_none());
}

#[test]
fn serde_skip_on_eager_and_pivot() {
    // `to_json()` walks the struct's Serialize impl which honours
    // `#[serde(skip)]`. The auto-injected fields must NOT surface in
    // JSON — they're framework runtime state, not part of the user-
    // facing API.
    let u = SmokeUser {
        id: 9,
        name: "Carol".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };
    let json = u.to_json();
    assert!(
        json.get("__eager").is_none(),
        "__eager must be #[serde(skip)] — got JSON: {json}",
    );
    assert!(
        json.get("__pivot").is_none(),
        "__pivot must be #[serde(skip)] — got JSON: {json}",
    );
    // Sanity: the real fields ARE present.
    assert_eq!(json["id"], 9);
    assert_eq!(json["name"], "Carol");
}

#[test]
fn pivot_is_arc_any_send_sync() {
    // Sanity: the pivot slot's type must be Arc<dyn Any + Send +
    // Sync> so T4's BelongsToMany loader can stash any pivot model
    // type. We don't have a concrete pivot in T1 — verify the slot
    // accepts an arbitrary Arc<dyn Any>.
    let pivot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(42i32);
    let u = SmokeUser {
        id: 3,
        name: "Dan".into(),
        __eager: EagerLoadCache::new(),
        __pivot: Some(pivot),
    };
    assert!(u.__pivot.is_some());
}
