use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::FrameworkError;

/// Minimal request abstraction used by [`crate::inertia::InertiaResponse::resolve`]
/// and [`PartialFilter::build`].
///
/// Production code uses [`crate::http::Request`] (which implements this
/// trait via the blanket impl below). Tests provide a tiny mock without
/// having to construct a real `hyper::Request<hyper::body::Incoming>` —
/// `Incoming` cannot be built outside hyper's connection internals.
pub trait InertiaRequestExt: Send + Sync {
    fn path(&self) -> &str;
    fn header(&self, name: &str) -> Option<&str>;
    fn is_inertia(&self) -> bool {
        self.header("X-Inertia")
            .map(|v| v == "true")
            .unwrap_or(false)
    }
}

impl InertiaRequestExt for crate::http::Request {
    fn path(&self) -> &str {
        crate::http::Request::path(self)
    }
    fn header(&self, name: &str) -> Option<&str> {
        crate::http::Request::header(self, name)
    }
    fn is_inertia(&self) -> bool {
        crate::http::Request::is_inertia(self)
    }
}

// Blanket impl so callers can pass `&Request`, `&MockRequest`, etc.
// interchangeably without worrying about ref depth.
impl<T: InertiaRequestExt + ?Sized> InertiaRequestExt for &T {
    fn path(&self) -> &str {
        (**self).path()
    }
    fn header(&self, name: &str) -> Option<&str> {
        (**self).header(name)
    }
    fn is_inertia(&self) -> bool {
        (**self).is_inertia()
    }
}

/// Future returned by a deferred prop resolver.
///
/// Used by [`Prop::Lazy`] and [`Prop::Optional`]. Resolvers can do async
/// work (DB queries, HTTP calls) because we're under Tokio. Errors are
/// surfaced through [`FrameworkError`] so they become 500 responses just
/// like any other handler failure.
pub type PropFuture = Pin<Box<dyn Future<Output = Result<Value, FrameworkError>> + Send>>;

/// Closure stored inside [`Prop::Lazy`] and [`Prop::Optional`].
///
/// `Arc` so the response can be cloned (cheap) before resolving;
/// `Send + Sync + 'static` so it can be moved across `.await` points.
pub type PropResolver = Arc<dyn Fn() -> PropFuture + Send + Sync>;

/// Configuration for a [`Prop::Defer`] entry.
#[derive(Clone)]
pub struct DeferConfig {
    pub resolver: PropResolver,
    /// Logical group; clients fetch all keys in a group in one follow-up
    /// XHR. Defaults to `"default"`.
    pub group: String,
    /// When `true`, the framework catches resolver errors, omits the key
    /// from `props`, and lists it under `rescuedProps` so the client can
    /// render its `rescue` slot. When `false`, errors propagate as 500.
    pub rescue: bool,
}

/// Builder for the options passed to
/// [`InertiaResponse::defer_with`](crate::InertiaResponse::defer_with).
#[derive(Debug, Clone)]
pub struct DeferOptions {
    pub(crate) group: String,
    pub(crate) rescue: bool,
}

impl Default for DeferOptions {
    fn default() -> Self {
        Self {
            group: "default".to_string(),
            rescue: false,
        }
    }
}

impl DeferOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bucket the deferred prop under a named group so multiple
    /// resolvers fetch together in a single follow-up XHR.
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Catch resolver errors instead of failing the page render. The
    /// failed key is omitted from `props` and reported under
    /// `rescuedProps` so the client renders its `rescue` slot.
    pub fn rescue(mut self) -> Self {
        self.rescue = true;
        self
    }
}

/// Merge strategy for [`Prop::Merge`].
#[derive(Clone)]
pub enum MergeStrategy {
    /// Append items to the array at the prop's root. Maps to
    /// `Inertia::merge(...)`.
    Append { match_on: Option<String> },
    /// Prepend items to the array at the prop's root. Maps to
    /// `Inertia::merge(...)->prepend()`.
    Prepend { match_on: Option<String> },
    /// Deep-merge structures. Maps to `Inertia::deepMerge(...)`.
    Deep { match_on: Option<String> },
}

/// Configuration for a [`Prop::Merge`] entry.
#[derive(Clone)]
pub struct MergeConfig {
    pub resolver: PropResolver,
    pub strategy: MergeStrategy,
}

/// Configuration for a [`Prop::Once`] entry.
#[derive(Clone)]
pub struct OnceConfig {
    pub resolver: PropResolver,
    /// Cache key the client uses to dedupe. Defaults to the prop's name;
    /// override with `OnceOptions::as_key` so multiple pages can share a
    /// cached value under different prop names.
    pub cache_key: String,
    /// Optional expiration timestamp in millis-since-epoch. The client
    /// invalidates its cached value once now() exceeds this.
    pub expires_at: Option<i64>,
    /// When `true`, ignore the client's `X-Inertia-Except-Once-Props` for
    /// this key — server-forced refresh. Maps to `Inertia::once()->fresh()`.
    pub fresh: bool,
}

/// Builder for the options passed to
/// [`InertiaResponse::once_with`](crate::InertiaResponse::once_with).
#[derive(Debug, Clone, Default)]
pub struct OnceOptions {
    pub(crate) cache_key: Option<String>,
    pub(crate) expires_at: Option<i64>,
    pub(crate) fresh: bool,
}

impl OnceOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the cache key the client uses to dedupe this prop.
    /// Defaults to the prop's name. Map to `Inertia::once()->as('key')`.
    pub fn as_key(mut self, key: impl Into<String>) -> Self {
        self.cache_key = Some(key.into());
        self
    }

    /// Expire the cached value at the given millis-since-epoch timestamp.
    /// The client invalidates and refetches once now() exceeds this.
    /// Maps to `Inertia::once()->until($timestamp)`.
    pub fn until(mut self, expires_at_ms: i64) -> Self {
        self.expires_at = Some(expires_at_ms);
        self
    }

    /// Force the resolver to run even when the client claims to have a
    /// cached value via `X-Inertia-Except-Once-Props`. Server-side override.
    /// Maps to `Inertia::once()->fresh()`.
    pub fn fresh(mut self) -> Self {
        self.fresh = true;
        self
    }
}

/// A page prop with a resolution strategy.
///
/// Tier 0 introduced `Eager` and `Always`. Tier 1 adds `Lazy` and
/// `Optional`. Tier 2 adds `Defer`, `Merge`, and `Once` per the
/// Inertia 3.x protocol — see `docs/parity/inertia.md`.
#[derive(Clone)]
pub enum Prop {
    /// Materialized at builder time. Included on standard visits;
    /// respects partial-reload filtering on partial visits.
    Eager(Value),

    /// Materialized at builder time. Always included, even on partial
    /// reloads that did not request the key. Maps to `Inertia::always(...)`.
    Always(Value),

    /// Resolved lazily at response time. Same inclusion rules as `Eager`
    /// (always on standard visits, only-when-requested on partial reloads)
    /// — but the closure only runs when the prop will actually be sent.
    /// Maps to Laravel's `fn () => ...` prop pattern.
    Lazy(PropResolver),

    /// Resolved lazily AND only when explicitly requested. Never included
    /// on standard visits; on partial reloads, included only when the key
    /// appears in `X-Inertia-Partial-Data`. Maps to `Inertia::optional(...)`.
    Optional(PropResolver),

    /// Deferred prop — never resolved on the initial visit. Emitted under
    /// `deferredProps: {group: [keys]}` so the client can issue a
    /// follow-up partial-reload XHR that includes the key. On that
    /// follow-up, the resolver runs and the value lands in `props`.
    /// Maps to `Inertia::defer(...)`.
    Defer(DeferConfig),

    /// Mergeable prop — resolver runs normally and value lands in `props`,
    /// but the framework also emits the key under `mergeProps` /
    /// `prependProps` / `deepMergeProps` so the client appends/prepends/
    /// deep-merges into existing client-side state instead of replacing.
    /// Maps to `Inertia::merge(...)` / `Inertia::deepMerge(...)`.
    Merge(MergeConfig),

    /// Cached-on-client prop — the client remembers the value across
    /// navigations and sends `X-Inertia-Except-Once-Props` to skip
    /// re-resolution. Resolver runs only when the client doesn't already
    /// have the key (or when the server forces refresh via `fresh()`).
    /// Maps to `Inertia::once(...)`.
    Once(OnceConfig),
}

impl std::fmt::Debug for Prop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Prop::Eager(v) => f.debug_tuple("Eager").field(v).finish(),
            Prop::Always(v) => f.debug_tuple("Always").field(v).finish(),
            Prop::Lazy(_) => f.debug_struct("Lazy").finish_non_exhaustive(),
            Prop::Optional(_) => f.debug_struct("Optional").finish_non_exhaustive(),
            Prop::Defer(c) => f
                .debug_struct("Defer")
                .field("group", &c.group)
                .field("rescue", &c.rescue)
                .finish_non_exhaustive(),
            Prop::Merge(_) => f.debug_struct("Merge").finish_non_exhaustive(),
            Prop::Once(c) => f
                .debug_struct("Once")
                .field("cache_key", &c.cache_key)
                .field("expires_at", &c.expires_at)
                .field("fresh", &c.fresh)
                .finish_non_exhaustive(),
        }
    }
}

impl Prop {
    /// True if this prop must appear regardless of partial-reload filtering.
    pub fn is_always(&self) -> bool {
        matches!(self, Prop::Always(_))
    }

    /// True if the prop will never appear on a standard (non-partial) visit
    /// and must be explicitly requested via `X-Inertia-Partial-Data`.
    pub fn is_optional(&self) -> bool {
        matches!(self, Prop::Optional(_))
    }

    /// True if the prop is a [`Prop::Defer`] — initial-visit-skipped.
    pub fn is_defer(&self) -> bool {
        matches!(self, Prop::Defer(_))
    }

    /// True if the prop holds a deferred (closure) resolver of any kind.
    pub fn is_deferred(&self) -> bool {
        matches!(
            self,
            Prop::Lazy(_) | Prop::Optional(_) | Prop::Defer(_) | Prop::Once(_) | Prop::Merge(_)
        )
    }

    /// Call the resolver associated with this prop, if any.
    ///
    /// Returns the produced value for the closure-backed variants (Lazy,
    /// Optional, Defer, Merge, Once) and the existing value for Eager /
    /// Always. The full request-aware materialization — including
    /// `deferredProps` / `mergeProps` / `onceProps` metadata emission —
    /// lives in `InertiaResponse::resolve` and uses this method internally.
    pub async fn resolve(self) -> Result<Value, FrameworkError> {
        match self {
            Prop::Eager(v) | Prop::Always(v) => Ok(v),
            Prop::Lazy(r) | Prop::Optional(r) => r().await,
            Prop::Defer(c) => (c.resolver)().await,
            Prop::Merge(c) => (c.resolver)().await,
            Prop::Once(c) => (c.resolver)().await,
        }
    }
}

/// Decision engine for partial-reload filtering.
///
/// Built from the request's `X-Inertia-Partial-*` headers and the component
/// name of the response being rendered. Per the v3 protocol:
///
/// - If `X-Inertia-Partial-Component` is absent or does not match the
///   response's component, the filter is inactive (treat as a standard
///   visit — no filtering applied).
/// - If `X-Inertia-Partial-Data` is set, treat it as a whitelist.
/// - If `X-Inertia-Partial-Except` is set, treat it as a blacklist that
///   takes precedence over the whitelist on conflicts.
/// - `Always` props bypass this filter (checked by the caller).
/// - `Optional` props use the explicit-only predicate (must be in `only`).
/// - The `errors` prop is always returned (handled by the caller).
#[derive(Debug, Clone, Default)]
pub struct PartialFilter {
    /// True when the request's `X-Inertia-Partial-Component` matched the
    /// response's component. When false, no filtering is applied to
    /// Eager/Lazy props, and Optional props are excluded outright.
    pub matched: bool,
    /// Whitelist of prop keys (parsed from `X-Inertia-Partial-Data`).
    pub only: Option<Vec<String>>,
    /// Blacklist of prop keys (parsed from `X-Inertia-Partial-Except`).
    pub except: Option<Vec<String>>,
}

impl PartialFilter {
    /// Build a filter from the request and the response's component name.
    pub fn build<R: InertiaRequestExt + ?Sized>(req: &R, component: &str) -> Self {
        let partial_component = req.header("X-Inertia-Partial-Component");
        let matched = partial_component
            .map(|c| c == component)
            .unwrap_or(false);

        if !matched {
            return Self::default();
        }

        let parse_csv = |raw: &str| -> Vec<String> {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        Self {
            matched: true,
            only: req.header("X-Inertia-Partial-Data").map(parse_csv),
            except: req.header("X-Inertia-Partial-Except").map(parse_csv),
        }
    }

    /// Whether an Eager-or-Lazy prop with `key` should be included.
    ///
    /// On a non-partial-reload (or partial reload targeting a different
    /// component), inclusion defaults to true. On a matched partial
    /// reload, inclusion follows the only/except rules per the v3 spec
    /// (except wins).
    pub fn should_include_eager(&self, key: &str) -> bool {
        if !self.matched {
            return true;
        }
        let mut included = match &self.only {
            Some(list) => list.iter().any(|k| k == key),
            None => true,
        };
        if included {
            if let Some(except) = &self.except {
                if except.iter().any(|k| k == key) {
                    included = false;
                }
            }
        }
        included
    }

    /// Whether an Optional prop with `key` should be included.
    ///
    /// Per the v3 protocol, Optional props are **never** included on a
    /// standard visit (or a partial reload targeting another component)
    /// and **only** included on a matched partial reload when the key
    /// appears in `X-Inertia-Partial-Data` and not in
    /// `X-Inertia-Partial-Except`.
    pub fn should_include_optional(&self, key: &str) -> bool {
        if !self.matched {
            return false;
        }
        let in_only = match &self.only {
            Some(list) => list.iter().any(|k| k == key),
            None => return false, // Optional requires explicit request
        };
        if !in_only {
            return false;
        }
        if let Some(except) = &self.except {
            if except.iter().any(|k| k == key) {
                return false;
            }
        }
        true
    }

    /// Dispatch the per-variant inclusion predicate.
    ///
    /// `Defer` follows the same "must be explicitly requested" rule as
    /// `Optional`; `Merge` and `Once` follow `Eager`. For `Once`, the
    /// `X-Inertia-Except-Once-Props` header is *not* consulted here —
    /// the caller passes that through to the page-object builder
    /// separately because it interacts with cache-key vs prop-key.
    pub fn should_include(&self, key: &str, prop: &Prop) -> bool {
        match prop {
            Prop::Always(_) => true,
            Prop::Eager(_) | Prop::Lazy(_) | Prop::Merge(_) | Prop::Once(_) => {
                self.should_include_eager(key)
            }
            Prop::Optional(_) | Prop::Defer(_) => self.should_include_optional(key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lazy_resolver(value: Value) -> PropResolver {
        Arc::new(move || {
            let v = value.clone();
            Box::pin(async move { Ok(v) })
        })
    }

    fn failing_resolver() -> PropResolver {
        Arc::new(|| {
            Box::pin(async move { Err(FrameworkError::internal("resolver exploded")) })
        })
    }

    #[test]
    fn filter_inactive_when_component_does_not_match() {
        let filter = PartialFilter::default();
        assert!(!filter.matched);
        assert!(filter.should_include_eager("any_key"));
        // Optional excluded when filter inactive.
        assert!(!filter.should_include_optional("any_key"));
    }

    #[test]
    fn filter_with_only_whitelist() {
        let filter = PartialFilter {
            matched: true,
            only: Some(vec!["users".into(), "events".into()]),
            except: None,
        };
        assert!(filter.should_include_eager("users"));
        assert!(filter.should_include_eager("events"));
        assert!(!filter.should_include_eager("auth"));
    }

    #[test]
    fn filter_with_except_blacklist() {
        let filter = PartialFilter {
            matched: true,
            only: None,
            except: Some(vec!["auth".into()]),
        };
        assert!(filter.should_include_eager("users"));
        assert!(!filter.should_include_eager("auth"));
    }

    #[test]
    fn filter_except_takes_precedence_over_only() {
        let filter = PartialFilter {
            matched: true,
            only: Some(vec!["users".into(), "auth".into()]),
            except: Some(vec!["auth".into()]),
        };
        assert!(filter.should_include_eager("users"));
        assert!(!filter.should_include_eager("auth"));
    }

    #[test]
    fn optional_excluded_on_standard_visit() {
        let filter = PartialFilter::default();
        assert!(!filter.should_include_optional("permissions"));
    }

    #[test]
    fn optional_excluded_when_only_unset_on_partial() {
        // Matched filter, no `only` list — optional must remain excluded
        // because it requires explicit listing.
        let filter = PartialFilter {
            matched: true,
            only: None,
            except: None,
        };
        assert!(!filter.should_include_optional("permissions"));
    }

    #[test]
    fn optional_included_only_when_in_only_list() {
        let filter = PartialFilter {
            matched: true,
            only: Some(vec!["permissions".into()]),
            except: None,
        };
        assert!(filter.should_include_optional("permissions"));
        assert!(!filter.should_include_optional("users"));
    }

    #[test]
    fn optional_excluded_when_in_except_even_if_in_only() {
        let filter = PartialFilter {
            matched: true,
            only: Some(vec!["permissions".into()]),
            except: Some(vec!["permissions".into()]),
        };
        assert!(!filter.should_include_optional("permissions"));
    }

    #[test]
    fn should_include_dispatches_per_variant() {
        let filter = PartialFilter {
            matched: true,
            only: Some(vec!["wanted".into()]),
            except: None,
        };
        let always = Prop::Always(json!(1));
        let eager = Prop::Eager(json!(2));
        let lazy = Prop::Lazy(lazy_resolver(json!(3)));
        let optional = Prop::Optional(lazy_resolver(json!(4)));

        // Always always wins regardless of key
        assert!(filter.should_include("ignored", &always));
        // Eager: in-only -> in, out-of-only -> out
        assert!(filter.should_include("wanted", &eager));
        assert!(!filter.should_include("nope", &eager));
        // Lazy follows Eager
        assert!(filter.should_include("wanted", &lazy));
        assert!(!filter.should_include("nope", &lazy));
        // Optional: in-only -> in, otherwise out
        assert!(filter.should_include("wanted", &optional));
        assert!(!filter.should_include("nope", &optional));
    }

    #[tokio::test]
    async fn prop_resolve_eager() {
        let p = Prop::Eager(json!({"hi": 1}));
        let v = p.resolve().await.unwrap();
        assert_eq!(v, json!({"hi": 1}));
    }

    #[tokio::test]
    async fn prop_resolve_always() {
        let p = Prop::Always(json!("yo"));
        let v = p.resolve().await.unwrap();
        assert_eq!(v, json!("yo"));
    }

    #[tokio::test]
    async fn prop_resolve_lazy_awaits_closure() {
        let p = Prop::Lazy(lazy_resolver(json!([1, 2, 3])));
        let v = p.resolve().await.unwrap();
        assert_eq!(v, json!([1, 2, 3]));
    }

    #[tokio::test]
    async fn prop_resolve_optional_awaits_closure() {
        let p = Prop::Optional(lazy_resolver(json!({"perm": "read"})));
        let v = p.resolve().await.unwrap();
        assert_eq!(v, json!({"perm": "read"}));
    }

    #[tokio::test]
    async fn prop_resolve_propagates_resolver_error() {
        let p = Prop::Lazy(failing_resolver());
        let err = p.resolve().await.unwrap_err();
        assert!(err.to_string().contains("resolver exploded"));
    }

    #[test]
    fn prop_marker_predicates() {
        assert!(Prop::Always(json!(1)).is_always());
        assert!(!Prop::Eager(json!(1)).is_always());

        assert!(Prop::Optional(lazy_resolver(json!(1))).is_optional());
        assert!(!Prop::Lazy(lazy_resolver(json!(1))).is_optional());

        assert!(Prop::Lazy(lazy_resolver(json!(1))).is_deferred());
        assert!(Prop::Optional(lazy_resolver(json!(1))).is_deferred());
        assert!(!Prop::Eager(json!(1)).is_deferred());
        assert!(!Prop::Always(json!(1)).is_deferred());
    }
}
