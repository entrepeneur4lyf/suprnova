use super::config::{Frontend, InertiaConfig};
use super::flash;
use super::prop::{
    DeferConfig, DeferOptions, InertiaRequestExt, MergeConfig, MergeStrategy, OnceConfig,
    OnceOptions, PartialFilter, Prop, PropResolver, ScrollConfig, ScrollMetadata,
};
use crate::container::App;
use crate::csrf::csrf_token;
use crate::error::FrameworkError;
use crate::http::HttpResponse;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Pinned boxed task future used when resolving lazy Inertia props.
type TaskFuture = Pin<Box<dyn Future<Output = Result<TaskOutcome, FrameworkError>> + Send>>;

/// A single prop entry returned by `#[derive(Data)]`'s `__into_inertia_props`.
///
/// - `Eager` — the field's value is already serialized; inserted directly
///   into the response prop bag.
/// - `LazyOwned` — standard lazy / `#[data(lazy)]` / `#[data(lazy(inertia))]`.
///   Must pass the `?include=` + allowlist gate before resolution.
/// - `DeferredOwned` — `#[data(lazy(deferred))]`. Same `?include=` gate as
///   `LazyOwned`; the variant tag signals Inertia deferred-props protocol to
///   the client (follow-up XHR). For v1, resolved via the same code path as
///   `LazyOwned`.
/// - `ClosureOwned` — `#[data(lazy(closure))]`. Same `?include=` gate for v1;
///   future releases will resolve eagerly on the initial visit. The variant
///   tag is preserved for downstream protocol differentiation.
#[derive(Debug)]
pub enum PropEntry {
    Eager(serde_json::Value),
    LazyOwned {
        owner: &'static str,
        field: &'static str,
        prop: Prop,
    },
    DeferredOwned {
        owner: &'static str,
        field: &'static str,
        prop: Prop,
    },
    ClosureOwned {
        owner: &'static str,
        field: &'static str,
        prop: Prop,
    },
}

/// Marker trait implemented by `#[derive(Data)]`-derived types so
/// `Inertia::data` can dispatch on them. Carries the macro-generated
/// `__into_inertia_props` surface — users should not implement this
/// manually.
pub trait IntoInertiaData {
    fn __into_inertia_props(self) -> Vec<(String, PropEntry)>;
}

/// Builder for Inertia.js page responses.
///
/// Construct with a component name, attach props with [`with`](Self::with),
/// [`always`](Self::always), [`lazy`](Self::lazy), [`optional`](Self::optional),
/// [`defer`](Self::defer), [`merge`](Self::merge), [`once`](Self::once), or
/// [`flash`](Self::flash). Optionally set a page title or override the
/// [`InertiaConfig`]. Then call [`resolve`](Self::resolve) with the current
/// request to produce an [`HttpResponse`].
pub struct InertiaResponse {
    component: String,
    props: IndexMap<String, Prop>,
    flash: serde_json::Map<String, Value>,
    config: InertiaConfig,
    title: Option<String>,
    /// Per-response history-encryption override. `Some(true)` forces
    /// encryption on, `Some(false)` forces off, `None` defers to the
    /// middleware task-local + config default. Maps to
    /// `Inertia::encryptHistory($bool)`.
    encrypt_history: Option<bool>,
    /// When `true`, the page object carries `clearHistory: true` so the
    /// client rotates its history-encryption key. Maps to
    /// `Inertia::clearHistory()`.
    clear_history: bool,
    /// Per-response override for the `preserveFragment` page-object
    /// flag. `None` defers to the session-flash flag set by
    /// `Redirect::preserve_fragment()`; `Some(true)` forces on;
    /// `Some(false)` forces off, defeating any inbound flashed `true`.
    /// Maps to `Inertia::preserveFragment()` per-response, with the
    /// session-flash mechanism mirroring Laravel's
    /// `redirect()->preserveFragment()` chainable.
    preserve_fragment: Option<bool>,
    /// Sidecar map for props registered via `prop_lazy_with_owner`.
    /// Maps the prop key to `(owner_struct_name, field_name)` so that
    /// `resolve_props` can call `Prop::resolve_with_owner` instead of
    /// the plain `Prop::Lazy` path. Keyed by the same string as `props`.
    lazy_owned: IndexMap<String, (&'static str, &'static str)>,
}

impl InertiaResponse {
    /// Begin a new Inertia response for the given page component.
    pub fn new(component: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            props: IndexMap::new(),
            flash: serde_json::Map::new(),
            config: InertiaConfig::default(),
            title: None,
            encrypt_history: None,
            clear_history: false,
            preserve_fragment: None,
            lazy_owned: IndexMap::new(),
        }
    }

    /// Override the default [`InertiaConfig`] for this response.
    pub fn with_config(mut self, config: InertiaConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the `<title>` for the HTML shell on this response.
    ///
    /// On Inertia XHR responses the title is ignored — `<Head>` on the
    /// client manages document title for SPA visits. The configured title
    /// is only used for the initial HTML render.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Attach an eager prop. Honors partial-reload filtering per the v3
    /// protocol — when the client sends `X-Inertia-Partial-Data` matching
    /// the same component, this key is included only if it's in that list
    /// (and not in `X-Inertia-Partial-Except`).
    pub fn with<V: Serialize>(mut self, key: impl Into<String>, value: V) -> Self {
        let v = to_value_or_die(&value);
        self.props.insert(key.into(), Prop::Eager(v));
        self
    }

    /// Attach an always-included prop. Bypasses partial-reload filtering —
    /// always returned in the response, even when the client requested a
    /// narrower set. Maps to Laravel's `Inertia::always($value)`.
    pub fn always<V: Serialize>(mut self, key: impl Into<String>, value: V) -> Self {
        let v = to_value_or_die(&value);
        self.props.insert(key.into(), Prop::Always(v));
        self
    }

    /// Attach a lazy prop. The async closure runs only when the prop will
    /// actually be sent to the client — typically once on the initial visit
    /// or when explicitly requested via `X-Inertia-Partial-Data`. Maps to
    /// Laravel's `fn () => ...` prop pattern.
    pub fn lazy<F, Fut, V>(mut self, key: impl Into<String>, resolver: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        let resolver = make_resolver(resolver);
        self.props.insert(key.into(), Prop::Lazy(resolver));
        self
    }

    /// Attach a lazy prop owned by a `#[derive(Data)]` DTO.
    ///
    /// The prop key is `field` (they are always identical in the DTO pattern).
    /// During resolution the `RequestIncludeSet` task-local is consulted via
    /// `Prop::resolve_with_owner`: the closure runs only when `field` appears
    /// in `?include=` AND is in the DTO's allowlist. Returns `400` to the
    /// client if the include set asks for a field not in the allowlist.
    ///
    /// Composition with `X-Inertia-Partial-Data`: partial-data is applied as
    /// a pre-resolution gate (the existing `should_include_eager` check), so
    /// the include-set gate and the partial-data filter compose correctly —
    /// a field must pass both to be resolved and returned.
    pub fn prop_lazy_with_owner(
        mut self,
        owner_struct_name: &'static str,
        field: &'static str,
        prop: Prop,
    ) -> Self {
        self.props.insert(field.to_string(), prop);
        self.lazy_owned.insert(field.to_string(), (owner_struct_name, field));
        self
    }

    /// Build an `InertiaResponse` from the `Vec<(String, PropEntry)>` produced
    /// by a `#[derive(Data)]` DTO's `__into_inertia_props`.
    ///
    /// Dispatches on each entry variant:
    /// - `Eager` → inserted directly via the internal prop map (equivalent to `.with(key, value)`).
    /// - `LazyOwned` → routed through `prop_lazy_with_owner` so the
    ///   `?include=` + allowlist gate applies at resolution time.
    pub fn from_data_props(
        component: &'static str,
        props: Vec<(String, PropEntry)>,
    ) -> Self {
        let mut r = Self::new(component);
        for (k, entry) in props {
            match entry {
                PropEntry::Eager(v) => {
                    r.props.insert(k, Prop::Eager(v));
                }
                PropEntry::LazyOwned { owner, field, prop }
                | PropEntry::DeferredOwned { owner, field, prop }
                | PropEntry::ClosureOwned { owner, field, prop } => {
                    r.props.insert(k, prop);
                    r.lazy_owned.insert(field.to_string(), (owner, field));
                }
            }
        }
        r
    }

    /// Attach an optional prop. Never included on standard visits;
    /// included only when explicitly requested via `X-Inertia-Partial-Data`
    /// on a matching partial reload. Maps to `Inertia::optional(...)`.
    pub fn optional<F, Fut, V>(mut self, key: impl Into<String>, resolver: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        let resolver = make_resolver(resolver);
        self.props.insert(key.into(), Prop::Optional(resolver));
        self
    }

    /// Attach a deferred prop. The resolver is **not** called on the
    /// initial visit; the key is emitted under `deferredProps` so the
    /// client can issue a follow-up partial-reload XHR. On that
    /// follow-up the resolver runs and the value lands in `props`.
    /// Maps to `Inertia::defer(...)`.
    pub fn defer<F, Fut, V>(self, key: impl Into<String>, resolver: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        self.defer_with(key, DeferOptions::default(), resolver)
    }

    /// Attach a deferred prop with explicit options
    /// ([`DeferOptions::group`](crate::DeferOptions::group),
    /// [`DeferOptions::rescue`](crate::DeferOptions::rescue)). Maps to
    /// `Inertia::defer(..., $group)` and `Inertia::defer(..., rescue: true)`.
    pub fn defer_with<F, Fut, V>(
        mut self,
        key: impl Into<String>,
        options: DeferOptions,
        resolver: F,
    ) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        let resolver = make_resolver(resolver);
        self.props.insert(
            key.into(),
            Prop::Defer(DeferConfig {
                resolver,
                group: options.group,
                rescue: options.rescue,
            }),
        );
        self
    }

    /// Attach a mergeable prop with an eager value (append-at-root). The
    /// value lands in `props` AND the key is emitted under `mergeProps`
    /// so the client appends into existing client-side state on
    /// partial reloads. Maps to `Inertia::merge($value)`.
    pub fn merge<V: Serialize>(self, key: impl Into<String>, value: V) -> Self {
        self.merge_with(key, value, MergeStrategy::Append { match_on: None })
    }

    /// Attach a prepend-merge prop with an eager value. Maps to
    /// `Inertia::merge($value)->prepend()`.
    pub fn merge_prepend<V: Serialize>(self, key: impl Into<String>, value: V) -> Self {
        self.merge_with(key, value, MergeStrategy::Prepend { match_on: None })
    }

    /// Attach a deep-merge prop with an eager value. Maps to
    /// `Inertia::deepMerge($value)`.
    pub fn deep_merge<V: Serialize>(self, key: impl Into<String>, value: V) -> Self {
        self.merge_with(key, value, MergeStrategy::Deep { match_on: None })
    }

    /// Attach a mergeable prop with explicit strategy (append / prepend /
    /// deep) and optional `match_on` field for diff-merging by key.
    pub fn merge_with<V: Serialize>(
        mut self,
        key: impl Into<String>,
        value: V,
        strategy: MergeStrategy,
    ) -> Self {
        let v = to_value_or_die(&value);
        let resolver = eager_resolver(v);
        self.props
            .insert(key.into(), Prop::Merge(MergeConfig { resolver, strategy }));
        self
    }

    /// Attach a once prop. The resolver runs the first time the client
    /// sees this key; on subsequent visits the client signals it already
    /// has the value via `X-Inertia-Except-Once-Props` and the resolver
    /// is skipped. Maps to `Inertia::once(...)`.
    pub fn once<F, Fut, V>(self, key: impl Into<String>, resolver: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        self.once_with(key, OnceOptions::default(), resolver)
    }

    /// Attach a once prop with explicit options
    /// ([`OnceOptions::until`](crate::OnceOptions::until),
    /// [`OnceOptions::as_key`](crate::OnceOptions::as_key),
    /// [`OnceOptions::fresh`](crate::OnceOptions::fresh)).
    pub fn once_with<F, Fut, V>(
        mut self,
        key: impl Into<String>,
        options: OnceOptions,
        resolver: F,
    ) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        let resolver = make_resolver(resolver);
        let key = key.into();
        let cache_key = options.cache_key.unwrap_or_else(|| key.clone());
        self.props.insert(
            key,
            Prop::Once(OnceConfig {
                resolver,
                cache_key,
                expires_at: options.expires_at,
                fresh: options.fresh,
            }),
        );
        self
    }

    /// Attach an infinite-scroll prop with an eager value. The
    /// framework normalizes the data shape: the value lands in `props`
    /// and the pagination metadata is emitted under `scrollProps`. The
    /// client's `<InfiniteScroll>` component reads both to drive
    /// next/previous fetches.
    ///
    /// On fresh visits (no `X-Inertia-Infinite-Scroll-Merge-Intent`
    /// header), `scrollProps[key].reset` is `true` so the client clears
    /// its accumulator. On subsequent fetches, the merge direction is
    /// driven by the header (`append` / `prepend`).
    ///
    /// **Conflict semantics:**
    /// - When the client sends both `X-Inertia-Reset` AND
    ///   `X-Inertia-Infinite-Scroll-Merge-Intent` for this key, the
    ///   scroll intent wins (intent → merge direction; reset=false).
    ///   The two headers come from different client flows in practice
    ///   and shouldn't both be set for the same prop.
    /// - Calling both `.scroll(key, ...)` and `.merge(key, ...)` /
    ///   `.with(key, ...)` for the same key is undefined; the
    ///   builder's `IndexMap` keeps the last write, silently
    ///   discarding the earlier prop. Don't.
    ///
    /// Maps to Laravel's `Inertia::scroll(...)`.
    pub fn scroll<V: Serialize>(
        self,
        key: impl Into<String>,
        metadata: ScrollMetadata,
        value: V,
    ) -> Self {
        let v = to_value_or_die(&value);
        let resolver = eager_resolver(v);
        self.attach_scroll(key.into(), metadata, resolver)
    }

    /// Attach an infinite-scroll prop whose value is produced by an
    /// async resolver. Useful when the paginated data requires a DB
    /// query or other async work — common for real scroll loaders.
    pub fn scroll_with<F, Fut, V>(
        self,
        key: impl Into<String>,
        metadata: ScrollMetadata,
        resolver: F,
    ) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        let resolver = make_resolver(resolver);
        self.attach_scroll(key.into(), metadata, resolver)
    }

    fn attach_scroll(
        mut self,
        key: String,
        metadata: ScrollMetadata,
        resolver: PropResolver,
    ) -> Self {
        self.props
            .insert(key, Prop::Scroll(ScrollConfig { resolver, metadata }));
        self
    }

    /// Attach a paginator (`LengthAwarePaginator` or `CursorPaginator`)
    /// as a scroll prop under `key`. The paginator's metadata becomes
    /// the prop's `ScrollMetadata`; its rows become the prop value.
    ///
    /// Equivalent to `.scroll(key, paginator.into_inertia_scroll().0, paginator.into_inertia_scroll().1)`,
    /// but reads better at the call site.
    pub fn paginate<T>(
        self,
        key: &'static str,
        paginator: impl crate::pagination::IntoInertiaScroll<T>,
    ) -> Self
    where
        T: Serialize + 'static,
    {
        let (meta, data) = paginator.into_inertia_scroll();
        self.scroll(key, meta, data)
    }

    /// Attach a flash value to this response. Appears under the
    /// top-level `flash` field of the page object (not under `props`).
    /// Use for one-shot toasts / success messages.
    pub fn flash<V: Serialize>(mut self, key: impl Into<String>, value: V) -> Self {
        let v = to_value_or_die(&value);
        self.flash.insert(key.into(), v);
        self
    }

    /// Force history encryption on or off for this response. Overrides
    /// both [`EncryptHistoryMiddleware`](crate::EncryptHistoryMiddleware)
    /// and [`InertiaConfig::encrypt_history_default`](crate::InertiaConfig::encrypt_history_default).
    /// Maps to `Inertia::encryptHistory($bool)`.
    pub fn encrypt_history(mut self, on: bool) -> Self {
        self.encrypt_history = Some(on);
        self
    }

    /// Mark this response so the client rotates its history-encryption
    /// key. Subsequent attempts to decrypt prior history entries fail
    /// and the client refetches them. Maps to `Inertia::clearHistory()`.
    pub fn clear_history(mut self) -> Self {
        self.clear_history = true;
        self
    }

    /// Set the `preserveFragment` flag on the page object. When the
    /// client receives a page with this flag set, it carries the URL
    /// fragment (`#anchor`) over to the new URL when this page is the
    /// destination of a redirect.
    ///
    /// Precedence: per-response wins over the session-flash flag set
    /// by [`Redirect::preserve_fragment`](crate::Redirect::preserve_fragment).
    /// Specifically, `.preserve_fragment(false)` defeats an inbound
    /// flashed `true`, so a destination controller can opt out of the
    /// fragment carry even when the redirect requested it.
    pub fn preserve_fragment(mut self, on: bool) -> Self {
        self.preserve_fragment = Some(on);
        self
    }

    /// Build a `409 Conflict` external-redirect response. The client
    /// performs `window.location = url`, doing a full page navigation
    /// (not an Inertia SPA visit). Maps to `Inertia::location($url)`.
    ///
    /// **When to use which redirect form:**
    /// - [`Redirect::to`](crate::Redirect::to) — standard 302/303 with
    ///   `Location` header. The normal case for redirects after form
    ///   submission inside the Inertia app.
    /// - [`InertiaResponse::redirect`](Self::redirect) — 409 +
    ///   `X-Inertia-Redirect` for soft Inertia SPA navigation; use
    ///   when the redirect must carry a `#fragment` (server `Location`
    ///   headers can't carry fragments through Inertia XHR).
    /// - [`InertiaResponse::location`](Self::location) — 409 +
    ///   `X-Inertia-Location` for full-page reload via
    ///   `window.location`; use to leave the Inertia app entirely.
    pub fn location(url: impl AsRef<str>) -> HttpResponse {
        HttpResponse::new()
            .status(409)
            .header("X-Inertia-Location", url.as_ref())
    }

    /// Build a `409 Conflict` Inertia-soft-redirect response. The client
    /// performs an Inertia SPA visit (not a full page navigation) to the
    /// target URL. The URL may include a `#fragment` which the client
    /// will land at after the visit. Counterpart to
    /// [`location`](Self::location) for the case where the redirect
    /// target is still inside the Inertia app.
    ///
    /// Maps to the Inertia v3 `X-Inertia-Redirect` protocol header.
    /// For standard server-side redirects (no fragment, plain
    /// post-form-submission) use [`Redirect::to`](crate::Redirect::to)
    /// instead — the auto-303 middleware will rewrite 302→303 for non-GET.
    pub fn redirect(url: impl AsRef<str>) -> HttpResponse {
        HttpResponse::new()
            .status(409)
            .header("X-Inertia-Redirect", url.as_ref())
    }

    /// Internal helper used by the `inertia_response!` macro to unfold a
    /// typed `Props` struct into individual eager props without re-serializing.
    ///
    /// Not part of the stable public API.
    #[doc(hidden)]
    pub fn __add_eager(&mut self, key: String, value: Value) {
        self.props.insert(key, Prop::Eager(value));
    }

    /// Resolve the builder into an [`HttpResponse`] using request state.
    ///
    /// Async because Lazy / Optional / Defer / Merge / Once props may
    /// run DB queries or other futures inside their resolvers.
    ///
    /// - When the request has `X-Inertia: true`, returns the JSON page
    ///   object response (filtered for partial reloads, with all the
    ///   Tier 2 protocol fields populated).
    /// - Otherwise returns the HTML shell with the JSON page object
    ///   embedded in the mount node's `data-page` attribute.
    pub async fn resolve<R: InertiaRequestExt>(
        self,
        req: &R,
    ) -> Result<HttpResponse, FrameworkError> {
        let url = req.path().to_string();
        let is_inertia_request = req.is_inertia();
        let filter = PartialFilter::build(req, &self.component);
        let except_once: Vec<String> = parse_csv_header(req, "X-Inertia-Except-Once-Props");
        // `X-Inertia-Reset` lists merge-prop keys the client wants to
        // start fresh from. We resolve their values normally (so the
        // client gets the current data) but omit the merge metadata so
        // the client treats the value as a replacement, not an append.
        // See `inertia-3.1.1/packages/core/src/requestParams.ts`: the
        // client puts reset keys into `only` AND `X-Inertia-Reset`, so
        // the partial filter already guarantees inclusion.
        let reset_keys: Vec<String> = parse_csv_header(req, "X-Inertia-Reset");
        // `X-Inertia-Error-Bag` scopes the `errors` prop under a named
        // bag, so multiple forms on a page can have isolated validation
        // errors. `errors: {}` becomes `errors: { bag_name: {} }`. When
        // validation parity wires real errors in, this is where they
        // get scoped.
        let error_bag: Option<String> = req
            .header("X-Inertia-Error-Bag")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        // `X-Inertia-Infinite-Scroll-Merge-Intent` tells the server
        // whether a follow-up infinite-scroll fetch wants the new chunk
        // appended or prepended to the existing accumulator. When the
        // header is absent this is a fresh visit and the scroll prop
        // emits `reset: true` so the client clears state.
        let scroll_intent: Option<String> = req
            .header("X-Inertia-Infinite-Scroll-Merge-Intent")
            .map(|s| s.trim().to_lowercase())
            .filter(|s| s == "append" || s == "prepend");

        let Self {
            component,
            props,
            flash: response_flash,
            config,
            title,
            encrypt_history,
            clear_history,
            preserve_fragment,
            lazy_owned,
        } = self;

        // History-encryption precedence: per-response override (handler
        // wins) > middleware task_local > config default.
        let resolved_encrypt_history = encrypt_history
            .or_else(flash::encrypt_history_flag)
            .unwrap_or(config.encrypt_history_default);

        // preserve-fragment precedence: per-response override > session
        // flash (set by `Redirect::preserve_fragment()`) > false. The
        // session lookup is a no-op outside a `SessionMiddleware` scope.
        // `get_flash` removes the entry, so the flag is one-shot.
        let flashed_preserve_fragment = crate::session::session_mut(|s| {
            s.get_flash::<bool>("_inertia.preserve_fragment")
        })
        .flatten()
        .unwrap_or(false);
        let resolved_preserve_fragment =
            preserve_fragment.unwrap_or(flashed_preserve_fragment);

        // Layer props in precedence order (later writes override earlier):
        //   1. Static shared registry  (App::inertia_share, App::inertia_share_lazy)
        //   2. Trait-registered shared data (InertiaSharedData::share)
        //   3. User-supplied props attached via the builder
        //
        // Track the union of (1) + (2) as `shared_keys` so the page
        // object can advertise them under `sharedProps` (the client
        // uses this for instant-swap during navigation — see
        // `inertia-3.1.1/packages/core/src/router.ts` `performInstantSwap`).
        let registry = App::inertia_registry();
        let mut merged: IndexMap<String, Prop> = IndexMap::new();
        let mut shared_keys: Vec<String> = Vec::new();
        for (k, v) in registry.snapshot_static()? {
            if !shared_keys.contains(&k) {
                shared_keys.push(k.clone());
            }
            merged.insert(k, v);
        }
        if let Some(provider) = registry.trait_provider()? {
            let trait_shared = provider.share(req).await?;
            for (k, v) in trait_shared {
                if !shared_keys.contains(&k) {
                    shared_keys.push(k.clone());
                }
                merged.insert(k, v);
            }
        }
        for (k, v) in props {
            // Note: when user props override a shared key, we keep the
            // key in `shared_keys` per the Inertia v3 client contract —
            // the client reads the value from `props` (user's override)
            // and uses `sharedProps` only as a key list.
            merged.insert(k, v);
        }

        let (materialized, metadata) = resolve_props(
            merged,
            &filter,
            &except_once,
            &reset_keys,
            error_bag.as_deref(),
            scroll_intent.as_deref(),
            &lazy_owned,
        )
        .await?;

        // Combine response-builder flash + task-local flash bag (App::flash).
        let mut flash = response_flash;
        for (k, v) in flash::drain() {
            flash.insert(k, v);
        }

        let page = build_page_object(
            &component,
            materialized,
            &config,
            url,
            &metadata,
            flash,
            resolved_encrypt_history,
            clear_history,
            resolved_preserve_fragment,
            shared_keys,
        );

        if is_inertia_request {
            Ok(build_json_response(&page))
        } else {
            // SSR runs only for HTML (non-XHR) visits. XHR is a JSON
            // page-object response and never needs prerender.
            let ssr_result =
                super::ssr::render(&config.ssr, req.path(), &page).await?;
            Ok(build_html_response(
                &page,
                &config,
                title.as_deref(),
                ssr_result.as_ref(),
            ))
        }
    }

    /// Build the page object without producing an HTTP response — used by
    /// tests that want to inspect the page object directly.
    #[cfg(test)]
    pub(crate) async fn build_page_object_for_test(
        self,
        url: String,
        filter: &PartialFilter,
    ) -> Value {
        let Self {
            component,
            props,
            flash,
            config,
            title: _,
            encrypt_history,
            clear_history,
            preserve_fragment,
            lazy_owned,
        } = self;
        let (materialized, metadata) =
            resolve_props(props, filter, &[], &[], None, None, &lazy_owned)
                .await
                .expect("test resolver should not fail");
        let resolved_encrypt_history = encrypt_history
            .unwrap_or(config.encrypt_history_default);
        // Test helper doesn't run inside a session scope, so we never
        // pick up a flashed flag here — only the explicit override.
        let resolved_preserve_fragment = preserve_fragment.unwrap_or(false);
        // The test helper does not exercise the shared-data registry.
        let shared_keys: Vec<String> = Vec::new();
        build_page_object(
            &component,
            materialized,
            &config,
            url,
            &metadata,
            flash,
            resolved_encrypt_history,
            clear_history,
            resolved_preserve_fragment,
            shared_keys,
        )
    }

    /// Build a `409 Conflict` response indicating an asset version mismatch.
    /// The client follows `X-Inertia-Location` for a fresh full-page visit.
    pub fn version_conflict(new_url: &str) -> HttpResponse {
        HttpResponse::new()
            .status(409)
            .header("X-Inertia-Location", new_url)
    }
}

/// Accumulator for Inertia v3 page-object metadata fields.
///
/// Each field corresponds to an optional top-level page-object property
/// — `deferredProps`, `rescuedProps`, `mergeProps`, etc. — and stays
/// empty when no props of that flavor are used in the response. The
/// `build_page_object` step only emits non-empty fields, so simple
/// responses keep their JSON small.
#[derive(Default)]
struct PageMetadata {
    deferred: IndexMap<String, Vec<String>>,
    rescued: Vec<String>,
    merge: Vec<String>,
    merge_prepend: Vec<String>,
    deep_merge: Vec<String>,
    match_props_on: Vec<String>,
    once: IndexMap<String, OnceMetadataEntry>,
    /// Infinite-scroll metadata: prop name → its `ScrollProp` payload
    /// (plus a `reset` flag computed from the merge-intent header).
    scroll: IndexMap<String, ScrollMetadataEntry>,
}

struct ScrollMetadataEntry {
    metadata: ScrollMetadata,
    /// `true` when the client should clear its accumulator before
    /// applying this response (no merge-intent header present).
    reset: bool,
}

struct OnceMetadataEntry {
    /// The prop name (key in `props`). May differ from `cache_key`
    /// when the user supplied `OnceOptions::as_key`.
    prop_name: String,
    expires_at: Option<i64>,
}

/// Outcome of a single prop's async resolution. Returned by each task
/// inside `try_join_all` so post-processing can apply the right
/// metadata side-effect.
#[allow(clippy::large_enum_variant)]
enum TaskOutcome {
    Insert {
        key: String,
        value: Value,
    },
    /// Produced by `prop_lazy_with_owner` resolution when the field is not
    /// in the request's `?include=` set. The key is simply omitted from the
    /// response — no error, no null sentinel.
    Skip,
    Rescued {
        key: String,
    },
    Merge {
        key: String,
        value: Value,
        strategy: MergeStrategy,
    },
    Once {
        key: String,
        cache_key: String,
        expires_at: Option<i64>,
        value: Value,
    },
    Scroll {
        key: String,
        value: Value,
        metadata: ScrollMetadata,
        /// `Some("append")` / `Some("prepend")` propagates to mergeProps/
        /// prependProps; `None` is a fresh visit and emits `reset: true`.
        intent: Option<String>,
    },
}

/// Parse a CSV header into a deduped list of trimmed, non-empty values.
fn parse_csv_header<R: InertiaRequestExt>(req: &R, name: &str) -> Vec<String> {
    req.header(name)
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Walk the prop bag, apply per-variant filtering / metadata rules, await
/// resolver closures concurrently, and return both the materialized prop
/// map and the page-object metadata.
///
/// `reset_keys` is the `X-Inertia-Reset` list: merge-prop keys the
/// client wants to start fresh from. For those keys we resolve the
/// value normally but suppress the merge metadata, so the client
/// treats the value as a replacement rather than an append.
async fn resolve_props(
    props: IndexMap<String, Prop>,
    filter: &PartialFilter,
    except_once: &[String],
    reset_keys: &[String],
    error_bag: Option<&str>,
    scroll_intent: Option<&str>,
    lazy_owned: &IndexMap<String, (&'static str, &'static str)>,
) -> Result<(serde_json::Map<String, Value>, PageMetadata), FrameworkError> {
    let mut materialized = serde_json::Map::new();
    let mut metadata = PageMetadata::default();

    // `errors` is always present per the Inertia v3 contract. Seed
    // with an empty object so the key exists even if no resolver
    // writes errors. The `X-Inertia-Error-Bag` wrapping happens AFTER
    // all props resolve — see the bottom of this function. Doing it
    // post-resolution means a handler that injects errors via
    // `.with("errors", {...})` still gets correctly scoped.
    materialized.insert(
        "errors".to_string(),
        Value::Object(serde_json::Map::new()),
    );

    let mut tasks: Vec<TaskFuture> = Vec::new();

    for (key, prop) in props {
        match prop {
            Prop::Always(v) => {
                materialized.insert(key, v);
            }
            Prop::Eager(v) => {
                if filter.should_include_eager(&key) {
                    materialized.insert(key, v);
                }
            }
            // Absent sentinel (from when_loaded! when relation not loaded) —
            // silently skip; no null, no error.
            Prop::EagerNone => {}
            Prop::Lazy(r) => {
                if let Some(&(owner, field)) = lazy_owned.get(&key) {
                    // OWNER-TAGGED LAZY PATH
                    //
                    // Gate order (spec):
                    //   Stage 1 — resolve_with_owner: include-set membership
                    //     check + per-DTO allowlist enforcement. Returns
                    //     Err(400) when the requested field is not on the
                    //     allowlist. This error MUST propagate before
                    //     partial-data can silently swallow it.
                    //   Stage 2 — partial-data filter: applied to the resolved
                    //     Some(v) result as the final "only" gate.
                    //
                    // The previous code had partial-data as the OUTER guard,
                    // which silently dropped disallowed-include errors when
                    // X-Inertia-Partial-Data was narrower than ?include=.
                    let filter_clone = filter.clone();
                    tasks.push(Box::pin(async move {
                        let prop = Prop::Lazy(r);
                        match prop.resolve_with_owner(owner, field).await? {
                            None => Ok(TaskOutcome::Skip), // not in include set
                            Some(v) => {
                                // Stage 2: partial-data is the final "only" filter.
                                if filter_clone.should_include_eager(&key) {
                                    Ok(TaskOutcome::Insert { key, value: v })
                                } else {
                                    Ok(TaskOutcome::Skip)
                                }
                            }
                        }
                    }));
                } else if filter.should_include_eager(&key) {
                    // PLAIN LAZY PATH (no owner tag)
                    // Partial-data is the only gate — existing behavior unchanged.
                    tasks.push(Box::pin(async move {
                        let v = r().await?;
                        Ok(TaskOutcome::Insert { key, value: v })
                    }));
                }
            }
            Prop::Optional(r) => {
                if filter.should_include_optional(&key) {
                    tasks.push(Box::pin(async move {
                        let v = r().await?;
                        Ok(TaskOutcome::Insert { key, value: v })
                    }));
                }
            }
            Prop::Defer(c) => {
                if filter.should_include_optional(&key) {
                    // Partial reload requesting this deferred key — fire
                    // the resolver. Rescue catches errors per spec.
                    let resolver = c.resolver;
                    let rescue = c.rescue;
                    tasks.push(Box::pin(async move {
                        match resolver().await {
                            Ok(v) => Ok(TaskOutcome::Insert { key, value: v }),
                            Err(e) if rescue => {
                                // TODO: log/dispatch to error tracker
                                // (depends on the events parity work).
                                let _ = e;
                                Ok(TaskOutcome::Rescued { key })
                            }
                            Err(e) => Err(e),
                        }
                    }));
                } else {
                    // Initial visit (or partial-reload not requesting this
                    // key) — DON'T resolve; emit in deferredProps so the
                    // client knows to issue a follow-up XHR.
                    metadata
                        .deferred
                        .entry(c.group)
                        .or_default()
                        .push(key);
                }
            }
            Prop::Merge(c) => {
                if filter.should_include_eager(&key) {
                    let resolver = c.resolver;
                    let strategy = c.strategy;
                    // X-Inertia-Reset: when the client asks to reset
                    // this merge key, resolve the value normally but
                    // emit it as a plain `Insert` so no merge metadata
                    // attaches. The client then treats the value as a
                    // replacement, not an append.
                    let is_reset = reset_keys.iter().any(|k| k == &key);
                    tasks.push(Box::pin(async move {
                        let v = resolver().await?;
                        if is_reset {
                            Ok(TaskOutcome::Insert { key, value: v })
                        } else {
                            Ok(TaskOutcome::Merge {
                                key,
                                value: v,
                                strategy,
                            })
                        }
                    }));
                }
            }
            Prop::Scroll(c) => {
                if filter.should_include_eager(&key) {
                    let resolver = c.resolver;
                    let metadata = c.metadata;
                    let intent = scroll_intent.map(|s| s.to_string());
                    tasks.push(Box::pin(async move {
                        let v = resolver().await?;
                        Ok(TaskOutcome::Scroll {
                            key,
                            value: v,
                            metadata,
                            intent,
                        })
                    }));
                }
            }
            Prop::Once(c) => {
                let client_has_cached =
                    !c.fresh && except_once.iter().any(|k| k == &c.cache_key);
                if client_has_cached {
                    // Client already has the value cached — skip resolver
                    // but still emit metadata so the client confirms the
                    // cache key is current.
                    metadata.once.insert(
                        c.cache_key.clone(),
                        OnceMetadataEntry {
                            prop_name: key,
                            expires_at: c.expires_at,
                        },
                    );
                } else if filter.should_include_eager(&key) {
                    let resolver = c.resolver;
                    let cache_key = c.cache_key.clone();
                    let expires_at = c.expires_at;
                    tasks.push(Box::pin(async move {
                        let v = resolver().await?;
                        Ok(TaskOutcome::Once {
                            key,
                            cache_key,
                            expires_at,
                            value: v,
                        })
                    }));
                }
                // else: partial filter excluded — no resolution, no metadata.
            }
        }
    }

    let outcomes = futures::future::try_join_all(tasks).await?;

    for outcome in outcomes {
        match outcome {
            TaskOutcome::Insert { key, value } => {
                materialized.insert(key, value);
            }
            // Field was not in the request's `?include=` set — omit silently.
            TaskOutcome::Skip => {}
            TaskOutcome::Rescued { key } => {
                metadata.rescued.push(key);
            }
            TaskOutcome::Merge {
                key,
                value,
                strategy,
            } => {
                materialized.insert(key.clone(), value);
                apply_merge_strategy(&mut metadata, key, strategy);
            }
            TaskOutcome::Once {
                key,
                cache_key,
                expires_at,
                value,
            } => {
                materialized.insert(key.clone(), value);
                metadata.once.insert(
                    cache_key,
                    OnceMetadataEntry {
                        prop_name: key,
                        expires_at,
                    },
                );
            }
            TaskOutcome::Scroll {
                key,
                value,
                metadata: scroll_meta,
                intent,
            } => {
                materialized.insert(key.clone(), value);
                // Direction of merge: client header drives. No header →
                // fresh visit → no merge metadata + reset: true.
                let reset = intent.is_none();
                match intent.as_deref() {
                    Some("append") => metadata.merge.push(key.clone()),
                    Some("prepend") => metadata.merge_prepend.push(key.clone()),
                    _ => {}
                }
                metadata.scroll.insert(
                    key,
                    ScrollMetadataEntry {
                        metadata: scroll_meta,
                        reset,
                    },
                );
            }
        }
    }

    // `X-Inertia-Error-Bag` scoping. Apply AFTER all props have
    // resolved so a handler-provided `errors` prop (via
    // `.with("errors", {...})`) gets correctly wrapped. Without this
    // post-pass, the seeded empty object would be wrapped here but
    // overwritten by the user prop, silently losing the bag.
    if let Some(bag) = error_bag
        && let Some(errors_val) = materialized.remove("errors")
    {
        let mut wrapper = serde_json::Map::new();
        wrapper.insert(bag.to_string(), errors_val);
        materialized.insert("errors".to_string(), Value::Object(wrapper));
    }

    Ok((materialized, metadata))
}

fn apply_merge_strategy(metadata: &mut PageMetadata, key: String, strategy: MergeStrategy) {
    match strategy {
        MergeStrategy::Append { match_on } => {
            if let Some(m) = match_on {
                metadata.match_props_on.push(format!("{}.{}", key, m));
            }
            metadata.merge.push(key);
        }
        MergeStrategy::Prepend { match_on } => {
            if let Some(m) = match_on {
                metadata.match_props_on.push(format!("{}.{}", key, m));
            }
            metadata.merge_prepend.push(key);
        }
        MergeStrategy::Deep { match_on } => {
            if let Some(m) = match_on {
                metadata.match_props_on.push(format!("{}.{}", key, m));
            }
            metadata.deep_merge.push(key);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_page_object(
    component: &str,
    materialized_props: serde_json::Map<String, Value>,
    config: &InertiaConfig,
    url: String,
    metadata: &PageMetadata,
    flash: serde_json::Map<String, Value>,
    encrypt_history: bool,
    clear_history: bool,
    preserve_fragment: bool,
    shared_keys: Vec<String>,
) -> Value {
    let mut page = serde_json::Map::new();
    page.insert(
        "component".to_string(),
        Value::String(component.to_string()),
    );
    page.insert("props".to_string(), Value::Object(materialized_props));
    page.insert("url".to_string(), Value::String(url));
    page.insert("version".to_string(), Value::String(config.version.resolve()));

    // Per spec, `encryptHistory` / `clearHistory` / `preserveFragment`
    // are only emitted when `true`. Falsy values are omitted to keep
    // the page object lean.
    if encrypt_history {
        page.insert("encryptHistory".to_string(), Value::Bool(true));
    }
    if clear_history {
        page.insert("clearHistory".to_string(), Value::Bool(true));
    }
    if preserve_fragment {
        page.insert("preserveFragment".to_string(), Value::Bool(true));
    }

    if !flash.is_empty() {
        page.insert("flash".to_string(), Value::Object(flash));
    }

    if !metadata.deferred.is_empty() {
        let deferred = metadata
            .deferred
            .iter()
            .map(|(group, keys)| {
                (
                    group.clone(),
                    Value::Array(keys.iter().cloned().map(Value::String).collect()),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        page.insert("deferredProps".to_string(), Value::Object(deferred));
    }
    if !metadata.rescued.is_empty() {
        page.insert(
            "rescuedProps".to_string(),
            Value::Array(metadata.rescued.iter().cloned().map(Value::String).collect()),
        );
    }
    if !metadata.merge.is_empty() {
        page.insert(
            "mergeProps".to_string(),
            Value::Array(metadata.merge.iter().cloned().map(Value::String).collect()),
        );
    }
    if !metadata.merge_prepend.is_empty() {
        page.insert(
            "prependProps".to_string(),
            Value::Array(
                metadata
                    .merge_prepend
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if !metadata.deep_merge.is_empty() {
        page.insert(
            "deepMergeProps".to_string(),
            Value::Array(
                metadata
                    .deep_merge
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if !metadata.match_props_on.is_empty() {
        page.insert(
            "matchPropsOn".to_string(),
            Value::Array(
                metadata
                    .match_props_on
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if !metadata.once.is_empty() {
        let once = metadata
            .once
            .iter()
            .map(|(cache_key, entry)| {
                let mut m = serde_json::Map::new();
                m.insert(
                    "prop".to_string(),
                    Value::String(entry.prop_name.clone()),
                );
                m.insert(
                    "expiresAt".to_string(),
                    entry
                        .expires_at
                        .map(|t| Value::Number(serde_json::Number::from(t)))
                        .unwrap_or(Value::Null),
                );
                (cache_key.clone(), Value::Object(m))
            })
            .collect::<serde_json::Map<_, _>>();
        page.insert("onceProps".to_string(), Value::Object(once));
    }

    // `sharedProps` lists the keys that came from the shared registry
    // (static + trait). The client uses this during instant-swap visits
    // to carry shared values across navigations. Omit when empty so
    // small responses stay small.
    if !shared_keys.is_empty() {
        page.insert(
            "sharedProps".to_string(),
            Value::Array(shared_keys.into_iter().map(Value::String).collect()),
        );
    }

    // `scrollProps` carries infinite-scroll pagination metadata,
    // keyed by prop name. The `reset` flag tells the client whether
    // this response is a fresh page load (clear accumulator) or a
    // follow-up next/previous fetch (preserve accumulator).
    if !metadata.scroll.is_empty() {
        let scroll = metadata
            .scroll
            .iter()
            .map(|(prop_key, entry)| {
                let mut m = serde_json::Map::new();
                m.insert(
                    "pageName".to_string(),
                    Value::String(entry.metadata.page_name.clone()),
                );
                m.insert(
                    "previousPage".to_string(),
                    entry.metadata.previous_page.clone().unwrap_or(Value::Null),
                );
                m.insert(
                    "nextPage".to_string(),
                    entry.metadata.next_page.clone().unwrap_or(Value::Null),
                );
                m.insert(
                    "currentPage".to_string(),
                    entry.metadata.current_page.clone().unwrap_or(Value::Null),
                );
                m.insert("reset".to_string(), Value::Bool(entry.reset));
                (prop_key.clone(), Value::Object(m))
            })
            .collect::<serde_json::Map<_, _>>();
        page.insert("scrollProps".to_string(), Value::Object(scroll));
    }

    Value::Object(page)
}

fn make_resolver<F, Fut, V>(resolver: F) -> PropResolver
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
    V: Serialize + 'static,
{
    Arc::new(move || {
        let fut = resolver();
        Box::pin(async move {
            let value = fut.await?;
            serde_json::to_value(&value).map_err(|e| {
                FrameworkError::internal(format!(
                    "InertiaResponse resolver value failed to serialize: {}",
                    e
                ))
            })
        })
    })
}

/// Wrap an eager `Value` in the closure shape required by [`PropResolver`].
/// Used by the merge / once builder methods that accept eager values but
/// store them in the same async-shaped variant slot for uniform handling.
fn eager_resolver(value: Value) -> PropResolver {
    Arc::new(move || {
        let v = value.clone();
        Box::pin(async move { Ok(v) })
    })
}

fn to_value_or_die<V: Serialize>(value: &V) -> Value {
    // `serde_json::to_value` only fails when the type's Serialize impl
    // errors (the rare case — typically a custom impl that returns Err).
    // For framework consumers this is a bug in their type, so panicking
    // with a clear message is the right call.
    serde_json::to_value(value)
        .expect("InertiaResponse prop value must serialize cleanly; check the type's Serialize impl")
}

fn build_json_response(page: &Value) -> HttpResponse {
    HttpResponse::json(page.clone())
        .header("X-Inertia", "true")
        .header("Vary", "X-Inertia")
}

fn build_html_response(
    page: &Value,
    config: &InertiaConfig,
    title_override: Option<&str>,
    ssr: Option<&super::ssr::SsrResponse>,
) -> HttpResponse {
    let title = title_override.unwrap_or(&config.default_title);
    let page_json = serde_json::to_string(page).unwrap_or_else(|_| "{}".to_string());
    let page_attr = escape_html_attr(&page_json);
    let csrf = csrf_token().unwrap_or_default();
    let csrf_attr = escape_html_attr(&csrf);
    let title_html = escape_html_text(title);

    let head_extras = if config.development {
        render_dev_head(config)
    } else {
        render_prod_head()
    };

    // SSR injection. The worker returns `head` as a list of HTML
    // snippets (title, meta, etc.) and `body` as the prerendered app
    // shell. When present we add `data-server-rendered="true"` so the
    // client hydrates instead of re-rendering.
    let ssr_head = ssr
        .map(|s| s.head.join("\n"))
        .unwrap_or_default();
    let ssr_body = ssr.map(|s| s.body.as_str()).unwrap_or("");
    let ssr_attr = if ssr.is_some() {
        " data-server-rendered=\"true\""
    } else {
        ""
    };

    let html = format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"UTF-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n\
         <meta name=\"csrf-token\" content=\"{csrf}\">\n\
         <title>{title}</title>\n\
         {ssr_head}\
         {head}\
         </head>\n\
         <body>\n\
         <div id=\"app\"{ssr_attr} data-page=\"{page}\">{ssr_body}</div>\n\
         </body>\n\
         </html>",
        csrf = csrf_attr,
        title = title_html,
        ssr_head = ssr_head,
        head = head_extras,
        page = page_attr,
        ssr_attr = ssr_attr,
        ssr_body = ssr_body,
    );

    HttpResponse::html(html).header("Vary", "X-Inertia")
}

fn render_dev_head(config: &InertiaConfig) -> String {
    // React requires the `@react-refresh` preamble before any module loads;
    // Svelte and Vue have HMR built into their Vite plugins and don't need
    // any extra preamble script.
    let preamble = match config.frontend {
        Frontend::React => format!(
            "<script type=\"module\">\n\
             import RefreshRuntime from '{server}/@react-refresh'\n\
             RefreshRuntime.injectIntoGlobalHook(window)\n\
             window.$RefreshReg$ = () => {{}}\n\
             window.$RefreshSig$ = () => (type) => type\n\
             window.__vite_plugin_react_preamble_installed__ = true\n\
             </script>\n",
            server = config.vite_dev_server,
        ),
        Frontend::Svelte | Frontend::Vue => String::new(),
    };

    format!(
        "{preamble}\
         <script type=\"module\" src=\"{server}/@vite/client\"></script>\n\
         <script type=\"module\" src=\"{server}/{entry}\"></script>\n",
        preamble = preamble,
        server = config.vite_dev_server,
        entry = config.entry_point,
    )
}

fn render_prod_head() -> String {
    "<script type=\"module\" src=\"/assets/main.js\"></script>\n\
     <link rel=\"stylesheet\" href=\"/assets/main.css\">\n"
        .to_string()
}

fn escape_html_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn escape_html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn build_page_object_eager_only() {
        let resp = InertiaResponse::new("Home")
            .with("title", "Welcome")
            .with("count", 42u32);

        let filter = PartialFilter::default();
        let page = resp
            .build_page_object_for_test("/home".into(), &filter)
            .await;

        let obj = page.as_object().unwrap();
        assert_eq!(obj["component"], Value::String("Home".into()));
        assert_eq!(obj["url"], Value::String("/home".into()));
        assert_eq!(obj["version"], Value::String("1.0".into()));

        let props = obj["props"].as_object().unwrap();
        assert_eq!(props["title"], Value::String("Welcome".into()));
        assert_eq!(props["count"], Value::Number(42.into()));
        assert!(props["errors"].is_object());
    }

    #[tokio::test]
    async fn always_bypasses_filter() {
        let resp = InertiaResponse::new("Users")
            .with("users", json!([]))
            .always("flash", json!({"msg": "hi"}));

        let filter = PartialFilter {
            matched: true,
            only: Some(vec!["users".into()]),
            except: None,
        };
        let page = resp
            .build_page_object_for_test("/users".into(), &filter)
            .await;

        let props = page["props"].as_object().unwrap();
        assert!(props.contains_key("users"));
        assert!(props.contains_key("flash"));
    }

    #[tokio::test]
    async fn version_conflict_response_shape() {
        let r = InertiaResponse::version_conflict("/new-url");
        let hyper_resp = r.into_hyper();
        assert_eq!(hyper_resp.status(), 409);
        assert_eq!(
            hyper_resp.headers().get("X-Inertia-Location").unwrap(),
            "/new-url"
        );
    }

    #[test]
    fn html_escape_handles_critical_chars() {
        let attr = escape_html_attr(r#"a&b<c>d"e'f"#);
        assert_eq!(attr, "a&amp;b&lt;c&gt;d&quot;e&#x27;f");

        let text = escape_html_text("<script>");
        assert_eq!(text, "&lt;script&gt;");
    }

    #[test]
    fn dev_head_includes_react_preamble_for_react_only() {
        let cfg = InertiaConfig::new().frontend(Frontend::React);
        let head = render_dev_head(&cfg);
        assert!(head.contains("@react-refresh"));
        assert!(head.contains("__vite_plugin_react_preamble_installed__"));

        let cfg = InertiaConfig::new().frontend(Frontend::Svelte);
        let head = render_dev_head(&cfg);
        assert!(!head.contains("@react-refresh"));

        let cfg = InertiaConfig::new().frontend(Frontend::Vue);
        let head = render_dev_head(&cfg);
        assert!(!head.contains("@react-refresh"));
    }

    #[test]
    fn dev_head_loads_correct_entry_point_per_frontend() {
        let cfg = InertiaConfig::new().frontend(Frontend::Svelte);
        let head = render_dev_head(&cfg);
        assert!(head.contains("src/main.ts"));
        assert!(!head.contains("src/main.tsx"));

        let cfg = InertiaConfig::new().frontend(Frontend::React);
        let head = render_dev_head(&cfg);
        assert!(head.contains("src/main.tsx"));

        let cfg = InertiaConfig::new().frontend(Frontend::Vue);
        let head = render_dev_head(&cfg);
        assert!(head.contains("src/main.ts"));
    }

    #[tokio::test]
    async fn flash_emits_top_level_field() {
        let resp = InertiaResponse::new("Home").flash("toast", json!({"msg": "saved"}));
        let page = resp
            .build_page_object_for_test("/".into(), &PartialFilter::default())
            .await;
        let obj = page.as_object().unwrap();
        assert!(obj.contains_key("flash"));
        assert_eq!(obj["flash"]["toast"], json!({"msg": "saved"}));
    }

    #[tokio::test]
    async fn flash_field_absent_when_empty() {
        let resp = InertiaResponse::new("Home");
        let page = resp
            .build_page_object_for_test("/".into(), &PartialFilter::default())
            .await;
        let obj = page.as_object().unwrap();
        assert!(!obj.contains_key("flash"));
    }

    #[tokio::test]
    async fn defer_initial_visit_emits_deferred_props_no_resolve() {
        // Defer key NOT in partial-data → not resolved, emitted in
        // deferredProps under the default group.
        let resp = InertiaResponse::new("Users").defer("permissions", || async {
            // Should not run on initial visit. The Result type annotation
            // is required because Rust can't infer V from a never-resolved
            // future.
            #[allow(unreachable_code)]
            Ok::<Value, FrameworkError>({
                panic!("defer resolver should not run on initial visit");
            })
        });
        let page = resp
            .build_page_object_for_test("/".into(), &PartialFilter::default())
            .await;

        let obj = page.as_object().unwrap();
        assert!(obj["deferredProps"].is_object());
        let deferred = obj["deferredProps"].as_object().unwrap();
        let default_group = deferred["default"].as_array().unwrap();
        assert_eq!(default_group.len(), 1);
        assert_eq!(default_group[0], json!("permissions"));
        // And the prop is NOT in props.
        let props = obj["props"].as_object().unwrap();
        assert!(!props.contains_key("permissions"));
    }

    #[tokio::test]
    async fn merge_emits_merge_props_with_match_on() {
        let resp = InertiaResponse::new("Posts").merge_with(
            "posts",
            json!([{"id": 1}]),
            MergeStrategy::Append {
                match_on: Some("id".into()),
            },
        );
        let page = resp
            .build_page_object_for_test("/".into(), &PartialFilter::default())
            .await;

        let obj = page.as_object().unwrap();
        assert_eq!(obj["mergeProps"], json!(["posts"]));
        assert_eq!(obj["matchPropsOn"], json!(["posts.id"]));
        assert_eq!(obj["props"]["posts"], json!([{"id": 1}]));
    }

    #[tokio::test]
    async fn deep_merge_emits_deep_merge_props() {
        let resp = InertiaResponse::new("Chat").deep_merge("chat", json!({"messages": []}));
        let page = resp
            .build_page_object_for_test("/".into(), &PartialFilter::default())
            .await;

        let obj = page.as_object().unwrap();
        assert_eq!(obj["deepMergeProps"], json!(["chat"]));
    }

    #[tokio::test]
    async fn preserve_fragment_true_emits_flag() {
        let resp = InertiaResponse::new("Article").preserve_fragment(true);
        let page = resp
            .build_page_object_for_test("/article/new".into(), &PartialFilter::default())
            .await;
        let obj = page.as_object().unwrap();
        assert_eq!(obj["preserveFragment"], Value::Bool(true));
    }

    #[tokio::test]
    async fn preserve_fragment_default_omits_flag() {
        let resp = InertiaResponse::new("Article");
        let page = resp
            .build_page_object_for_test("/article".into(), &PartialFilter::default())
            .await;
        assert!(!page.as_object().unwrap().contains_key("preserveFragment"));
    }

    #[tokio::test]
    async fn preserve_fragment_false_omits_flag() {
        let resp = InertiaResponse::new("Article").preserve_fragment(false);
        let page = resp
            .build_page_object_for_test("/article".into(), &PartialFilter::default())
            .await;
        assert!(!page.as_object().unwrap().contains_key("preserveFragment"));
    }

    #[tokio::test]
    async fn redirect_response_shape() {
        let r = InertiaResponse::redirect("/articles/new#section");
        let hyper_resp = r.into_hyper();
        assert_eq!(hyper_resp.status(), 409);
        assert_eq!(
            hyper_resp.headers().get("X-Inertia-Redirect").unwrap(),
            "/articles/new#section"
        );
        // Distinct from `location`: must NOT carry X-Inertia-Location.
        assert!(hyper_resp.headers().get("X-Inertia-Location").is_none());
    }
}
