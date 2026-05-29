//! Mass-assignment guard. Filters an [`Attrs`] map down to only the
//! columns a model is willing to accept via `create` / `update` /
//! `first_or_create` / `update_or_create`.
//!
//! Task 4 shipped the runtime primitive and a default that guards the
//! primary-key column only. Task 6 wires the macro-side
//! `fillable = [...]` / `guarded = [...]` attributes through
//! `fillable_filter()` so users can declare per-model allowlists /
//! denylists, plus the [`unguarded`] escape hatch — a task-local scope
//! that bypasses the filter entirely for migrations and seeders.
//!
//! ## Strict mode
//!
//! By default `Fillable` silently drops attributes the guard rejects
//! — the permissive Laravel default. Production APIs often prefer
//! the strict shape: a client overpost or a typo should reject the
//! request, not create / update a row with database defaults. Flip
//! the process-wide knob via [`prevent_silently_discarding_attributes`]
//! at boot:
//!
//! ```rust,ignore
//! use suprnova::eloquent::fillable::prevent_silently_discarding_attributes;
//!
//! prevent_silently_discarding_attributes(true);
//! ```
//!
//! With strict mode on, any `create` / `update` / `first_or_create` /
//! `update_or_create` call whose `Attrs` carries a key the model's
//! `fillable_filter()` would drop returns a
//! `FrameworkError::bad_request("...")` instead of silently
//! discarding it. Mirrors Laravel's `Model::preventSilentlyDiscardingAttributes()`.
//!
//! The flag is process-wide so it can flip during boot before any
//! request lands. Concurrent tests that exercise both modes use the
//! [`unguarded`] task-local OR `serial_test::serial` to avoid
//! interleaving.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::eloquent::attrs::Attrs;
use crate::error::FrameworkError;

tokio::task_local! {
    /// Task-local "filter disabled" flag. When `true`, [`Fillable::apply`]
    /// passes every attribute through regardless of the configured mode.
    /// Set by [`unguarded`] for the duration of a single async scope so
    /// concurrent requests on other tasks are unaffected.
    static UNGUARDED: bool;
}

/// Process-wide knob: when `true`, [`Fillable::apply_checked`] errors
/// instead of silently dropping rejected keys. Mirrors Laravel's
/// `Model::preventSilentlyDiscardingAttributes()`.
static STRICT_DISCARD: AtomicBool = AtomicBool::new(false);

/// Toggle strict-discard mode for the entire process. Pass `true` to
/// reject `create` / `update` payloads that carry keys the per-model
/// mass-assignment guard would drop; pass `false` to restore the
/// silent-drop default. Mirrors Laravel's
/// `Model::preventSilentlyDiscardingAttributes()`.
///
/// Intended to be called once during boot. Concurrent
/// strict-vs-permissive tests serialize on a mutex or use
/// [`unguarded`] for the permissive-side cases.
pub fn prevent_silently_discarding_attributes(strict: bool) {
    STRICT_DISCARD.store(strict, Ordering::SeqCst);
}

/// Inspect the current strict-discard mode. Reads the same
/// [`AtomicBool`] [`prevent_silently_discarding_attributes`] writes.
pub fn preventing_silently_discarding_attributes() -> bool {
    STRICT_DISCARD.load(Ordering::SeqCst)
}

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
    ///
    /// When the calling task is inside an [`unguarded`] scope, the
    /// filter is bypassed and the input is returned unmodified — the
    /// task-local check happens here so every code path through
    /// `Model::create` / `Model::update` honours the escape hatch.
    pub fn apply(&self, attrs: Attrs) -> Attrs {
        if Self::is_unguarded() {
            return attrs;
        }
        self.filter_only(attrs)
    }

    /// Strict-mode variant of [`Self::apply`]. When
    /// [`prevent_silently_discarding_attributes`] has been set,
    /// returns `Err(FrameworkError::bad_request(...))` listing every
    /// key the guard would have dropped; otherwise returns the
    /// filtered map (silent-drop semantics, identical to [`Self::apply`]).
    ///
    /// CRUD entry points on [`Model`](crate::eloquent::Model) call
    /// this path so the strict-discard knob flips behaviour without
    /// per-call wiring. The [`unguarded`] task-local still wins —
    /// inside an `unguarded(|| ...)` scope the filter is bypassed
    /// entirely, strict or not.
    pub fn apply_checked(&self, attrs: Attrs) -> Result<Attrs, FrameworkError> {
        if Self::is_unguarded() {
            return Ok(attrs);
        }
        if !preventing_silently_discarding_attributes() {
            return Ok(self.filter_only(attrs));
        }
        let dropped = self.dropped_keys(&attrs);
        if dropped.is_empty() {
            return Ok(self.filter_only(attrs));
        }
        Err(FrameworkError::bad_request(format!(
            "mass-assignment guard would silently discard attributes: {} \
             (strict mode is active; mark these fields fillable, drop them \
             from the payload, or wrap the call in `suprnova::eloquent::unguarded(|| ...)`)",
            dropped.join(", "),
        )))
    }

    /// The core filter step — no task-local or strict-mode check. The
    /// `apply` and `apply_checked` entrypoints layer their respective
    /// policies on top.
    fn filter_only(&self, attrs: Attrs) -> Attrs {
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

    /// Keys the guard would silently drop from `attrs`. Pure — no
    /// task-local check, no strict-mode read. Used by
    /// [`Self::apply_checked`] to assemble its error message.
    fn dropped_keys(&self, attrs: &Attrs) -> Vec<String> {
        match &self.mode {
            FillableMode::AllowAll => Vec::new(),
            FillableMode::Allowlist(allowed) => attrs
                .iter()
                .filter(|(k, _)| !allowed.contains(k))
                .map(|(k, _)| k.to_string())
                .collect(),
            FillableMode::Denylist(blocked) => attrs
                .iter()
                .filter(|(k, _)| blocked.contains(k))
                .map(|(k, _)| k.to_string())
                .collect(),
        }
    }

    /// Returns `true` if the current task is inside an [`unguarded`]
    /// scope. `try_with` returns `Err` outside the scope (the task-local
    /// is uninitialised) — that's the off state.
    fn is_unguarded() -> bool {
        UNGUARDED.try_with(|b| *b).unwrap_or(false)
    }
}

/// Run `fut` with the mass-assignment guard disabled for the current
/// async task. Equivalent to Laravel's `Model::unguarded(closure)`:
///
/// ```rust,ignore
/// use suprnova::{attrs, eloquent::unguarded};
///
/// let user = unguarded(|| async {
///     // Inside this scope, fillable/guarded are ignored —
///     // every attribute is passed through to the database.
///     User::create(attrs! {
///         name: "boot",
///         email: "boot@x.com",
///         admin: true,
///     })
///     .await
/// })
/// .await?;
/// ```
///
/// The bypass flag is a `tokio::task_local!`, so it does not leak
/// across `tokio::spawn` boundaries and concurrent requests on other
/// tasks continue to see the normal filter. Use this for one-shot
/// scripts (data migrations, seeders, test fixtures); the default
/// per-route handler should always run with the filter on.
pub async fn unguarded<F, Fut, T>(fut: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    UNGUARDED.scope(true, fut()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrs;

    /// Tests that flip the process-wide [`STRICT_DISCARD`] flag
    /// serialize on this mutex so two parallel tests can't observe
    /// each other's flag state. The flag is restored to its prior
    /// value on test exit via [`StrictGuard`].
    static STRICT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Scope guard: lock the strict-mode test mutex, flip the
    /// process-wide knob, restore the prior value on drop. Panics in
    /// the test still drop the guard via stack unwinding.
    struct StrictGuard<'a> {
        _g: std::sync::MutexGuard<'a, ()>,
        prior: bool,
    }
    impl StrictGuard<'_> {
        fn set(strict: bool) -> Self {
            let lock = STRICT_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let prior = preventing_silently_discarding_attributes();
            prevent_silently_discarding_attributes(strict);
            Self { _g: lock, prior }
        }
    }
    impl Drop for StrictGuard<'_> {
        fn drop(&mut self) {
            prevent_silently_discarding_attributes(self.prior);
        }
    }

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

    #[tokio::test]
    async fn unguarded_bypasses_filter() {
        let f = Fillable::guarded(vec!["secret"]);
        let inside =
            super::unguarded(|| async { f.apply(attrs! { secret: "x", visible: 1 }) }).await;
        // Inside the scope, the denylist is ignored.
        assert!(inside.contains_key("secret"));
        assert!(inside.contains_key("visible"));

        // Outside the scope, the denylist is back on.
        let outside = f.apply(attrs! { secret: "x", visible: 1 });
        assert!(!outside.contains_key("secret"));
        assert!(outside.contains_key("visible"));
    }

    #[test]
    fn apply_checked_permissive_default_drops_silently() {
        // Strict mode OFF (the default) — apply_checked silently
        // drops rejected keys, identical to apply.
        let _g = StrictGuard::set(false);
        let f = Fillable::guarded(vec!["secret"]);
        let out = f
            .apply_checked(attrs! { secret: "x", visible: 1 })
            .expect("permissive default should silently drop");
        assert!(!out.contains_key("secret"));
        assert!(out.contains_key("visible"));
    }

    #[test]
    fn apply_checked_strict_rejects_dropped_keys() {
        // Strict mode ON — a payload carrying a rejected key errors.
        let _g = StrictGuard::set(true);
        let f = Fillable::guarded(vec!["secret"]);
        let err = f
            .apply_checked(attrs! { secret: "x", visible: 1 })
            .expect_err("strict mode should reject silently-discarded keys");
        let msg = err.to_string();
        assert!(
            msg.contains("secret"),
            "error should name the dropped key: {msg}"
        );
    }

    #[test]
    fn apply_checked_strict_passes_when_payload_clean() {
        // Strict mode ON — payload with no rejected keys passes.
        let _g = StrictGuard::set(true);
        let f = Fillable::guarded(vec!["secret"]);
        let out = f
            .apply_checked(attrs! { visible: 1 })
            .expect("clean payload should pass even in strict mode");
        assert!(out.contains_key("visible"));
    }

    #[test]
    fn apply_checked_strict_allowlist_rejects_unlisted() {
        // Allowlist mode + strict mode: unlisted keys error.
        let _g = StrictGuard::set(true);
        let f = Fillable::fillable(vec!["name"]);
        let err = f
            .apply_checked(attrs! { name: "X", email: "x@x.com" })
            .expect_err("strict allowlist must reject unlisted keys");
        let msg = err.to_string();
        assert!(msg.contains("email"), "error should name unlisted: {msg}");
    }

    #[tokio::test]
    async fn apply_checked_strict_honors_unguarded_scope() {
        // Inside an unguarded scope, strict mode is bypassed too —
        // the escape hatch must remain absolute.
        let _g = StrictGuard::set(true);
        let f = Fillable::guarded(vec!["secret"]);
        let out =
            super::unguarded(|| async { f.apply_checked(attrs! { secret: "x", visible: 1 }) })
                .await
                .expect("unguarded must bypass strict mode");
        assert!(out.contains_key("secret"));
        assert!(out.contains_key("visible"));
    }
}
