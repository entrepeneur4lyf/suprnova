//! Feature-flag declarations for the example app.
//!
//! Holds every `Feature` const the app references. Centralising the
//! declarations here gives us:
//!
//! * a single place to grep when an operator asks "what flags does this
//!   app even know about?",
//! * compile-time uniqueness of the flag name (a typo at the
//!   `is_enabled!("new-checout-flow", ...)` site would compile, but
//!   pulling the name from a `const Feature` here surfaces the typo
//!   at the declaration),
//! * the right place for a code reviewer to see the default-when-absent
//!   value — the second argument to `Feature::new` is what
//!   `is_enabled!` returns when no row exists in the `features` table
//!   or no evaluator is installed.
//!
//! Phase 13 T7 ships exactly one flag (`NEW_CHECKOUT_FLOW`) so the
//! dogfood path stays minimal; real apps would list every flag here
//! and reference each one from the handler / view / job that gates on
//! it.

use suprnova::features::Feature;

/// Gates the "new checkout flow" prop on the home page. When the
/// flag's persisted row says `enabled = true` for the current
/// `(user, team)` context, [`crate::controllers::home::index`] adds a
/// `new_checkout_banner` prop the Svelte/React/Vue page conditionally
/// renders. When the flag is off (the default, on a fresh DB), the
/// prop is absent and the page renders the legacy checkout.
///
/// Toggle via `admin::upsert("new-checkout-flow", "", true, ...)` from
/// a CLI or admin handler. The middleware-bound
/// [`crate::bootstrap`] evaluator picks up the change before
/// `admin::upsert` returns.
pub const NEW_CHECKOUT_FLOW: Feature<'static> = Feature::new("new-checkout-flow", false);
