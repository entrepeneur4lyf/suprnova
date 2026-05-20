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

// ---- pivot::<P>() panic-message diagnostics (T1 quality fix) -------
//
// The macro-emitted `pivot::<P>()` accessor must distinguish the two
// failure modes loudly. A single "load via BelongsToMany::get()"
// message conflated both into one bucket — these tests pin the
// distinct messages so a future regression flips the assertion, not
// the user's debugging experience.

/// Concrete pivot type for the panic tests. Stand-in for what T4 will
/// emit; here it just needs to be a `Send + Sync + 'static` value the
/// `pivot::<P>()` downcast can succeed on (for the contrast case) and
/// fail on (for the wrong-type case).
#[derive(Debug)]
struct BtmRoleUserPivot {
    #[allow(dead_code)]
    assigned_at: i64,
}

/// A different `'static` type — the wrong-type panic uses this to
/// assert the downcast fails when the caller passes the wrong pivot
/// type into `pivot::<P>()`.
#[derive(Debug)]
struct MmTaggable {
    #[allow(dead_code)]
    tag: &'static str,
}

#[test]
#[should_panic(expected = "row has no pivot context; load via `BelongsToMany::get()`")]
fn pivot_panics_when_no_pivot_context() {
    // Row was built without a pivot (find() path, not the m2m loader).
    let u = SmokeUser {
        id: 10,
        name: "Eve".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };
    // Should panic with the no-context message, naming the row type.
    let _: &BtmRoleUserPivot = u.pivot::<BtmRoleUserPivot>();
}

#[test]
#[should_panic(expected = "pivot is not of type")]
fn pivot_panics_with_distinct_message_on_wrong_type() {
    // Row HAS a pivot context — but it's a `BtmRoleUserPivot`. The
    // caller asks for `MmTaggable`. The accessor must NOT direct them
    // to "load via BelongsToMany::get()" (the data is there); it must
    // tell them the requested type is wrong.
    let pivot: Arc<dyn std::any::Any + Send + Sync> =
        Arc::new(BtmRoleUserPivot { assigned_at: 1 });
    let u = SmokeUser {
        id: 11,
        name: "Frank".into(),
        __eager: EagerLoadCache::new(),
        __pivot: Some(pivot),
    };
    let _: &MmTaggable = u.pivot::<MmTaggable>();
}

#[test]
fn pivot_returns_correctly_typed_value() {
    // Positive control: when the type matches, no panic, returns the
    // borrow. Pins the success path against future refactors of the
    // match/downcast structure.
    let pivot: Arc<dyn std::any::Any + Send + Sync> =
        Arc::new(BtmRoleUserPivot { assigned_at: 42 });
    let u = SmokeUser {
        id: 12,
        name: "Gwen".into(),
        __eager: EagerLoadCache::new(),
        __pivot: Some(pivot),
    };
    let p: &BtmRoleUserPivot = u.pivot::<BtmRoleUserPivot>();
    assert_eq!(p.assigned_at, 42);
}
