//! Per-request include/exclude/only/except set parsed from the query
//! string by `IncludeMiddleware` and bound to a `tokio::task_local!`
//! so handlers and the lazy-prop resolver can consult it.

use std::sync::Arc;

/// Parsed `?include=`/`?exclude=`/`?only=`/`?except=` query parameters.
///
/// Semantics (Laravel-Data parity):
/// - `include` — lazy fields to resolve (default: none resolved).
/// - `exclude` — fields to drop from the response.
/// - `only` — when set, the response includes ONLY these fields.
/// - `except` — fields to drop (same effect as `exclude`; both names
///   exist for Laravel-Data API parity).
///
/// The four fields are `pub` for back-compat with code that builds an
/// instance via struct-literal syntax (the `data_partial_data_composition`
/// and `data_lazy_resolution` integration tests rely on this). New
/// callers should prefer the fluent builder methods
/// ([`Self::include`], [`Self::exclude`], [`Self::only`], [`Self::except`])
/// and their bool-conditional variants ([`Self::include_when`], etc.).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestIncludeSet {
    /// Field paths requested via `?include=`. Additive on top of the default set.
    pub include: Vec<String>,
    /// Field paths to drop via `?exclude=`. Removed from the resolved set after `include` is applied.
    pub exclude: Vec<String>,
    /// Field paths from `?only=` — when present, the resolved set is reduced to exactly this list.
    pub only: Option<Vec<String>>,
    /// Field paths from `?except=` — removed from the resolved set, equivalent to `exclude` but distinct so query-string round-trips preserve operator intent.
    pub except: Vec<String>,
}

impl RequestIncludeSet {
    /// Parse `?include=`/`?exclude=`/`?only=`/`?except=` query parameters
    /// from a raw query string.
    ///
    /// # Input contract
    ///
    /// - `raw` must NOT include the leading `?` — caller strips it.
    /// - The parser URL-decodes pairs via `url::form_urlencoded::parse`
    ///   before splitting, so `include=foo%2Cbar` decodes to two
    ///   entries `[foo, bar]`, and array-form keys may also be encoded
    ///   (`include%5B%5D=foo`).
    /// - Repeated keys accumulate (`include=a&include=b` → `include: [a, b]`),
    ///   matching Laravel's array semantics.
    /// - The Laravel array form `include[]=a&include[]=b` is also accepted
    ///   and accumulates the same way.
    /// - Whitespace around values is trimmed; empty values are dropped
    ///   (`include= a , , b` → `[a, b]`).
    /// - Unknown keys are silently ignored — only the four canonical names
    ///   are recognized.
    /// - Malformed percent-encoding sequences are tolerated lossily by
    ///   `form_urlencoded::parse`; bad bytes are decoded with replacement
    ///   rather than rejected, matching browser behavior.
    pub fn from_query(raw: &str) -> Self {
        let mut s = Self::default();
        for (key_cow, val_cow) in url::form_urlencoded::parse(raw.as_bytes()) {
            let key = key_cow.trim();
            let stripped_key = key.strip_suffix("[]").unwrap_or(key);
            let values: Vec<String> = val_cow
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            match stripped_key {
                "include" => s.include.extend(values),
                "exclude" => s.exclude.extend(values),
                "only" => {
                    let bucket = s.only.get_or_insert_with(Vec::new);
                    bucket.extend(values);
                }
                "except" => s.except.extend(values),
                _ => {}
            }
        }
        s
    }

    /// Returns `true` when none of the four lists carry any directive — i.e.
    /// the request did not constrain the include set in any way.
    pub fn is_empty(&self) -> bool {
        self.include.is_empty()
            && self.exclude.is_empty()
            && self.only.is_none()
            && self.except.is_empty()
    }

    /// Returns `true` when `field` appears in the `include` list.
    ///
    /// Dot-paths in the include list count as a request for their root
    /// segment: `include=author.posts` reports `set.includes("author")
    /// == true`. This matches Laravel-Data's path resolution, where the
    /// top-level field name is what the resolver checks.
    pub fn includes(&self, field: &str) -> bool {
        self.include
            .iter()
            .any(|s| s == field || s.split_once('.').map(|(head, _)| head) == Some(field))
    }

    /// Returns `true` when `field` appears in the `exclude` list (also
    /// honoring dot-path root matching). The mirror of [`Self::includes`].
    pub fn is_excluded(&self, field: &str) -> bool {
        self.exclude
            .iter()
            .any(|s| s == field || s.split_once('.').map(|(head, _)| head) == Some(field))
    }

    /// Returns `true` when `field` appears in the `except` list.
    /// Laravel-Data treats `except` as a permanent drop signal — the
    /// field is removed even if `only` would otherwise include it.
    pub fn is_excepted(&self, field: &str) -> bool {
        self.except
            .iter()
            .any(|s| s == field || s.split_once('.').map(|(head, _)| head) == Some(field))
    }

    /// Returns `true` when `only` is set AND `field` is on the
    /// allowlist. Returns `true` when `only` is unset (no narrowing
    /// requested). Returns `false` when `only` is set but `field` is
    /// not on the list — that field has been narrowed out.
    pub fn is_only_listed(&self, field: &str) -> bool {
        match &self.only {
            None => true,
            Some(list) => list
                .iter()
                .any(|s| s == field || s.split_once('.').map(|(head, _)| head) == Some(field)),
        }
    }

    /// Compose all four lists into one verdict: is `field` visible in
    /// the response? Laravel-Data's resolution order:
    ///
    /// 1. `except` wins over everything — excepted fields are dropped.
    /// 2. `exclude` also drops the field.
    /// 3. `only` narrows: when set, only listed fields survive.
    /// 4. Otherwise the field is visible.
    ///
    /// `?include=` is NOT part of this predicate — `include` flips
    /// lazy fields from "omit" to "resolve", but doesn't gate the
    /// visibility of eager fields. Use [`Self::includes`] for that.
    pub fn is_visible(&self, field: &str) -> bool {
        if self.is_excepted(field) {
            return false;
        }
        if self.is_excluded(field) {
            return false;
        }
        self.is_only_listed(field)
    }

    /// Append fields to the `include` list. Fluent: returns `self`.
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::include(...$fields)`.
    /// Strings are trimmed; empty strings are dropped. Duplicates
    /// against existing entries are preserved (matching Laravel's
    /// append-semantics — the resolver de-dupes downstream).
    pub fn include<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::extend_list(&mut self.include, fields);
        self
    }

    /// Append fields to the `exclude` list. Fluent: returns `self`.
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::exclude(...$fields)`.
    pub fn exclude<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::extend_list(&mut self.exclude, fields);
        self
    }

    /// Set or extend the `only` list. Fluent: returns `self`.
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::only(...$fields)`.
    /// Calling `only(...)` for the first time initialises the list;
    /// subsequent calls APPEND to it (idempotent — once `only` is set,
    /// it stays set). To clear it, construct a fresh set.
    pub fn only<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let bucket = self.only.get_or_insert_with(Vec::new);
        Self::extend_list(bucket, fields);
        self
    }

    /// Append fields to the `except` list. Fluent: returns `self`.
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::except(...$fields)`.
    pub fn except<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::extend_list(&mut self.except, fields);
        self
    }

    /// [`Self::include`] gated by a `bool`. When `condition` is false
    /// the call is a no-op (the original `self` is returned).
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::includeWhen($field, $condition)`.
    /// Laravel takes a `bool|Closure`; in Rust the caller computes the
    /// condition at the call site.
    pub fn include_when<I, S>(self, condition: bool, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if condition {
            self.include(fields)
        } else {
            self
        }
    }

    /// [`Self::exclude`] gated by a `bool`.
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::excludeWhen($field, $condition)`.
    pub fn exclude_when<I, S>(self, condition: bool, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if condition {
            self.exclude(fields)
        } else {
            self
        }
    }

    /// [`Self::only`] gated by a `bool`.
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::onlyWhen($field, $condition)`.
    pub fn only_when<I, S>(self, condition: bool, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if condition { self.only(fields) } else { self }
    }

    /// [`Self::except`] gated by a `bool`.
    ///
    /// Matches `Spatie\LaravelData\Concerns\IncludeableData::exceptWhen($field, $condition)`.
    pub fn except_when<I, S>(self, condition: bool, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if condition { self.except(fields) } else { self }
    }

    /// Merge another set into `self`. `include`/`exclude`/`except` are
    /// appended; `only` is appended into an existing `Some(list)` or
    /// adopted wholesale if `self.only` is `None`. Useful when a
    /// handler wants to layer programmatic overrides on top of the
    /// request-driven set produced by [`super::IncludeMiddleware`].
    pub fn merge(mut self, other: Self) -> Self {
        self.include.extend(other.include);
        self.exclude.extend(other.exclude);
        self.except.extend(other.except);
        match (&mut self.only, other.only) {
            (Some(list), Some(other_list)) => list.extend(other_list),
            (slot @ None, Some(other_list)) => *slot = Some(other_list),
            _ => {}
        }
        self
    }

    /// Trim + drop-empty + push into `dest`. Shared by every builder
    /// method to keep the input-normalisation behaviour identical to
    /// [`Self::from_query`].
    fn extend_list<I, S>(dest: &mut Vec<String>, fields: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for raw in fields {
            let value: String = raw.into();
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                dest.push(trimmed.to_string());
            }
        }
    }
}

tokio::task_local! {
    /// Per-request include set. Bound by `IncludeMiddleware`; consulted
    /// by `Prop::Lazy` resolution and any handler that wants to honor
    /// `?include=` / `?only=` semantics.
    pub static REQUEST_INCLUDE_SET: Arc<RequestIncludeSet>;
}

/// Helper: get the current request's include set, or empty if none bound.
pub fn current_include_set() -> Arc<RequestIncludeSet> {
    REQUEST_INCLUDE_SET
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::new(RequestIncludeSet::default()))
}

/// Scope a [`RequestIncludeSet`] around a future. Used by both
/// [`super::IncludeMiddleware`] (to bind the parsed query string) and
/// by tests/handlers that need to install a programmatic set.
///
/// ```rust,no_run
/// # use suprnova::{RequestIncludeSet, scope_include_set};
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// let set = RequestIncludeSet::from_query("include=author");
/// let result = scope_include_set(set, async { /* handler code */ }).await;
/// # let _ = result;
/// # Ok(()) }
/// ```
pub async fn scope_include_set<F, R>(set: RequestIncludeSet, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    REQUEST_INCLUDE_SET.scope(Arc::new(set), f).await
}

/// Read the request-bound include set, apply the caller's mutation
/// closure, and re-scope the result around `f`. This is the
/// production-grade way for a handler to add programmatic
/// includes/excludes on top of what the request's query string
/// already declared.
///
/// ```rust,no_run
/// # use suprnova::with_include_overrides;
/// # struct User;
/// # impl User { fn is_admin(&self) -> bool { true } }
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// # let user = User;
/// // Inside a handler — admins get the audit-log relationship
/// // appended to whatever the client asked for.
/// let body = with_include_overrides(
///     |set| if user.is_admin() { set.include(["audit_log"]) } else { set },
///     async move {
///         // ...build and return the response here...
///         "Album/Show".to_string()
///     },
/// ).await;
/// # let _ = body;
/// # Ok(()) }
/// ```
///
/// Calling this when no set is currently bound starts from the empty
/// default — equivalent to `scope_include_set(f(RequestIncludeSet::default()), ...)`.
pub async fn with_include_overrides<M, F, R>(mutate: M, f: F) -> R
where
    M: FnOnce(RequestIncludeSet) -> RequestIncludeSet,
    F: std::future::Future<Output = R>,
{
    let current = current_include_set();
    // `Arc::try_unwrap` would clone the underlying set anyway when the
    // refcount is > 1 (it nearly always is — the middleware holds one).
    // Clone via `(*current).clone()` keeps the borrow explicit.
    let mutated = mutate((*current).clone());
    REQUEST_INCLUDE_SET.scope(Arc::new(mutated), f).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------- fluent builder methods --------

    #[test]
    fn include_appends_strings() {
        let s = RequestIncludeSet::default().include(["author", "tags"]);
        assert_eq!(s.include, vec!["author", "tags"]);
    }

    #[test]
    fn include_is_chainable() {
        let s = RequestIncludeSet::default()
            .include(["author"])
            .include(["tags", "comments"]);
        assert_eq!(s.include, vec!["author", "tags", "comments"]);
    }

    #[test]
    fn include_trims_and_drops_empty() {
        let s = RequestIncludeSet::default().include([" author ", "", "tags "]);
        assert_eq!(s.include, vec!["author", "tags"]);
    }

    #[test]
    fn include_accepts_string_owned() {
        let s = RequestIncludeSet::default().include(vec![String::from("author")]);
        assert_eq!(s.include, vec!["author"]);
    }

    #[test]
    fn exclude_appends_strings() {
        let s = RequestIncludeSet::default().exclude(["secret"]);
        assert_eq!(s.exclude, vec!["secret"]);
    }

    #[test]
    fn only_initialises_and_extends() {
        let s = RequestIncludeSet::default().only(["id", "name"]);
        assert_eq!(s.only, Some(vec!["id".into(), "name".into()]));
        let s = s.only(["email"]);
        assert_eq!(
            s.only,
            Some(vec!["id".into(), "name".into(), "email".into()])
        );
    }

    #[test]
    fn except_appends_strings() {
        let s = RequestIncludeSet::default().except(["password"]);
        assert_eq!(s.except, vec!["password"]);
    }

    // -------- conditional variants --------

    #[test]
    fn include_when_true_appends() {
        let s = RequestIncludeSet::default().include_when(true, ["author"]);
        assert_eq!(s.include, vec!["author"]);
    }

    #[test]
    fn include_when_false_is_noop() {
        let s = RequestIncludeSet::default().include_when(false, ["author"]);
        assert!(s.include.is_empty());
    }

    #[test]
    fn exclude_when_false_is_noop() {
        let s = RequestIncludeSet::default().exclude_when(false, ["secret"]);
        assert!(s.exclude.is_empty());
    }

    #[test]
    fn only_when_false_leaves_only_unset() {
        let s = RequestIncludeSet::default().only_when(false, ["id"]);
        assert!(s.only.is_none());
    }

    #[test]
    fn except_when_true_appends() {
        let s = RequestIncludeSet::default().except_when(true, ["password"]);
        assert_eq!(s.except, vec!["password"]);
    }

    // -------- predicates: includes / is_excluded / is_excepted / is_only_listed --------

    #[test]
    fn includes_matches_dot_path_root() {
        let s = RequestIncludeSet::default().include(["author.posts"]);
        assert!(s.includes("author"));
        // The deeper segment is not by itself in the include list.
        assert!(!s.includes("posts"));
    }

    #[test]
    fn is_excluded_matches_dot_path_root() {
        let s = RequestIncludeSet::default().exclude(["author.email"]);
        assert!(s.is_excluded("author"));
        assert!(!s.is_excluded("email"));
    }

    #[test]
    fn is_excepted_matches_dot_path_root() {
        let s = RequestIncludeSet::default().except(["author.password"]);
        assert!(s.is_excepted("author"));
    }

    #[test]
    fn is_only_listed_returns_true_when_only_unset() {
        let s = RequestIncludeSet::default();
        assert!(s.is_only_listed("anything"));
    }

    #[test]
    fn is_only_listed_filters_when_only_set() {
        let s = RequestIncludeSet::default().only(["id", "name"]);
        assert!(s.is_only_listed("id"));
        assert!(s.is_only_listed("name"));
        assert!(!s.is_only_listed("password"));
    }

    // -------- is_visible composition --------

    #[test]
    fn is_visible_true_when_no_filters_active() {
        let s = RequestIncludeSet::default();
        assert!(s.is_visible("anything"));
    }

    #[test]
    fn is_visible_excepted_wins() {
        // Even if `only` lists the field, `except` removes it.
        let s = RequestIncludeSet::default()
            .only(["id", "secret"])
            .except(["secret"]);
        assert!(s.is_visible("id"));
        assert!(!s.is_visible("secret"));
    }

    #[test]
    fn is_visible_excluded_drops() {
        let s = RequestIncludeSet::default().exclude(["password"]);
        assert!(!s.is_visible("password"));
        assert!(s.is_visible("name"));
    }

    #[test]
    fn is_visible_only_narrows() {
        let s = RequestIncludeSet::default().only(["id"]);
        assert!(s.is_visible("id"));
        assert!(!s.is_visible("password"));
    }

    // -------- merge --------

    #[test]
    fn merge_concatenates_lists() {
        let a = RequestIncludeSet::default()
            .include(["author"])
            .exclude(["secret"]);
        let b = RequestIncludeSet::default()
            .include(["tags"])
            .except(["password"]);
        let merged = a.merge(b);
        assert_eq!(merged.include, vec!["author", "tags"]);
        assert_eq!(merged.exclude, vec!["secret"]);
        assert_eq!(merged.except, vec!["password"]);
    }

    #[test]
    fn merge_adopts_only_when_self_was_none() {
        let a = RequestIncludeSet::default();
        let b = RequestIncludeSet::default().only(["id"]);
        let merged = a.merge(b);
        assert_eq!(merged.only, Some(vec!["id".into()]));
    }

    #[test]
    fn merge_extends_only_when_self_already_set() {
        let a = RequestIncludeSet::default().only(["id"]);
        let b = RequestIncludeSet::default().only(["name"]);
        let merged = a.merge(b);
        assert_eq!(merged.only, Some(vec!["id".into(), "name".into()]));
    }

    // -------- with_include_overrides: actually changes what current_include_set sees --------

    #[tokio::test]
    async fn with_include_overrides_layers_on_top_of_existing_scope() {
        // Simulate IncludeMiddleware's bind: `?include=author`.
        let base = RequestIncludeSet::from_query("include=author");
        let observed = scope_include_set(base, async move {
            // A handler appends `audit_log` programmatically.
            with_include_overrides(|set| set.include(["audit_log"]), async {
                current_include_set()
            })
            .await
        })
        .await;

        assert_eq!(observed.include, vec!["author", "audit_log"]);
    }

    #[tokio::test]
    async fn with_include_overrides_from_empty_starts_at_default() {
        // No outer scope_include_set — current_include_set returns
        // the default empty set; the override is applied on top.
        let observed =
            with_include_overrides(|set| set.include(["author"]).exclude(["secret"]), async {
                current_include_set()
            })
            .await;

        assert_eq!(observed.include, vec!["author"]);
        assert_eq!(observed.exclude, vec!["secret"]);
    }

    #[tokio::test]
    async fn with_include_overrides_propagates_through_includes_predicate() {
        // Drive the resolver-side check: after the override, the
        // built set must report `includes("audit_log") == true`.
        let result = with_include_overrides(|set| set.include(["audit_log"]), async {
            current_include_set().includes("audit_log")
        })
        .await;
        assert!(result);
    }
}
