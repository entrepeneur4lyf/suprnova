//! Rule objects — composable validators that work alongside (and
//! independently of) `#[derive(Validate)]`.
//!
//! Three traits cover the design space:
//!
//! - [`Rule`] — pure sync check on a single value. Built-ins:
//!   [`rules::Required`], [`rules::Email`], [`rules::Min`],
//!   [`rules::Max`], [`rules::Between`], [`rules::In`],
//!   [`rules::NotIn`], [`rules::Integer`], [`rules::Numeric`],
//!   [`rules::Boolean`], [`rules::Alpha`], [`rules::AlphaNum`],
//!   [`rules::Url`], [`rules::Uuid`].
//! - [`ContextualRule`] — sync check that can read sibling fields
//!   (think Laravel `required_if:other,value`). Built-ins:
//!   [`rules::RequiredIf`], [`rules::RequiredWith`],
//!   [`rules::RequiredUnless`], [`rules::Same`],
//!   [`rules::Different`], [`rules::Confirmed`].
//! - [`AsyncRule`] — async check (DB queries — [`async_rules::Unique`]
//!   lives here).
//!
//! # Coherence
//!
//! No blanket `impl<R: Rule> ContextualRule for R` is provided. Each
//! built-in rule implements exactly **one** of `Rule` or
//! `ContextualRule` (and `Unique` implements `AsyncRule` only). Adding
//! a blanket would conflict with the explicit `ContextualRule` impls
//! on the conditional rules. Consumers writing their own rules should
//! pick a trait and stick to it.

use crate::error::ValidationErrors;
use std::collections::HashMap;

/// A synchronous validator over a single string value.
///
/// `Err(msg)` carries a human-readable message describing why the
/// value failed. Suprnova does not impose a translation scheme on the
/// message — wrap [`Rule`] yourself if you need i18n.
pub trait Rule {
    /// Check `value`. Return `Ok(())` if it passes, `Err(message)` if
    /// it fails.
    fn passes(&self, value: &str) -> Result<(), String>;

    /// Run the rule and push any failure message onto `errs` under
    /// the given field key. Returns `()` so calls can be chained
    /// without an `if let` per rule. Used by the [`validate!`] macro
    /// and convenient for hand-written `after_validation` bodies that
    /// accumulate errors across many checks.
    ///
    /// [`validate!`]: crate::validate
    fn check(&self, value: &str, errs: &mut ValidationErrors, field: &str) {
        if let Err(msg) = self.passes(value) {
            errs.add(field.to_string(), msg);
        }
    }
}

/// Map of "field name → its current string value", supplied to rules
/// that need to read sibling fields during validation.
pub type FormContext = HashMap<String, String>;

/// A synchronous validator that needs visibility into other form
/// fields.
///
/// This is the trait Laravel's `required_if` / `required_with` /
/// `required_unless` style rules implement. The runner is expected to
/// build a [`FormContext`] keyed by field name and pass it in alongside
/// the value under test.
pub trait ContextualRule {
    /// Check `value` against `ctx`. Return `Ok(())` if it passes,
    /// `Err(message)` if it fails.
    ///
    /// Rules whose semantics depend on the name of the field being
    /// validated (for example, [`rules::Confirmed`], which looks up
    /// `<field>_confirmation` in `ctx`) cannot implement a meaningful
    /// `passes` because the name isn't available here. Such rules
    /// override [`Self::check_named`] instead and use `passes` only
    /// as a stub explaining the limitation.
    fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String>;

    /// Run the rule and push any failure message onto `errs` under
    /// the given field key. The error-accumulating analogue of
    /// [`Self::passes`].
    ///
    /// Most rules don't need the field name — [`Self::check_named`]'s
    /// default impl calls into this method, ignoring `field`. Override
    /// `check_named` directly when the rule needs the name (see
    /// [`rules::Confirmed`]).
    ///
    /// [`validate!`]: crate::validate
    fn check(&self, value: &str, errs: &mut ValidationErrors, field: &str, ctx: &FormContext) {
        if let Err(msg) = self.passes(value, ctx) {
            errs.add(field.to_string(), msg);
        }
    }

    /// Like [`Self::check`], but the rule may use `field` (e.g.
    /// [`rules::Confirmed`] derives `<field>_confirmation` to look up
    /// in `ctx`). The [`validate!`] macro always dispatches through
    /// this method, threading the field ident via
    /// `stringify!($field)`. The default impl ignores `field` and
    /// forwards to [`Self::check`], so rules that don't care about the
    /// field name need not override it.
    ///
    /// [`validate!`]: crate::validate
    fn check_named(
        &self,
        value: &str,
        errs: &mut ValidationErrors,
        field: &str,
        ctx: &FormContext,
    ) {
        self.check(value, errs, field, ctx);
    }
}

/// Built-in synchronous rules — both pure ([`Rule`]) and contextual
/// ([`ContextualRule`]).
pub mod rules {
    use super::{ContextualRule, FormContext, Rule};
    use validator::ValidateEmail;

    /// Treat a value as "blank" when it is empty or whitespace-only.
    ///
    /// Matches Laravel's [`Validator::isImplicit`] heuristic: a string
    /// of only spaces is not considered present.
    fn is_blank(value: &str) -> bool {
        value.trim().is_empty()
    }

    /// Laravel `required` — value must be present and non-whitespace.
    pub struct Required;
    impl Rule for Required {
        fn passes(&self, value: &str) -> Result<(), String> {
            if is_blank(value) {
                Err("required".into())
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `email` — defers to [`validator::ValidateEmail`] so
    /// semantics match `#[validate(email)]` on derived types.
    pub struct Email;
    impl Rule for Email {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.validate_email() {
                Ok(())
            } else {
                Err("must be a valid email".into())
            }
        }
    }

    /// Laravel `min:N` — value must be at least `N` characters long.
    ///
    /// Counts Unicode scalar values (`char`s), not bytes, so multi-byte
    /// characters count as a single character.
    pub struct Min(pub usize);
    impl Rule for Min {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() >= self.0 {
                Ok(())
            } else {
                Err(format!("must be at least {} characters", self.0))
            }
        }
    }

    /// Laravel `max:N` — value must be at most `N` characters long.
    ///
    /// Counts Unicode scalar values (`char`s), not bytes.
    pub struct Max(pub usize);
    impl Rule for Max {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() <= self.0 {
                Ok(())
            } else {
                Err(format!("must be at most {} characters", self.0))
            }
        }
    }

    /// Laravel `between:min,max` — value length is `min..=max` inclusive
    /// (counted in Unicode scalar values, not bytes).
    pub struct Between(pub usize, pub usize);
    impl Rule for Between {
        fn passes(&self, value: &str) -> Result<(), String> {
            let len = value.chars().count();
            if len < self.0 || len > self.1 {
                Err(format!(
                    "must be between {} and {} characters",
                    self.0, self.1
                ))
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `in:foo,bar,baz` — value must be one of the allowed
    /// strings (exact match, case-sensitive).
    pub struct In(pub &'static [&'static str]);
    impl Rule for In {
        fn passes(&self, value: &str) -> Result<(), String> {
            if self.0.contains(&value) {
                Ok(())
            } else {
                Err(format!("must be one of {:?}", self.0))
            }
        }
    }

    /// Laravel `not_in:foo,bar,baz` — value must NOT be in the
    /// forbidden list (exact match, case-sensitive).
    pub struct NotIn(pub &'static [&'static str]);
    impl Rule for NotIn {
        fn passes(&self, value: &str) -> Result<(), String> {
            if self.0.contains(&value) {
                Err(format!("must not be one of {:?}", self.0))
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `integer` — value parses cleanly as an `i64`.
    pub struct Integer;
    impl Rule for Integer {
        fn passes(&self, value: &str) -> Result<(), String> {
            value
                .parse::<i64>()
                .map(|_| ())
                .map_err(|_| "must be an integer".into())
        }
    }

    /// Laravel `numeric` — value parses cleanly as an `f64` (covers
    /// integers, floats, and scientific notation).
    pub struct Numeric;
    impl Rule for Numeric {
        fn passes(&self, value: &str) -> Result<(), String> {
            value
                .parse::<f64>()
                .map(|_| ())
                .map_err(|_| "must be numeric".into())
        }
    }

    /// Laravel `boolean` — accepts `"true"`, `"false"`, `"0"`, `"1"`,
    /// `"yes"`, `"no"`, `"on"`, `"off"` (case-insensitive).
    pub struct Boolean;
    impl Rule for Boolean {
        fn passes(&self, value: &str) -> Result<(), String> {
            match value.to_ascii_lowercase().as_str() {
                "true" | "false" | "0" | "1" | "yes" | "no" | "on" | "off" => Ok(()),
                _ => Err("must be a boolean".into()),
            }
        }
    }

    /// Laravel `alpha` — value must contain only alphabetic
    /// characters and be non-empty.
    ///
    /// **Unicode semantics:** uses [`char::is_alphabetic`] which
    /// accepts non-ASCII letters (`é`, `ñ`, `中`, etc.). This differs
    /// from Laravel 13's default `alpha`, which is ASCII-only — Laravel
    /// only matches Unicode if the `:ascii` suffix is omitted in newer
    /// versions. Suprnova picks the international default; if you need
    /// ASCII-only behaviour, gate with a custom rule.
    pub struct Alpha;
    impl Rule for Alpha {
        fn passes(&self, value: &str) -> Result<(), String> {
            if !value.is_empty() && value.chars().all(|c| c.is_alphabetic()) {
                Ok(())
            } else {
                Err("must contain only letters".into())
            }
        }
    }

    /// Laravel `alpha_dash` — value is letters, digits, underscores,
    /// or hyphens; must be non-empty. Uses Unicode-aware
    /// [`char::is_alphanumeric`].
    pub struct AlphaNum;
    impl Rule for AlphaNum {
        fn passes(&self, value: &str) -> Result<(), String> {
            if !value.is_empty()
                && value
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                Ok(())
            } else {
                Err("must be alphanumeric (letters, digits, _, -)".into())
            }
        }
    }

    /// Laravel `url` — value parses as a well-formed URL via the
    /// [`url`] crate. Note that `Url::parse` is liberal about schemes
    /// (`file:`, custom URIs all pass); tighten with an extra rule if
    /// you specifically want `http`/`https`.
    pub struct Url;
    impl Rule for Url {
        fn passes(&self, value: &str) -> Result<(), String> {
            url::Url::parse(value)
                .map(|_| ())
                .map_err(|_| "must be a valid URL".into())
        }
    }

    /// Laravel `uuid` — value parses as a UUID in any of the formats
    /// the [`uuid`] crate's `parse_str` accepts (hyphenated, simple,
    /// braced, urn).
    pub struct Uuid;
    impl Rule for Uuid {
        fn passes(&self, value: &str) -> Result<(), String> {
            uuid::Uuid::parse_str(value)
                .map(|_| ())
                .map_err(|_| "must be a valid UUID".into())
        }
    }

    /// Laravel `required_if:other,value` — the field is required only
    /// when sibling field `other` is exactly equal to `value`.
    ///
    /// When `other` matches: empty/whitespace value fails.
    /// When `other` does not match (or is missing): always passes.
    pub struct RequiredIf {
        pub other: &'static str,
        pub value: &'static str,
    }
    impl ContextualRule for RequiredIf {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            let other_matches = ctx
                .get(self.other)
                .map(|v| v == self.value)
                .unwrap_or(false);
            if other_matches && is_blank(value) {
                Err(format!("required when {} is {}", self.other, self.value))
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `required_with:other` — the field is required only
    /// when sibling field `other` is present and non-blank.
    pub struct RequiredWith {
        pub other: &'static str,
    }
    impl ContextualRule for RequiredWith {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            let other_present = ctx
                .get(self.other)
                .map(|v| !is_blank(v))
                .unwrap_or(false);
            if other_present && is_blank(value) {
                Err(format!("required when {} is present", self.other))
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `required_unless:other,value` — the field is required
    /// unless sibling field `other` is exactly equal to `value`.
    ///
    /// When `other` matches `value`: always passes.
    /// Otherwise: empty/whitespace value fails.
    pub struct RequiredUnless {
        pub other: &'static str,
        pub value: &'static str,
    }
    impl ContextualRule for RequiredUnless {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            let other_matches = ctx
                .get(self.other)
                .map(|v| v == self.value)
                .unwrap_or(false);
            if !other_matches && is_blank(value) {
                Err(format!("required unless {} is {}", self.other, self.value))
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `same:other_field` — value must equal `ctx[other]`. Used
    /// for password-confirmation style flows where the two fields don't
    /// share the `<field>_confirmation` suffix convention.
    ///
    /// Missing `other` field is treated as a failure.
    pub struct Same {
        pub other: &'static str,
    }
    impl ContextualRule for Same {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            match ctx.get(self.other) {
                Some(v) if v == value => Ok(()),
                _ => Err(format!("must match {}", self.other)),
            }
        }
    }

    /// Laravel `different:other_field` — value must differ from
    /// `ctx[other]`. If `other` is missing, the rule passes (there is
    /// nothing to be the same as).
    pub struct Different {
        pub other: &'static str,
    }
    impl ContextualRule for Different {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            match ctx.get(self.other) {
                Some(v) if v == value => Err(format!("must differ from {}", self.other)),
                _ => Ok(()),
            }
        }
    }

    /// Laravel `confirmed` — value must equal `ctx["<field>_confirmation"]`.
    ///
    /// Because [`ContextualRule::passes`] does not receive the name of
    /// the field being validated, the lookup target is explicit on the
    /// rule itself. Construct as `Confirmed { field: "password" }` when
    /// validating a `password` field; the rule will look up
    /// `password_confirmation` in the form context.
    ///
    /// Missing confirmation field is treated as a failure.
    pub struct Confirmed {
        pub field: &'static str,
    }
    impl ContextualRule for Confirmed {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            let key = format!("{}_confirmation", self.field);
            match ctx.get(&key) {
                Some(v) if v == value => Ok(()),
                _ => Err("confirmation does not match".into()),
            }
        }
    }
}

/// An asynchronous validator over a single string value.
///
/// Rules that need to hit a database, an HTTP service, or any other
/// `.await` point go here. [`async_rules::Unique`] is the canonical
/// built-in.
#[async_trait::async_trait]
pub trait AsyncRule: Send + Sync {
    /// Check `value`. Return `Ok(())` if it passes, `Err(message)` if
    /// it fails.
    async fn passes(&self, value: &str) -> Result<(), String>;

    /// Async analogue of [`Rule::check`]: run the rule and push any
    /// failure message onto `errs` under the given field key.
    ///
    /// The [`validate!`] macro does not currently weave async rules
    /// in (placing `.await` inside a declarative macro arm gets
    /// awkward); use this helper to accumulate async rule failures
    /// alongside your sync checks:
    ///
    /// ```rust,ignore
    /// let mut errs = ValidationErrors::new();
    /// Unique { table: "users", column: "email", except_id: None }
    ///     .check_async(&self.email, &mut errs, "email")
    ///     .await;
    /// errs.into_result()
    /// ```
    ///
    /// [`validate!`]: crate::validate
    async fn check_async(&self, value: &str, errs: &mut ValidationErrors, field: &str) {
        if let Err(msg) = self.passes(value).await {
            errs.add(field.to_string(), msg);
        }
    }
}

/// Built-in asynchronous rules.
pub mod async_rules {
    use super::AsyncRule;
    use crate::DB;
    use sea_orm::{ConnectionTrait, Statement, Value};

    /// Laravel `unique:table,column[,except_id]` — issues a single
    /// parameterized `COUNT(*)` against the configured DB connection.
    ///
    /// Returns `Err` when at least one row matches and its `id`
    /// (when [`Self::except_id`] is set) differs.
    ///
    /// # Safety on identifiers
    ///
    /// `table` and `column` are `&'static str` slices under crate
    /// control (i.e. caller-provided literals in source). The
    /// implementation interpolates them directly into the SQL string,
    /// which is safe because they are not user-controlled. The actual
    /// value being checked and the `except_id`, on the other hand, are
    /// passed as bound parameters.
    pub struct Unique {
        pub table: &'static str,
        pub column: &'static str,
        pub except_id: Option<i64>,
    }

    #[async_trait::async_trait]
    impl AsyncRule for Unique {
        async fn passes(&self, value: &str) -> Result<(), String> {
            let conn = DB::connection().map_err(|e| format!("db: {e}"))?;
            let backend = conn.inner().get_database_backend();

            let (sql, values) = match self.except_id {
                None => (
                    format!(
                        "SELECT COUNT(*) AS c FROM {} WHERE {} = ?",
                        self.table, self.column
                    ),
                    vec![Value::from(value.to_string())],
                ),
                Some(id) => (
                    format!(
                        "SELECT COUNT(*) AS c FROM {} WHERE {} = ? AND id <> ?",
                        self.table, self.column
                    ),
                    vec![Value::from(value.to_string()), Value::from(id)],
                ),
            };

            let stmt = Statement::from_sql_and_values(backend, &sql, values);
            let row = conn
                .inner()
                .query_one(stmt)
                .await
                .map_err(|e| format!("unique query: {e}"))?
                .ok_or_else(|| "unique query returned no rows".to_string())?;

            let count: i64 = row
                .try_get::<i64>("", "c")
                .map_err(|e| format!("unique decode: {e}"))?;

            if count == 0 {
                Ok(())
            } else {
                Err(format!(
                    "{} already exists for {}",
                    self.column, self.table
                ))
            }
        }
    }
}

pub use async_rules::Unique;

/// Run a chain of validation rules over fields of `$self`, accumulating
/// errors into a single [`ValidationErrors`](crate::ValidationErrors).
/// Returns `Ok(())` if every rule passes, `Err(ValidationErrors)`
/// otherwise.
///
/// # Syntax
///
/// ```rust,ignore
/// use suprnova::{validate, Required, Email, Min, Max, RequiredIf, ValidationErrors};
///
/// fn after_validation(&self) -> Result<(), ValidationErrors> {
///     let ctx = self.to_form_context();
///     validate! { self =>
///         email       => Required, Email;
///         password    => Min(8), Max(72);
///         bio?:          Min(10), Max(500);
///         card_number => RequiredIf {
///             other: "billing_type",
///             value: "card",
///         } => with ctx;
///     }
/// }
/// ```
///
/// Each row is one of two shapes:
///
/// - `field_ident => Rule1, Rule2, ... ;` — the field is treated as
///   required-shaped: the rule is invoked on `&self.field` directly.
///   This is the shape for `String`, `i64`, or any other concrete
///   type that derefs to `&str` (or implements [`Rule`] / [`ContextualRule`]
///   over the contained scalar).
/// - `field_ident ?: Rule1, Rule2, ... ;` — the field is `Option<T>`.
///   When `Some`, the rules run on the unwrapped inner value; when
///   `None`, every rule on the row is skipped. This matches Laravel's
///   "if present, validate" semantics for optional form fields and is
///   the right choice for every `Option<String>` (or `Option<i64>`, …)
///   field on a form.
///
/// Each rule is either a plain [`Rule`] (no suffix) or a
/// [`ContextualRule`] followed by `=> with $ctx_ident`. The contextual
/// separator is `=> with` (not parenthesised) because `macro_rules!`
/// matches `$rule:expr` greedily — placing the suffix in parentheses
/// runs into Rust's `FOLLOW` set rules for `expr` fragments.
///
/// The macro expands to a fresh [`ValidationErrors`](crate::ValidationErrors),
/// calls [`Rule::check`] / [`ContextualRule::check_named`] for each
/// declared rule, then returns
/// [`ValidationErrors::into_result`](crate::ValidationErrors::into_result).
///
/// # `Option<T>` fields
///
/// Use the `?:` row separator (`bio?: Min(10), Max(500);`). The
/// macro expands to `if let Some(ref __val) = self.bio { ... }`, so
/// rules only run when the field is `Some`. Rules see the inner value
/// borrowed as the type they expect (typically `&str` via
/// `String: Deref<Target=str>` auto-deref). For non-string optional
/// types, the inner type must implement the rule's expected borrow
/// itself.
///
/// # Async rules
///
/// The macro is sync-only. Call
/// [`AsyncRule::check_async`](crate::AsyncRule::check_async) inline
/// for async-backed checks; both styles accumulate into the same
/// [`ValidationErrors`](crate::ValidationErrors).
#[macro_export]
macro_rules! validate {
    ($self:ident => $($tt:tt)*) => {{
        let mut __errs = $crate::ValidationErrors::new();
        $crate::__validate_rows!(__errs, $self, $($tt)*);
        __errs.into_result()
    }};
}

/// Internal row walker used by [`validate!`]. Not part of the public
/// API even though `#[macro_export]` makes it reachable at the crate
/// root — `#[doc(hidden)]` keeps it out of rustdoc.
///
/// The walker consumes one row per recursive invocation. A row is
/// either `field => rule1, rule2;` (required-shape) or
/// `field?: rule1, rule2;` (optional-shape — runs rules only when the
/// field is `Some`). Recursion terminates when the input is empty (or
/// only a stray `;` remains, supporting the optional trailing
/// semicolon style).
#[macro_export]
#[doc(hidden)]
macro_rules! __validate_rows {
    // Optional-shape row: `field?: Rule1, Rule2 => with ctx, ... ;`
    ($errs:ident, $self:ident, $field:ident ?: $($rule:expr $(=> with $ctx:ident)?),+ ; $($rest:tt)*) => {
        if let ::core::option::Option::Some(ref __val) = $self.$field {
            $(
                $crate::__validate_one_optional!($errs, $field, __val, $rule $(=> with $ctx)?);
            )+
        }
        $crate::__validate_rows!($errs, $self, $($rest)*);
    };
    // Required-shape row: `field => Rule1, Rule2 => with ctx, ... ;`
    ($errs:ident, $self:ident, $field:ident => $($rule:expr $(=> with $ctx:ident)?),+ ; $($rest:tt)*) => {
        $(
            $crate::__validate_one!($errs, $self, $field, $rule $(=> with $ctx)?);
        )+
        $crate::__validate_rows!($errs, $self, $($rest)*);
    };
    // Terminal: input exhausted (with or without a trailing `;`).
    ($errs:ident, $self:ident, $(;)?) => {};
}

/// Internal dispatch macro used by [`validate!`] for required-shape
/// rows. Not part of the public API.
#[macro_export]
#[doc(hidden)]
macro_rules! __validate_one {
    ($errs:ident, $self:ident, $field:ident, $rule:expr => with $ctx:ident) => {
        $crate::validation::rule::ContextualRule::check_named(
            &$rule,
            &$self.$field,
            &mut $errs,
            ::core::stringify!($field),
            &$ctx,
        );
    };
    ($errs:ident, $self:ident, $field:ident, $rule:expr) => {
        $crate::validation::rule::Rule::check(
            &$rule,
            &$self.$field,
            &mut $errs,
            ::core::stringify!($field),
        );
    };
}

/// Internal dispatch macro used by [`validate!`] for optional-shape
/// rows. Runs against the borrowed inner value of an `Option`. Not
/// part of the public API.
#[macro_export]
#[doc(hidden)]
macro_rules! __validate_one_optional {
    ($errs:ident, $field:ident, $val:ident, $rule:expr => with $ctx:ident) => {
        $crate::validation::rule::ContextualRule::check_named(
            &$rule,
            $val,
            &mut $errs,
            ::core::stringify!($field),
            &$ctx,
        );
    };
    ($errs:ident, $field:ident, $val:ident, $rule:expr) => {
        $crate::validation::rule::Rule::check(
            &$rule,
            $val,
            &mut $errs,
            ::core::stringify!($field),
        );
    };
}
