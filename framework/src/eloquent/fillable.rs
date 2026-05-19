//! Mass-assignment guard. Filters an [`Attrs`] map down to only the
//! columns a model is willing to accept via `create` / `update` /
//! `first_or_create` / `update_or_create`.
//!
//! Task 4 ships the runtime primitive and a default that guards the
//! primary-key column only — sufficient to support every CRUD method
//! in this task. Task 6 wires the macro-side `fillable = [...]` /
//! `guarded = [...]` attributes through `fillable_filter()` so users
//! can declare per-model allowlists / denylists.

use crate::eloquent::attrs::Attrs;

/// Per-model mass-assignment guard. The macro emits one of these from
/// `Model::fillable_filter()`; CRUD entry points call
/// [`Fillable::apply`] before building the SeaORM `ActiveModel`.
pub struct Fillable {
    mode: FillableMode,
}

#[derive(Debug, Clone)]
enum FillableMode {
    /// Pass every attribute through unmodified. Used by Task 6 when
    /// the model declares `fillable = ["*"]` or equivalent — Task 4
    /// itself never emits this.
    AllowAll,
    /// Pass only the listed column names through.
    Allowlist(Vec<&'static str>),
    /// Drop the listed column names, pass the rest.
    Denylist(Vec<&'static str>),
}

impl Fillable {
    /// Allow every attribute. Used by tests and by Task 6 once
    /// explicit `fillable = ["*"]` lands.
    pub fn allow_all() -> Self {
        Self {
            mode: FillableMode::AllowAll,
        }
    }

    /// The default Task 4 macro emission denylists `"id"`. Kept as a
    /// convenience for hand-rolled `Model` impls; the macro emits
    /// [`Fillable::guarded`] directly with the parsed primary-key
    /// name so models with `primary_key = "uid"` still have their PK
    /// protected.
    pub fn guarded_default() -> Self {
        Self::guarded(vec!["id"])
    }

    /// Allow only the listed columns. Lint allow on the constructor
    /// name: clippy flags constructors sharing the type name, but in
    /// this case the trait surface (`Fillable::fillable(...)` vs
    /// `Fillable::guarded(...)`) maps directly to Laravel's
    /// `protected $fillable = [...]` / `protected $guarded = [...]`
    /// declarations, which is the documented contract.
    #[allow(clippy::self_named_constructors)]
    pub fn fillable(allowed: Vec<&'static str>) -> Self {
        Self {
            mode: FillableMode::Allowlist(allowed),
        }
    }

    /// Block the listed columns; allow the rest.
    pub fn guarded(blocked: Vec<&'static str>) -> Self {
        Self {
            mode: FillableMode::Denylist(blocked),
        }
    }

    /// Filter an `Attrs` map according to this guard's mode. Returns a
    /// new `Attrs` containing only the columns the guard permits, in
    /// the same insertion order as the input.
    pub fn apply(&self, attrs: Attrs) -> Attrs {
        match &self.mode {
            FillableMode::AllowAll => attrs,
            FillableMode::Allowlist(allowed) => {
                let mut out = Attrs::new();
                for (k, v) in attrs.iter() {
                    if allowed.contains(&k) {
                        out.insert(k, v.clone());
                    }
                }
                out
            }
            FillableMode::Denylist(blocked) => {
                let mut out = Attrs::new();
                for (k, v) in attrs.iter() {
                    if !blocked.contains(&k) {
                        out.insert(k, v.clone());
                    }
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrs;

    #[test]
    fn allow_all_passes_through() {
        let f = Fillable::allow_all();
        let a = attrs! { id: 1, name: "X" };
        let out = f.apply(a);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn allowlist_drops_unlisted() {
        let f = Fillable::fillable(vec!["name"]);
        let a = attrs! { id: 1, name: "X", email: "x@x.com" };
        let out = f.apply(a);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("name"));
    }

    #[test]
    fn denylist_drops_listed() {
        let f = Fillable::guarded(vec!["id"]);
        let a = attrs! { id: 1, name: "X" };
        let out = f.apply(a);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("name"));
        assert!(!out.contains_key("id"));
    }

    #[test]
    fn guarded_default_blocks_id() {
        let f = Fillable::guarded_default();
        let a = attrs! { id: 1, name: "X" };
        let out = f.apply(a);
        assert!(!out.contains_key("id"));
    }
}
