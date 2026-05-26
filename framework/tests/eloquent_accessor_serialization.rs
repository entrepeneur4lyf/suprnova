//! Phase 10B P8 — `#[accessor]` + `appends` + `to_json` integration pins.
//!
//! Phase 10A T8 already covered:
//!   - accessor callable directly (`u.full_name()`)
//!   - accessor appears in `to_json` when listed in `appends = [...]`
//!   - hidden + appends coexist (different keys)
//!   - mutator routes through `fill` / `create` / `first_or_new`
//!   - mutator works on i32 fields (typing-agnostic)
//!
//! This file pins the non-obvious shape decisions that P8 surfaces and
//! that previous tests don't lock in:
//!
//! 1. Accessor present in source but NOT in `appends` → key is absent
//!    from `to_json`. (`appends` is opt-in, not auto-detection.)
//! 2. Multiple accessors with different return types (`String`, `i64`,
//!    nested object) all serialise into `to_json`. Pins that the
//!    `serde_json::to_value(&self.#method())` emission works across
//!    `Serialize` return types, not only `String`.
//! 3. `hidden = ["full_name"]` + `appends = ["full_name"]` → the
//!    accessor STILL appears in `to_json`. `append_inserts` runs after
//!    `filter_apply`, matching Laravel's "$appends always serialises"
//!    semantics. The doc comment on the macro emits this claim — this
//!    test pins it.
//! 4. `visible = ["id"]` + `appends = ["full_name"]` → the accessor
//!    appears even though it isn't in the visible allowlist. Same
//!    bypass mechanism, mirrored for the allowlist path.

use suprnova::{Model, accessor, model};

// ---- Models -------------------------------------------------------------

/// Accessor declared but NOT listed in `appends`. The key must NOT
/// appear in `to_json`.
#[model(table = "p8_no_appends", timestamps = false)]
pub struct P8NoAppends {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
}

impl P8NoAppends {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }
}

/// Multiple accessors with different `Serialize` return types. Pins
/// that the macro's `serde_json::to_value(&self.#method())` emission
/// is generic over return type — it doesn't assume `String`.
#[model(
    table = "p8_multi_types",
    timestamps = false,
    appends = ["display_name", "char_count", "stats"]
)]
pub struct P8MultiTypes {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
}

#[derive(serde::Serialize)]
pub struct P8Stats {
    pub initial_first: char,
    pub initial_last: char,
    pub total_chars: usize,
}

impl P8MultiTypes {
    #[accessor]
    pub fn display_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    #[accessor]
    pub fn char_count(&self) -> i64 {
        (self.first_name.len() + self.last_name.len()) as i64
    }

    #[accessor]
    pub fn stats(&self) -> P8Stats {
        P8Stats {
            initial_first: self.first_name.chars().next().unwrap_or('?'),
            initial_last: self.last_name.chars().next().unwrap_or('?'),
            total_chars: self.first_name.len() + self.last_name.len(),
        }
    }
}

/// `hidden` and `appends` collide on the same key. Per the macro doc
/// comment ("$appends always serialises"), the accessor wins — the
/// hidden filter is applied to the base struct map, then accessor
/// inserts happen afterwards. Pins this resolution.
#[model(
    table = "p8_hidden_collide",
    timestamps = false,
    appends = ["full_name"],
    hidden = ["full_name"]
)]
pub struct P8HiddenCollide {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
}

impl P8HiddenCollide {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }
}

/// `visible` allowlist that doesn't list the accessor. The accessor
/// still appears in `to_json` because `append_inserts` runs after the
/// allowlist filter. Pins the allowlist-bypass behaviour.
#[model(
    table = "p8_visible_bypass",
    timestamps = false,
    appends = ["full_name"],
    visible = ["id"]
)]
pub struct P8VisibleBypass {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
}

impl P8VisibleBypass {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }
}

// ---- Tests --------------------------------------------------------------

#[tokio::test]
async fn accessor_without_appends_does_not_appear_in_to_json() {
    let u = P8NoAppends {
        id: 1,
        first_name: "Alice".into(),
        last_name: "Smith".into(),
        ..Default::default()
    };
    // The method is callable directly — pin that the `#[accessor]`
    // attribute is a pure pass-through.
    assert_eq!(u.full_name(), "Alice Smith");
    let v = u.to_array();
    assert_eq!(v["first_name"], "Alice");
    assert_eq!(v["last_name"], "Smith");
    assert!(
        v.get("full_name").is_none(),
        "full_name accessor must NOT appear in to_json without `appends`"
    );
}

#[tokio::test]
async fn multiple_accessors_with_mixed_return_types_all_serialise() {
    let u = P8MultiTypes {
        id: 1,
        first_name: "Alice".into(),
        last_name: "Smith".into(),
        ..Default::default()
    };
    let v = u.to_array();
    // String accessor.
    assert_eq!(v["display_name"], "Alice Smith");
    // i64 accessor.
    assert_eq!(v["char_count"], 10);
    // Nested-object accessor (struct with `Serialize`).
    assert_eq!(v["stats"]["initial_first"], "A");
    assert_eq!(v["stats"]["initial_last"], "S");
    assert_eq!(v["stats"]["total_chars"], 10);
}

#[tokio::test]
async fn hidden_does_not_suppress_an_appended_accessor() {
    // Per macro doc: "$appends always serialises". `hidden` filter
    // is applied to the base struct map; appended accessors are
    // inserted afterwards, so the accessor wins the collision.
    let u = P8HiddenCollide {
        id: 1,
        first_name: "Alice".into(),
        last_name: "Smith".into(),
        ..Default::default()
    };
    let v = u.to_array();
    assert_eq!(v["first_name"], "Alice");
    assert_eq!(v["last_name"], "Smith");
    assert_eq!(
        v["full_name"], "Alice Smith",
        "appended accessor must appear even when its name is in `hidden`",
    );
}

#[tokio::test]
async fn visible_does_not_suppress_an_appended_accessor() {
    // Same bypass mechanism mirrored for the allowlist path:
    // `visible = ["id"]` drops `first_name` and `last_name` from the
    // base map, but the `full_name` accessor still appears because
    // `append_inserts` runs after the allowlist filter.
    let u = P8VisibleBypass {
        id: 7,
        first_name: "Alice".into(),
        last_name: "Smith".into(),
        ..Default::default()
    };
    let v = u.to_array();
    assert_eq!(v["id"], 7);
    assert!(
        v.get("first_name").is_none(),
        "first_name should be dropped by the visible allowlist",
    );
    assert!(
        v.get("last_name").is_none(),
        "last_name should be dropped by the visible allowlist",
    );
    assert_eq!(
        v["full_name"], "Alice Smith",
        "appended accessor must appear even when not listed in `visible`",
    );
}
