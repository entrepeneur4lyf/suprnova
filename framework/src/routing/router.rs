//! HTTP `Router`, route-name registry, and per-method matching.
//!
//! ## Route names are process-global
//!
//! Suprnova stores named-route bindings in a single
//! `OnceLock<RwLock<HashMap<String, String>>>` — there is one
//! `name → path` table per process, not per [`Router`]. Two
//! consequences worth knowing:
//!
//! 1. **Suprnova supports one [`Router`] per process.**
//!    `Server::from_config` consumes a single `Router`, and the
//!    Laravel-shaped `route("users.show", &[("id", "42")])` helper
//!    resolves through the process-global table without a `Router`
//!    reference — that's the ergonomic call sites depend on
//!    (`http::response::Redirect::route`, handler-side
//!    `suprnova::route`, templates). If you need two isolated
//!    route-name spaces in one process (multi-tenant subapps,
//!    hot-reload of a competing router), that's a deliberate gap;
//!    file an issue and we'll add a per-Router registry surface.
//!
//! 2. **Names must be globally unique.** [`register_route_name`]
//!    panics when two routes claim the same name under different
//!    paths; [`try_register_route_name`] returns
//!    `Err(FrameworkError)` instead. Re-registering the same
//!    `(name, path)` pair is idempotent, so inventory-driven
//!    registration (e.g. `routes!{}` inside a `register()` called
//!    more than once) doesn't panic.
//!
//! Parallel tests should pick unique names per test
//! (`users.show.encoding`, `users.show.frag`, …) — the inline tests
//! in this module follow that convention. Tests that want a clean
//! slate can call [`clear_route_names_for_test`]; the matchit
//! per-method routes themselves live on the `Router` you build per
//! test, only the name table is process-global.

use crate::FrameworkError;
use crate::http::{Request, Response};
use crate::middleware::{BoxedMiddleware, Middleware, into_boxed};
use crate::ws::BoxedWebSocketHandler;
use hyper::Method;
use matchit::Router as MatchitRouter;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

/// Process-global registry mapping route names to path patterns.
///
/// See the module-level docstring for the one-Router-per-process
/// rationale and the test-isolation utility
/// [`clear_route_names_for_test`].
static ROUTE_REGISTRY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Drain every entry from the process-global route-name registry.
///
/// Public test utility for tests that register named routes against
/// the process-global table and want a clean slate between cases.
/// Mirrors the [`crate::events::EventDispatcher::clear`] and
/// [`crate::eloquent::scope::ScopeRegistry::clear`] precedent for
/// process-global state that other tests rely on across the
/// framework.
///
/// **Concurrency:** the registry is shared across every thread.
/// Tests that call this must either run with `#[serial_test::serial]`
/// or coordinate their own ordering; concurrent parallel tests
/// calling `clear` would see each other's effects.
///
/// **Production:** the function is callable in production too, but
/// Suprnova boots its named routes once at startup — the only
/// intended call site is tests. The `_for_test` suffix signals
/// that intent.
///
/// A poisoned write lock is recovered in place (matches
/// [`register_route_name`]'s policy) so a panic during one thread's
/// registration doesn't permanently disable the clear hook.
pub fn clear_route_names_for_test() {
    if let Some(lock) = ROUTE_REGISTRY.get() {
        match lock.write() {
            Ok(mut map) => map.clear(),
            Err(poisoned) => poisoned.into_inner().clear(),
        }
    }
}

/// Characters that must be percent-encoded when substituting a value
/// into a route pattern segment. The set covers the gen-delims and
/// sub-delims from RFC 3986 (`/ ? # [ ] @ ! $ & ' ( ) * + , ; =`) plus
/// the unsafe characters (space, `"`, `<`, `>`, `\`, `^`, `` ` ``, `{`,
/// `|`, `}`, `%`) and ASCII control codes.
///
/// Unreserved characters (`A-Z a-z 0-9 - _ . ~`) pass through unchanged,
/// matching what a browser sends for a URL-safe path segment.
const PATH_SEGMENT_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'\\')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'%')
    // gen-delims
    .add(b':')
    .add(b'/')
    .add(b'?')
    .add(b'#')
    .add(b'[')
    .add(b']')
    .add(b'@')
    // sub-delims
    .add(b'!')
    .add(b'$')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b';')
    .add(b'=');

/// Register a route name -> path mapping.
///
/// # Panics
///
/// Panics if `name` is already registered under a different `path`.
/// Two routes resolving the same name silently shadowing each other is
/// a security-shaped bug — redirects and named-URL helpers would route
/// to whichever happened to win the registration race. Route names
/// must be unique; collisions fail loudly at boot.
///
/// Re-registering the same `(name, path)` pair is a no-op (idempotent).
/// Poisoned write locks are recovered via `PoisonError::into_inner` —
/// a panic during one thread's registration must not silently make
/// every subsequent name lookup return `None`.
///
/// Use [`try_register_route_name`] to get an `Err(FrameworkError)` instead
/// of a panic when names come from a fallible source (dynamic config,
/// plugins).
pub fn register_route_name(name: &str, path: &str) {
    try_register_route_name(name, path).unwrap_or_else(|e| panic!("{e}"));
}

/// Fallible sibling of [`register_route_name`]: returns `Err(FrameworkError)`
/// (naming the conflicting name and the path it is already bound to) instead
/// of panicking when `name` is already registered to a *different* `path`.
///
/// Re-registering the same `(name, path)` pair stays a no-op `Ok(())`
/// (idempotent). Poisoned write locks are recovered in place, matching
/// [`register_route_name`]. Backs [`RouteBuilder::try_name`].
pub fn try_register_route_name(name: &str, path: &str) -> Result<(), FrameworkError> {
    let registry = ROUTE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()));
    let mut map = match registry.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = map.get(name)
        && existing != path
    {
        return Err(FrameworkError::internal(format!(
            "Route name '{name}' is already registered to path '{existing}'; \
             refusing to re-register to '{path}'. Route names must be unique \
             across the application — rename one of the routes.",
        )));
    }
    map.insert(name.to_string(), path.to_string());
    Ok(())
}

fn lookup_route(name: &str) -> Option<String> {
    let lock = ROUTE_REGISTRY.get()?;
    let map = match lock.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.get(name).cloned()
}

/// Decode the captured `(key, value)` pairs from a successful
/// `matchit` match.
///
/// matchit captures raw segment bytes from `hyper::Uri::path()`, which
/// is the already-percent-encoded request target. The router matches
/// against that raw form so encoded segment delimiters (`%2F`) stay
/// inside a single segment instead of being misinterpreted as a path
/// separator — that's the right matching policy and must NOT change.
/// Handlers however expect Laravel-shaped, decoded values:
/// `route("posts.show", &[("slug", "a/b")])` percent-encodes to
/// `/posts/a%2Fb`, and the handler that receives that request must
/// see `"a/b"` from `req.param("slug")` (round-trip).
///
/// Decoding is lossy on invalid UTF-8 (percent-encoded bytes that do
/// not form a valid UTF-8 sequence become the Unicode replacement
/// character via `decode_utf8_lossy`). That matches `serde_urlencoded`
/// behaviour for query strings and is safer than ignoring the param.
fn decode_matched_params<'k, 'v, I>(pairs: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (&'k str, &'v str)>,
{
    pairs
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                percent_decode_str(v).decode_utf8_lossy().into_owned(),
            )
        })
        .collect()
}

/// Substitute parameter placeholders in a route pattern.
///
/// Unfilled placeholders are preserved verbatim (`/{a}/{b}` with only
/// `a` supplied becomes `/x/{b}`); the lenient form is the
/// long-standing default and lets callers spot the missing parameter
/// when staring at a URL. [`substitute_strict`] is the strict sibling
/// that returns the list of unfilled placeholder names instead, used
/// by redirects so a missing param surfaces as a 500 rather than as
/// a `Location` header with `{name}` baked into it.
fn substitute<F>(pattern: &str, mut next_value: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open + 1..];
        let Some(close) = rest.find('}') else {
            out.push('{');
            out.push_str(rest);
            return out;
        };
        let key = &rest[..close];
        if let Some(encoded) = next_value(key) {
            out.push_str(&encoded);
        } else {
            out.push('{');
            out.push_str(key);
            out.push('}');
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Strict sibling of [`substitute`]: substitute placeholders, and
/// return `Err` carrying the (ordered, deduplicated) list of
/// unfilled placeholder names if any required substitution was
/// missing. The substituted-so-far prefix is discarded — callers
/// only care that the URL is unsafe to emit.
fn substitute_strict<F>(pattern: &str, mut next_value: F) -> Result<String, Vec<String>>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut rest = pattern;
    let mut missing: Vec<String> = Vec::new();
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open + 1..];
        let Some(close) = rest.find('}') else {
            // Malformed pattern (unclosed `{`); treat the remainder as
            // a literal in the lenient form and report the leading
            // token as missing.
            out.push('{');
            out.push_str(rest);
            if !missing.iter().any(|m| m == rest) {
                missing.push(rest.to_string());
            }
            return Err(missing);
        };
        let key = &rest[..close];
        if let Some(encoded) = next_value(key) {
            out.push_str(&encoded);
        } else if !missing.iter().any(|m| m == key) {
            missing.push(key.to_string());
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

/// Generate a URL for a named route with parameters.
///
/// Path-parameter values are percent-encoded per RFC 3986 path-segment
/// rules so user-supplied content (slugs, IDs from query strings) cannot
/// inject path delimiters, query strings, or fragments into the resulting
/// URL. Unreserved characters (`A-Z a-z 0-9 - _ . ~`) pass through.
///
/// # Arguments
/// * `name` - The route name (e.g., "users.show")
/// * `params` - Slice of (key, value) tuples for path parameters
///
/// # Returns
/// * `Some(String)` - The generated URL with parameters substituted
/// * `None` - If the route name is not found
///
/// # Example
/// ```no_run
/// use suprnova::route;
///
/// let url = route("users.show", &[("id", "123")]);
/// assert_eq!(url, Some("/users/123".to_string()));
///
/// // Path-traversal attempts are encoded, not pasted in raw:
/// let url = route("users.show", &[("id", "../../etc/passwd")]);
/// assert_eq!(url, Some("/users/..%2F..%2Fetc%2Fpasswd".to_string()));
/// ```
pub fn route(name: &str, params: &[(&str, &str)]) -> Option<String> {
    let path_pattern = lookup_route(name)?;
    Some(substitute(&path_pattern, |key| {
        params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| utf8_percent_encode(v, PATH_SEGMENT_ENCODE).to_string())
    }))
}

/// Generate URL with HashMap parameters (used internally by Redirect).
///
/// Path-parameter values are percent-encoded; see [`route`] for the
/// encoding policy.
pub fn route_with_params(name: &str, params: &HashMap<String, String>) -> Option<String> {
    let path_pattern = lookup_route(name)?;
    Some(substitute(&path_pattern, |key| {
        params
            .get(key)
            .map(|v| utf8_percent_encode(v, PATH_SEGMENT_ENCODE).to_string())
    }))
}

/// Error returned by [`try_route`] / [`try_route_with_params`] when a
/// named route cannot be resolved to a complete URL.
///
/// The strict route helpers surface this so callers (notably
/// `Redirect::route`) refuse to emit a `Location` header containing a
/// raw `{placeholder}` segment when a required path parameter is
/// missing. The lenient [`route`] / [`route_with_params`] helpers
/// preserve the placeholder verbatim, which is fine for debug logging
/// but unsafe to ship to a browser as a redirect target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteUrlError {
    /// No route is registered under this name.
    NameNotFound(String),
    /// The route exists but one or more required path parameters
    /// were not supplied. The vec contains the placeholder names in
    /// pattern order (deduplicated).
    MissingParams {
        /// The route name that was looked up.
        name: String,
        /// Names of the placeholders that had no matching value.
        missing: Vec<String>,
    },
}

impl std::fmt::Display for RouteUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameNotFound(name) => {
                write!(f, "Route '{name}' not found")
            }
            Self::MissingParams { name, missing } => {
                write!(
                    f,
                    "Route '{name}' is missing required path parameter(s): {}",
                    missing.join(", "),
                )
            }
        }
    }
}

impl std::error::Error for RouteUrlError {}

/// Strict sibling of [`route`]: returns `Err(RouteUrlError)` when the
/// name is unknown OR when any `{placeholder}` in the pattern lacks a
/// matching value, instead of silently leaving the placeholder in the
/// generated URL.
///
/// Use this whenever the URL is going to land somewhere a user
/// follows (a `Location` header, a redirect, an email link). Use
/// [`route`] when a partial URL is acceptable (debug logging, dev
/// dashboards).
pub fn try_route(name: &str, params: &[(&str, &str)]) -> Result<String, RouteUrlError> {
    let path_pattern =
        lookup_route(name).ok_or_else(|| RouteUrlError::NameNotFound(name.into()))?;
    substitute_strict(&path_pattern, |key| {
        params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| utf8_percent_encode(v, PATH_SEGMENT_ENCODE).to_string())
    })
    .map_err(|missing| RouteUrlError::MissingParams {
        name: name.into(),
        missing,
    })
}

/// Strict sibling of [`route_with_params`]: returns
/// `Err(RouteUrlError)` when the name is unknown OR when any
/// `{placeholder}` in the pattern lacks a matching value. See
/// [`try_route`] for when to prefer the strict surface.
pub fn try_route_with_params(
    name: &str,
    params: &HashMap<String, String>,
) -> Result<String, RouteUrlError> {
    let path_pattern =
        lookup_route(name).ok_or_else(|| RouteUrlError::NameNotFound(name.into()))?;
    substitute_strict(&path_pattern, |key| {
        params
            .get(key)
            .map(|v| utf8_percent_encode(v, PATH_SEGMENT_ENCODE).to_string())
    })
    .map_err(|missing| RouteUrlError::MissingParams {
        name: name.into(),
        missing,
    })
}

/// Reverse-lookup a route name from a matched route pattern.
///
/// `pattern` is the matchit route template (e.g. `/users/{id}`) that
/// the router returned for the current request. Returns the registered
/// name (e.g. `"users.show"`) if one was assigned via `.name(...)` /
/// [`register_route_name`], or `None` for unnamed routes.
///
/// The name registry is keyed `name → pattern` so this performs an O(n)
/// scan over registered names. n is typically small (most apps register
/// well under a thousand routes); the scan cost is negligible compared
/// to the surrounding request lifecycle and avoids a second map.
///
/// Powers [`crate::http::Request::route_is`] and Inertia's
/// previous-route flash plumbing.
pub fn route_name_for_pattern(pattern: &str) -> Option<String> {
    let registry = ROUTE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()));
    let guard = match registry.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    for (name, registered) in guard.iter() {
        if registered == pattern {
            return Some(name.clone());
        }
    }
    None
}

/// Type alias for route handlers
pub type BoxedHandler =
    Box<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

/// HTTP Router with Laravel-like route registration
///
/// Each matchit per-method router stores `(pattern, handler)` so a
/// successful match returns both the resolved pattern (e.g.
/// `/api/posts/{id}`) and the handler. The pattern is what the
/// middleware registry is keyed by, so dispatch must look up
/// middleware under the matched pattern — not the raw request path
/// (e.g. `/api/posts/42`) — for group-applied middleware on
/// parameterised routes to run.
pub struct Router {
    get_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    post_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    put_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    patch_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    delete_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    head_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    options_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    /// WebSocket route registry. Separate from the HTTP route
    /// registries because the handler type is different
    /// (`BoxedWebSocketHandler` vs `Arc<BoxedHandler>`) and the match
    /// returns a different shape (`WsMatch` instead of `(pattern,
    /// handler, params)`).
    ///
    /// Each entry stores `(pattern, handler, per_route_middleware,
    /// optional_ws_config)` so the per-route middleware chain and
    /// optional `WsConfig` override can be recovered by `match_ws`
    /// and used in `handle_ws_upgrade`.
    ws_routes: MatchitRouter<(
        String,
        BoxedWebSocketHandler,
        Vec<BoxedMiddleware>,
        Option<crate::ws::WsConfig>,
    )>,
    /// Middleware assignments: (method, path) -> boxed middleware instances.
    ///
    /// Keying by `(Method, String)` rather than path alone prevents
    /// middleware registered for one method on a path (e.g.
    /// `POST /api/posts` under an auth group) from silently bleeding
    /// onto a different method registered separately for the same
    /// path (e.g. a public `GET /api/posts`). This is a
    /// security-shaped invariant — the codex review tracked it as
    /// "route_middleware keyed by path leaks across methods".
    route_middleware: HashMap<(Method, String), Vec<BoxedMiddleware>>,
    /// Fallback handler for when no routes match (overrides default 404)
    fallback_handler: Option<Arc<BoxedHandler>>,
    /// Middleware for the fallback route
    fallback_middleware: Vec<BoxedMiddleware>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            get_routes: MatchitRouter::new(),
            post_routes: MatchitRouter::new(),
            put_routes: MatchitRouter::new(),
            patch_routes: MatchitRouter::new(),
            delete_routes: MatchitRouter::new(),
            head_routes: MatchitRouter::new(),
            options_routes: MatchitRouter::new(),
            ws_routes: MatchitRouter::new(),
            route_middleware: HashMap::new(),
            fallback_handler: None,
            fallback_middleware: Vec::new(),
        }
    }

    /// Get middleware registered for a specific `(method, pattern)` pair.
    ///
    /// The key is the HTTP method plus the route **pattern** that
    /// `add_middleware` was called with — e.g. `/api/posts/{id}`,
    /// not the resolved request path `/api/posts/42`. The dispatcher
    /// in `server.rs` calls `match_route` first to recover the matched
    /// pattern and then passes that pattern here, so parameterised
    /// routes inherit group middleware correctly.
    pub fn get_route_middleware(&self, method: &Method, pattern: &str) -> Vec<BoxedMiddleware> {
        self.route_middleware
            .get(&(method.clone(), pattern.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    /// Register middleware for a `(method, path)` pair (internal use).
    pub(crate) fn add_middleware(
        &mut self,
        method: Method,
        path: &str,
        middleware: BoxedMiddleware,
    ) {
        self.route_middleware
            .entry((method, path.to_string()))
            .or_default()
            .push(middleware);
    }

    /// Set the fallback handler for when no routes match
    pub(crate) fn set_fallback(&mut self, handler: Arc<BoxedHandler>) {
        self.fallback_handler = Some(handler);
    }

    /// Add middleware to the fallback route
    pub(crate) fn add_fallback_middleware(&mut self, middleware: BoxedMiddleware) {
        self.fallback_middleware.push(middleware);
    }

    /// Get the fallback handler and its middleware
    pub fn get_fallback(&self) -> Option<(Arc<BoxedHandler>, Vec<BoxedMiddleware>)> {
        self.fallback_handler
            .as_ref()
            .map(|h| (h.clone(), self.fallback_middleware.clone()))
    }

    /// Insert a GET route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate route registration or any other matchit insert
    /// error (malformed pattern, too-many-segments, ...). Route registration
    /// is boot-time; silent swallowing of the second registration was a
    /// security-shaped bug because the surviving handler depended on
    /// registration order rather than user intent.
    pub(crate) fn insert_get(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_get(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_get`]: returns
    /// `Err(FrameworkError)` (naming the method + path) instead of panicking
    /// on a duplicate or malformed route pattern. Backs the public
    /// `try_*` registration surface and [`GroupBuilder::try_finalize`].
    pub(crate) fn try_insert_get(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.get_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register GET route '{path}': {e}"))
            })
    }

    /// Insert a POST route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any other matchit insert error.
    /// See [`Router::insert_get`] for rationale.
    pub(crate) fn insert_post(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_post(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_post`]. See
    /// [`Router::try_insert_get`].
    pub(crate) fn try_insert_post(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.post_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register POST route '{path}': {e}"))
            })
    }

    /// Insert a PUT route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any other matchit insert error.
    /// See [`Router::insert_get`] for rationale.
    pub(crate) fn insert_put(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_put(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_put`]. See
    /// [`Router::try_insert_get`].
    pub(crate) fn try_insert_put(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.put_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register PUT route '{path}': {e}"))
            })
    }

    /// Insert a DELETE route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any other matchit insert error.
    /// See [`Router::insert_get`] for rationale.
    pub(crate) fn insert_delete(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_delete(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_delete`]. See
    /// [`Router::try_insert_get`].
    pub(crate) fn try_insert_delete(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.delete_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register DELETE route '{path}': {e}"))
            })
    }

    /// Insert a PATCH route with a pre-boxed handler (internal use for groups
    /// and the `routes!` macro).
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// See [`Router::insert_get`].
    pub(crate) fn insert_patch(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_patch(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_patch`].
    pub(crate) fn try_insert_patch(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.patch_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register PATCH route '{path}': {e}"))
            })
    }

    /// Insert a HEAD route with a pre-boxed handler.
    ///
    /// An explicit HEAD registration wins over the GET fallback inside
    /// [`Router::match_route`] — register HEAD only when you need custom
    /// headers without running the GET body computation.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    pub(crate) fn insert_head(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_head(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_head`].
    pub(crate) fn try_insert_head(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.head_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register HEAD route '{path}': {e}"))
            })
    }

    /// Insert an OPTIONS route with a pre-boxed handler.
    ///
    /// CORS preflight (`OPTIONS` + `Access-Control-Request-Method`) is
    /// answered by `CorsMiddleware` at the global-middleware layer, before
    /// the router. An explicit OPTIONS handler serves non-preflight uses:
    /// advertising allowed verbs for a resource, public API discovery, etc.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    pub(crate) fn insert_options(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_options(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_options`].
    pub(crate) fn try_insert_options(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.options_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register OPTIONS route '{path}': {e}"))
            })
    }

    /// Register a GET route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically — `Router::new().get("/users/:id", h)`
    /// and the `get!("/users/:id", h)` macro produce identical
    /// `matchit` registrations.
    ///
    /// # Panics
    ///
    /// Panics on duplicate route registration (two handlers on the same
    /// pattern) or any matchit insert error. See [`Router::insert_get`].
    /// Use [`Router::try_get`] to get an `Err(FrameworkError)` instead of a
    /// panic when registering routes from a fallible source.
    pub fn get<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_get(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::get`]: returns `Err(FrameworkError)`
    /// (naming the method + path) on a duplicate or malformed pattern
    /// instead of panicking. The chain is consumed either way — on `Err`
    /// the partially-built router is dropped. Prefer this over [`Router::get`]
    /// when route patterns come from dynamic config, plugins, or any source
    /// you don't control at compile time.
    pub fn try_get<H, Fut>(mut self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.get_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register GET route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::GET,
        })
    }

    /// Register a POST route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_post`] for a fallible variant.
    pub fn post<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_post(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::post`]. See [`Router::try_get`].
    pub fn try_post<H, Fut>(
        mut self,
        path: &str,
        handler: H,
    ) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.post_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register POST route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::POST,
        })
    }

    /// Register a PUT route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_put`] for a fallible variant.
    pub fn put<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_put(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::put`]. See [`Router::try_get`].
    pub fn try_put<H, Fut>(mut self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.put_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register PUT route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::PUT,
        })
    }

    /// Register a DELETE route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_delete`] for a fallible variant.
    pub fn delete<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_delete(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::delete`]. See [`Router::try_get`].
    pub fn try_delete<H, Fut>(
        mut self,
        path: &str,
        handler: H,
    ) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.delete_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register DELETE route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::DELETE,
        })
    }

    /// Register a PATCH route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_patch`] for a fallible variant.
    pub fn patch<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_patch(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::patch`]. See [`Router::try_get`].
    pub fn try_patch<H, Fut>(
        mut self,
        path: &str,
        handler: H,
    ) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.patch_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register PATCH route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::PATCH,
        })
    }

    /// Register a HEAD route.
    ///
    /// Per RFC 9110 §9.3.2, a HEAD request is identical to GET except the
    /// server MUST NOT send a body. Suprnova's dispatcher honours this in
    /// two ways:
    ///
    /// 1. If no explicit HEAD route is registered for a path,
    ///    [`Router::match_route`] returns the matching GET handler so the
    ///    same logic runs for both verbs.
    /// 2. The response body is stripped at the server boundary whenever the
    ///    request method is HEAD, regardless of whether the handler emitted
    ///    one. This holds equally for explicit HEAD handlers and for the
    ///    auto-fallback to GET.
    ///
    /// Register an explicit HEAD route only when you need it to override
    /// the GET fallback (for example, returning custom headers without
    /// running the GET body computation).
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_head`] for a fallible variant.
    pub fn head<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_head(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::head`]. See [`Router::try_get`].
    pub fn try_head<H, Fut>(
        mut self,
        path: &str,
        handler: H,
    ) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.head_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register HEAD route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::HEAD,
        })
    }

    /// Register an OPTIONS route.
    ///
    /// CORS preflight (`OPTIONS` + `Access-Control-Request-Method`) is
    /// handled by `CorsMiddleware` installed as global middleware; explicit
    /// OPTIONS routes are for non-preflight uses — advertising allowed
    /// verbs (`Accept-Patch`), public API discovery, programmatic resource
    /// description.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_options`] for a fallible variant.
    pub fn options<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_options(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::options`]. See [`Router::try_get`].
    pub fn try_options<H, Fut>(
        mut self,
        path: &str,
        handler: H,
    ) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.options_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register OPTIONS route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::OPTIONS,
        })
    }

    /// Register the same handler against an explicit list of HTTP methods.
    ///
    /// Laravel parity for `Route::match([...], ...)`. Each method gets its
    /// own entry in the per-method matchit registry, all sharing the same
    /// boxed handler (cloned `Arc`), so dispatch is O(1) per request. The
    /// returned [`MultiMethodRouteBuilder`] lets you attach a single name
    /// and a single middleware list that fan out across every method —
    /// see the type docs for the fan-out semantics.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics if `methods` is empty, contains a verb other than
    /// GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS, or if any of the methods
    /// already has a route registered at this path. The error message
    /// names the offending verb so the conflict is debuggable.
    ///
    /// Partial-failure caveat: if the first `n` methods register cleanly
    /// and method `n+1` conflicts, the first `n` registrations remain in
    /// the Router. This matches the chained-registration behavior of
    /// `.get(...).get(...)` (the first wins, the second panics). Use
    /// [`Router::try_methods`] when registering from a fallible source.
    pub fn methods<H, Fut>(
        self,
        methods: &[Method],
        path: &str,
        handler: H,
    ) -> MultiMethodRouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_methods(methods, path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::methods`]. Returns `Err(FrameworkError)`
    /// (naming the conflicting verb) instead of panicking. The partial-
    /// failure caveat on [`Router::methods`] applies here too: methods
    /// before the failing one stay registered.
    pub fn try_methods<H, Fut>(
        mut self,
        methods: &[Method],
        path: &str,
        handler: H,
    ) -> Result<MultiMethodRouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        if methods.is_empty() {
            return Err(FrameworkError::internal(
                "Router::methods() requires at least one HTTP method",
            ));
        }
        let converted = crate::routing::macros::convert_route_params(path);
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        let handler_arc = Arc::new(boxed);
        let mut registered = Vec::with_capacity(methods.len());
        for method in methods {
            match *method {
                Method::GET => self.try_insert_get(&converted, handler_arc.clone())?,
                Method::POST => self.try_insert_post(&converted, handler_arc.clone())?,
                Method::PUT => self.try_insert_put(&converted, handler_arc.clone())?,
                Method::PATCH => self.try_insert_patch(&converted, handler_arc.clone())?,
                Method::DELETE => self.try_insert_delete(&converted, handler_arc.clone())?,
                Method::HEAD => self.try_insert_head(&converted, handler_arc.clone())?,
                Method::OPTIONS => self.try_insert_options(&converted, handler_arc.clone())?,
                ref other => {
                    return Err(FrameworkError::internal(format!(
                        "Router::methods() got unsupported method '{other}'; only \
                         GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS are accepted"
                    )));
                }
            }
            registered.push(method.clone());
        }
        Ok(MultiMethodRouteBuilder {
            router: self,
            methods: registered,
            path: converted,
        })
    }

    /// Register the same handler against every common HTTP method
    /// (GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS).
    ///
    /// Laravel parity for `Route::any(...)`. Equivalent to calling
    /// [`Router::methods`] with the seven-method list. Same dual-API
    /// + chained-name + fan-out-middleware story as [`Router::methods`].
    ///
    /// # Panics
    ///
    /// Panics if any of the seven methods already has a route registered
    /// at this path. See [`Router::methods`] for the partial-failure
    /// caveat. Use [`Router::try_any`] when the path may already be
    /// registered (dynamic config, plugins).
    pub fn any<H, Fut>(self, path: &str, handler: H) -> MultiMethodRouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_any(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::any`]. Returns `Err(FrameworkError)`
    /// instead of panicking; otherwise identical.
    pub fn try_any<H, Fut>(
        self,
        path: &str,
        handler: H,
    ) -> Result<MultiMethodRouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_methods(ANY_METHODS, path, handler)
    }

    /// Register a WebSocket route. The handler runs after the
    /// framework completes the HTTP/1.1 Upgrade handshake; it
    /// receives a [`WsSocket`] plus the original [`Request`] so it
    /// can read cookies, session, headers, and captured route params.
    ///
    /// Unlike `get` / `post` / etc., this returns `Router` directly
    /// (not `RouteBuilder`) — WS routes support per-route middleware
    /// via [`Router::ws_with_middleware`] or the `ws!(...).middleware(M)`
    /// chain on `WsRouteDef`.
    ///
    /// [`WsSocket`]: crate::ws::WsSocket
    pub fn ws<H>(self, path: &str, handler: H) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), None)
    }

    /// Fallible sibling of [`Router::ws`]: returns `Err(FrameworkError)` on a
    /// duplicate or malformed pattern instead of panicking. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws<H>(self, path: &str, handler: H) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), None)
    }

    /// Register a WebSocket route with a per-route [`WsConfig`] override.
    ///
    /// Use to set per-route ping_interval, max_message_size, max_frame_size,
    /// or max_missed_pings. Routes without an explicit config use the
    /// framework default.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::time::Duration;
    /// use suprnova::ws::WsConfig;
    ///
    /// Router::new().ws_with_config("/ws/chat", ChatHandler, WsConfig {
    ///     ping_interval: Duration::from_secs(5),
    ///     ..Default::default()
    /// })
    /// ```
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    pub fn ws_with_config<H>(self, path: &str, handler: H, config: crate::ws::WsConfig) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), Some(config))
    }

    /// Fallible sibling of [`Router::ws_with_config`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws_with_config<H>(
        self,
        path: &str,
        handler: H,
        config: crate::ws::WsConfig,
    ) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), Some(config))
    }

    /// Register a WebSocket route with a pre-populated middleware list.
    ///
    /// Middleware runs over the upgrade `Request` before the handler is
    /// dispatched. A non-2xx response from any middleware short-circuits
    /// the upgrade (the peer receives the rejection response and the
    /// WebSocket future drops cleanly). Middleware can substitute a
    /// modified `Request` via `next(modified_req)`.
    pub fn ws_with_middleware<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
    ) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware_and_config(path, boxed, middleware, None)
    }

    /// Fallible sibling of [`Router::ws_with_middleware`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws_with_middleware<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
    ) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, middleware, None)
    }

    /// Register a WebSocket route with both a middleware list and a per-route
    /// [`WsConfig`] override.
    pub fn ws_with_middleware_and_config<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
        config: crate::ws::WsConfig,
    ) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware_and_config(path, boxed, middleware, Some(config))
    }

    /// Fallible sibling of [`Router::ws_with_middleware_and_config`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws_with_middleware_and_config<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
        config: crate::ws::WsConfig,
    ) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, middleware, Some(config))
    }

    /// Register a pre-boxed WebSocket handler. Used internally by
    /// the `ws!` macro which type-erases the handler at the call
    /// site so the macro's `WsRouteDef` shape doesn't need a generic
    /// parameter.
    #[doc(hidden)]
    pub fn ws_boxed(self, path: &str, handler: BoxedWebSocketHandler) -> Router {
        self.ws_boxed_with_middleware_and_config(path, handler, Vec::new(), None)
    }

    /// Fallible sibling of [`Router::ws_boxed`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    #[doc(hidden)]
    pub fn try_ws_boxed(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
    ) -> Result<Router, FrameworkError> {
        self.try_ws_boxed_with_middleware_and_config(path, handler, Vec::new(), None)
    }

    /// Register a pre-boxed WebSocket handler with a per-route middleware list.
    ///
    /// Used internally by `WsRouteDef::register` when middleware has been
    /// chained via `.middleware(M)`.
    #[doc(hidden)]
    pub fn ws_boxed_with_middleware(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
    ) -> Router {
        self.ws_boxed_with_middleware_and_config(path, handler, middleware, None)
    }

    /// Fallible sibling of [`Router::ws_boxed_with_middleware`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    #[doc(hidden)]
    pub fn try_ws_boxed_with_middleware(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
    ) -> Result<Router, FrameworkError> {
        self.try_ws_boxed_with_middleware_and_config(path, handler, middleware, None)
    }

    /// Register a pre-boxed WebSocket handler with a per-route middleware list
    /// and an optional [`WsConfig`] override. This is the canonical registration
    /// method — all other `ws*` variants delegate to this one.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically — `Router::new().ws("/ws/rooms/:id", h)`
    /// and the `ws!("/ws/rooms/:id", h)` macro produce identical
    /// `matchit` registrations.
    ///
    /// # Panics
    ///
    /// Panics on duplicate route registration or any matchit insert error.
    /// WebSocket routes share the same boot-time-fail-loud policy as HTTP
    /// routes (see [`Router::insert_get`]).
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    #[doc(hidden)]
    pub fn ws_boxed_with_middleware_and_config(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
        config: Option<crate::ws::WsConfig>,
    ) -> Router {
        self.try_ws_boxed_with_middleware_and_config(path, handler, middleware, config)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::ws_boxed_with_middleware_and_config`]:
    /// returns `Err(FrameworkError)` (naming the path) on a duplicate or
    /// malformed pattern instead of panicking. This is the canonical
    /// fallible WebSocket registration primitive — every `try_ws*` helper
    /// delegates to it, mirroring the infallible family.
    #[doc(hidden)]
    pub fn try_ws_boxed_with_middleware_and_config(
        mut self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
        config: Option<crate::ws::WsConfig>,
    ) -> Result<Router, FrameworkError> {
        // Fail boot loud if the explicit per-route config carries a
        // runtime-fatal invariant violation (zero ping_interval,
        // zero max_missed_pings). The `None` arm picks up the framework
        // default, which is validated by a unit test in `ws::mod`.
        if let Some(cfg) = config.as_ref()
            && let Err(reason) = cfg.validate()
        {
            return Err(FrameworkError::internal(format!(
                "Failed to register WS route '{path}': {reason}"
            )));
        }
        let converted = crate::routing::macros::convert_route_params(path);
        self.ws_routes
            .insert(&converted, (converted.clone(), handler, middleware, config))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register WS route '{path}': {e}"))
            })?;
        Ok(self)
    }

    /// Look up a WebSocket route by path. Returns the matched
    /// handler + captured params + per-route middleware + optional
    /// per-route [`WsConfig`] if any route registered with
    /// [`Router::ws`], [`Router::ws_with_config`], or
    /// [`Router::ws_with_middleware`] matches.
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    pub fn match_ws(&self, path: &str) -> Option<WsMatch> {
        self.ws_routes.at(path).ok().map(|m| {
            let params: HashMap<String, String> = m
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let (pattern, handler, middleware, config) = m.value;
            WsMatch {
                handler: handler.clone(),
                pattern: pattern.clone(),
                params,
                middleware: middleware.clone(),
                config: config.clone(),
            }
        })
    }

    /// Match a request and return the matched route's **pattern**, its
    /// handler, and the extracted path parameters.
    ///
    /// The pattern (e.g. `/api/posts/{id}`) is what the middleware
    /// registry is keyed by — pass it to
    /// [`Router::get_route_middleware`] so group-applied middleware on
    /// parameterised routes runs.
    pub fn match_route(
        &self,
        method: &hyper::Method,
        path: &str,
    ) -> Option<(String, Arc<BoxedHandler>, HashMap<String, String>)> {
        let router = match *method {
            hyper::Method::GET => &self.get_routes,
            hyper::Method::POST => &self.post_routes,
            hyper::Method::PUT => &self.put_routes,
            hyper::Method::PATCH => &self.patch_routes,
            hyper::Method::DELETE => &self.delete_routes,
            hyper::Method::HEAD => {
                // RFC 9110 §9.3.2: HEAD is identical to GET except the
                // server MUST NOT send a body. Honour the spec by giving
                // an explicit HEAD registration priority (custom headers
                // without running the GET body computation), and otherwise
                // returning the matching GET handler. The body is stripped
                // for HEAD requests at the server boundary regardless of
                // which arm matched.
                if let Ok(matched) = self.head_routes.at(path) {
                    let params = decode_matched_params(matched.params.iter());
                    let (pattern, handler) = matched.value;
                    return Some((pattern.clone(), handler.clone(), params));
                }
                &self.get_routes
            }
            hyper::Method::OPTIONS => &self.options_routes,
            _ => return None,
        };

        router.at(path).ok().map(|matched| {
            let params = decode_matched_params(matched.params.iter());
            let (pattern, handler) = matched.value;
            (pattern.clone(), handler.clone(), params)
        })
    }

    /// Whether `path` has a HEAD handler registered explicitly (as
    /// opposed to falling back to GET in [`Router::match_route`]).
    ///
    /// Used by the server to pick the correct `(method, pattern)`
    /// route-middleware key when a HEAD request falls back to GET: if a
    /// GET-only registration exists, the GET middleware list must run
    /// instead of an empty HEAD list.
    pub fn has_explicit_head(&self, path: &str) -> bool {
        self.head_routes.at(path).is_ok()
    }

    /// Register a redirecting route. A `GET` to `from` responds with
    /// `status` (default 302) and `Location: to`.
    ///
    /// Mirrors Laravel's `Route::redirect($from, $to, $status = 302)` from
    /// `Illuminate/Routing/Router.php:258`. Useful for permanent URL
    /// migrations, deprecated paths, and surfacing redirects at the
    /// route layer instead of inside a controller body.
    ///
    /// The destination is a literal path (or absolute URL); no parameter
    /// substitution. For redirects that need to resolve a named route,
    /// register a normal handler and return [`crate::Redirect::route`].
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration. See [`Router::get`] for the
    /// boot-time-fail-loud rationale. `status` must be a valid 3xx code
    /// (300..400) — values outside that range are clamped to 302.
    pub fn redirect(self, from: &str, to: &str, status: u16) -> Router {
        let status = if (300..400).contains(&status) {
            status
        } else {
            302
        };
        let to_owned = to.to_string();
        self.get(from, move |_req: Request| {
            let target = to_owned.clone();
            async move {
                Ok(crate::http::HttpResponse::new()
                    .status(status)
                    .header("Location", target))
            }
        })
        .into()
    }

    /// Register a permanent-redirecting route (status 301). Convenience
    /// wrapper over [`Router::redirect`] with a `301` status. Mirrors
    /// Laravel's `Route::permanentRedirect($from, $to)` from
    /// `Illuminate/Routing/Router.php:272`.
    pub fn permanent_redirect(self, from: &str, to: &str) -> Router {
        self.redirect(from, to, 301)
    }

    /// Register a static-page route that renders an Inertia component
    /// with constant props.
    ///
    /// Suprnova's analogue of Laravel's `Route::view($uri, $view,
    /// $data)` (`Illuminate/Routing/Router.php:287`). Laravel's `view`
    /// route renders a Blade template; Suprnova renders an Inertia
    /// page component (SvelteKit/React/Vue), because the framework's
    /// templating system is Inertia, not Blade.
    ///
    /// Useful for static pages (about/terms/privacy) where the handler
    /// would otherwise be a one-line `Inertia::render("About", json!({...}))`
    /// — this saves the function definition.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration.
    pub fn view(self, path: &str, component: &'static str, props: serde_json::Value) -> Router {
        let component = component.to_string();
        self.get(path, move |req: Request| {
            let component = component.clone();
            let props = props.clone();
            async move {
                let mut response = crate::inertia::InertiaResponse::new(component);
                if let serde_json::Value::Object(map) = props {
                    for (k, v) in map {
                        response = response.with(&k, v);
                    }
                }
                response
                    .resolve(&req)
                    .await
                    .map_err(crate::http::HttpResponse::from)
            }
        })
        .into()
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder returned after registering a route, enabling .name() chaining
pub struct RouteBuilder {
    pub(crate) router: Router,
    last_path: String,
    last_method: Method,
}

impl RouteBuilder {
    /// Name the most recently registered route.
    ///
    /// # Panics
    ///
    /// Panics if `name` is already registered to a different path (see
    /// [`register_route_name`]). Use [`RouteBuilder::try_name`] for a
    /// fallible variant.
    pub fn name(self, name: &str) -> Router {
        self.try_name(name).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`RouteBuilder::name`]: returns
    /// `Err(FrameworkError)` (naming the conflicting name) instead of
    /// panicking when `name` is already bound to a different path. The
    /// builder is consumed either way.
    pub fn try_name(self, name: &str) -> Result<Router, FrameworkError> {
        try_register_route_name(name, &self.last_path)?;
        Ok(self.router)
    }

    /// Apply middleware to the most recently registered route
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Router::new()
    ///     .get("/admin", admin_handler).middleware(AuthMiddleware)
    ///     .get("/api/users", users_handler).middleware(CorsMiddleware)
    /// ```
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> RouteBuilder {
        let method = self.last_method.clone();
        let path = self.last_path.clone();
        self.router
            .add_middleware(method, &path, into_boxed(middleware));
        self
    }

    /// Apply pre-boxed middleware to the most recently registered route
    /// (Used internally by route macros)
    pub fn middleware_boxed(mut self, middleware: BoxedMiddleware) -> RouteBuilder {
        let method = self.last_method.clone();
        let path = self.last_path.clone();
        self.router.add_middleware(method, &path, middleware);
        self
    }

    /// Register a GET route (for chaining without .name())
    pub fn get<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.get(path, handler)
    }

    /// Register a POST route (for chaining without .name())
    pub fn post<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.post(path, handler)
    }

    /// Register a PUT route (for chaining without .name())
    pub fn put<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.put(path, handler)
    }

    /// Register a DELETE route (for chaining without .name())
    pub fn delete<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.delete(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::get`]. See [`Router::try_get`].
    pub fn try_get<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_get(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::post`]. See [`Router::try_post`].
    pub fn try_post<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_post(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::put`]. See [`Router::try_put`].
    pub fn try_put<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_put(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::delete`]. See
    /// [`Router::try_delete`].
    pub fn try_delete<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_delete(path, handler)
    }

    /// Register a PATCH route (for chaining without `.name()`).
    pub fn patch<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.patch(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::patch`]. See [`Router::try_patch`].
    pub fn try_patch<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_patch(path, handler)
    }

    /// Register a HEAD route (for chaining without `.name()`).
    pub fn head<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.head(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::head`]. See [`Router::try_head`].
    pub fn try_head<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_head(path, handler)
    }

    /// Register an OPTIONS route (for chaining without `.name()`).
    pub fn options<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.options(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::options`]. See
    /// [`Router::try_options`].
    pub fn try_options<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_options(path, handler)
    }

    /// Register a route across every common HTTP method (GET / POST /
    /// PUT / PATCH / DELETE / HEAD / OPTIONS) — Laravel `Route::any`.
    /// See [`Router::any`].
    pub fn any<H, Fut>(self, path: &str, handler: H) -> MultiMethodRouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.any(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::any`]. See [`Router::try_any`].
    pub fn try_any<H, Fut>(
        self,
        path: &str,
        handler: H,
    ) -> Result<MultiMethodRouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_any(path, handler)
    }

    /// Register a route across an explicit list of HTTP methods —
    /// Laravel `Route::match([...], ...)`. See [`Router::methods`].
    pub fn methods<H, Fut>(
        self,
        methods: &[Method],
        path: &str,
        handler: H,
    ) -> MultiMethodRouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.methods(methods, path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::methods`]. See
    /// [`Router::try_methods`].
    pub fn try_methods<H, Fut>(
        self,
        methods: &[Method],
        path: &str,
        handler: H,
    ) -> Result<MultiMethodRouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_methods(methods, path, handler)
    }
}

impl From<RouteBuilder> for Router {
    fn from(builder: RouteBuilder) -> Self {
        builder.router
    }
}

/// The seven HTTP methods that [`Router::any`] fans out across. Kept
/// in registration order so `methods` field of the returned builder
/// matches the order callers see in tests / logs.
const ANY_METHODS: &[Method] = &[
    Method::GET,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
    Method::HEAD,
    Method::OPTIONS,
];

/// Builder returned by [`Router::methods`] and [`Router::any`]. Holds the
/// inner `Router` plus the list of methods the route was registered against,
/// and the canonical path (after `:param` → `{param}` conversion). Chained
/// `.name(...)` and `.middleware(...)` calls fan out across every stored
/// method, then `Into<Router>` (or `.name(...)` returning `Router`) hands
/// back ownership.
///
/// Fan-out semantics:
///
/// - `.name(name)` registers `name` once (the path is shared across
///   methods); reverse lookup via [`route`] returns the same URL no
///   matter which verb the user came from.
/// - `.middleware(M)` adds the middleware under every
///   `(method, path)` key — auth / CSRF / rate-limit registered on an
///   `any` route therefore guard all seven verbs, not just one.
///
/// `MultiMethodRouteBuilder` cannot be re-entered as a `RouteBuilder`
/// because the "most recently registered route" is ambiguous when N
/// methods share one registration. Chain further routes off the
/// `Router` (via `.into()`) instead.
pub struct MultiMethodRouteBuilder {
    pub(crate) router: Router,
    methods: Vec<Method>,
    path: String,
}

impl MultiMethodRouteBuilder {
    /// Methods this builder registered against (in registration order).
    /// Used by tests and the macro-layer `AnyRouteDefBuilder` to verify
    /// the fan-out.
    pub fn methods(&self) -> &[Method] {
        &self.methods
    }

    /// The canonical path (after `:param` normalisation) the route was
    /// registered under.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Name this multi-method route. The name is registered ONCE; reverse
    /// lookup via [`route`] returns the same URL regardless of which verb
    /// the user is querying for.
    ///
    /// # Panics
    ///
    /// Panics if `name` is already registered to a different path. Use
    /// [`MultiMethodRouteBuilder::try_name`] for a fallible variant.
    pub fn name(self, name: &str) -> Router {
        self.try_name(name).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`MultiMethodRouteBuilder::name`].
    pub fn try_name(self, name: &str) -> Result<Router, FrameworkError> {
        try_register_route_name(name, &self.path)?;
        Ok(self.router)
    }

    /// Attach middleware that runs for every method this route was
    /// registered against. Fans out internally to N `(method, path)`
    /// entries in the route-middleware map so each per-method route
    /// inherits the same instance.
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        let boxed = into_boxed(middleware);
        for method in &self.methods {
            self.router
                .add_middleware(method.clone(), &self.path, boxed.clone());
        }
        self
    }

    /// Pre-boxed sibling of [`MultiMethodRouteBuilder::middleware`]
    /// (used internally by the macro-layer `AnyRouteDefBuilder` so it
    /// doesn't need to know about the `M: Middleware` generic).
    pub fn middleware_boxed(mut self, middleware: BoxedMiddleware) -> Self {
        for method in &self.methods {
            self.router
                .add_middleware(method.clone(), &self.path, middleware.clone());
        }
        self
    }
}

impl From<MultiMethodRouteBuilder> for Router {
    fn from(builder: MultiMethodRouteBuilder) -> Self {
        builder.router
    }
}

/// Result of a [`Router::match_ws`] lookup. Bundles the matched
/// handler with the matched pattern (e.g. `/ws/rooms/{id}`),
/// the captured path parameters (e.g. `{"id": "42"}`), the
/// per-route middleware list (empty if registered via plain
/// [`Router::ws`]), and an optional per-route [`WsConfig`] override.
///
/// [`WsConfig`]: crate::ws::WsConfig
pub struct WsMatch {
    handler: BoxedWebSocketHandler,
    pattern: String,
    params: HashMap<String, String>,
    middleware: Vec<BoxedMiddleware>,
    config: Option<crate::ws::WsConfig>,
}

impl WsMatch {
    /// The matched handler. Clone the `Arc` to move it into the
    /// spawned handler task.
    pub fn handler(&self) -> BoxedWebSocketHandler {
        self.handler.clone()
    }

    /// The registered route pattern that matched (e.g.
    /// `/ws/rooms/{id}`). Useful for telemetry / observability.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Captured path params as a borrowed `HashMap<String, String>`
    /// for handler consumption. The map is materialized once at
    /// match time so the lifetime is bound to the `WsMatch` itself,
    /// not the underlying matchit::Params reference. Callers that
    /// need owned data can `.clone()` at the call site.
    pub fn params(&self) -> &HashMap<String, String> {
        &self.params
    }

    /// Per-route middleware to run over the upgrade `Request` before
    /// dispatching to the handler. Empty slice when the route was
    /// registered without any `.middleware(M)` chaining.
    pub fn middleware(&self) -> &Vec<BoxedMiddleware> {
        &self.middleware
    }

    /// Per-route [`WsConfig`] override, if the route was registered
    /// with `.config(WsConfig)` or [`Router::ws_with_config`].
    /// Returns `None` when no override was set — the caller should
    /// fall back to [`WsConfig::default()`].
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    /// [`WsConfig::default()`]: crate::ws::WsConfig::default
    pub fn config(&self) -> Option<&crate::ws::WsConfig> {
        self.config.as_ref()
    }
}

#[cfg(test)]
mod tests {
    //! Domain 1 audit regressions (2026-05).
    //!
    //! Findings F1, F5, F6, F7, F8 from
    //! `docs/superpowers/audit-2026-05/DOMAIN-01-router-and-dispatch.md`.
    //!
    //! **Route-name registry isolation.** Every test that registers a route
    //! name (`.name(...)` / `register_route_name`) or resolves one (`route` /
    //! `route_with_params`) shares the process-global `ROUTE_REGISTRY`, so they
    //! all carry `#[serial_test::serial(route_registry)]`. The hazard is
    //! `clear_route_names_for_test`'s global drain: without the shared key it
    //! can interleave between a registration and the lookup (or conflict panic)
    //! that depends on it, wiping a binding mid-test. A new name-touching test
    //! MUST join this serial group. Tests that only drive `Router::match_route`
    //! touch a local `Router`, not the global table, and need no key.

    use super::*;
    use crate::http::text;

    async fn h(_req: Request) -> Response {
        text("ok")
    }

    /// F1: duplicate route registration panics instead of silently dropping
    /// the second handler.
    #[test]
    #[should_panic(expected = "Failed to register GET route '/users'")]
    fn duplicate_get_registration_panics() {
        let _ = Router::new().get("/users", h).get("/users", h);
    }

    /// F1: insert_post / put / delete share the same policy.
    #[test]
    #[should_panic(expected = "Failed to register DELETE route '/x'")]
    fn duplicate_delete_registration_panics() {
        let _ = Router::new().delete("/x", h).delete("/x", h);
    }

    /// F6: fluent `Router::get(":id", h)` registers under the matchit-shape
    /// pattern `/users/{id}` and captures `id` from the request path.
    /// Previously the fluent path passed `:id` through verbatim and matchit
    /// treated it as a literal segment, so `/users/42` failed to match.
    #[test]
    fn fluent_path_supports_colon_param_syntax() {
        let router: Router = Router::new().get("/users/:id", h).into();
        let m = router.match_route(&Method::GET, "/users/42");
        let (pattern, _handler, params) = m.expect(
            "fluent :id syntax must match /users/42 \
             — convert_route_params should run in the fluent path too",
        );
        assert_eq!(pattern, "/users/{id}");
        assert_eq!(params.get("id"), Some(&"42".to_string()));
    }

    /// F6 + F5 + F7 + name resolution: a fluent `:id` route registered with
    /// `.name(...)` must resolve via `route(name, &[...])` to the same URL
    /// the macro path would produce.
    #[test]
    #[serial_test::serial(route_registry)]
    fn fluent_named_colon_route_resolves_via_route_helper() {
        let _ = Router::new()
            .get("/posts/:slug", h)
            .name("posts.show.fluent");
        let url = route("posts.show.fluent", &[("slug", "hello-world")]);
        assert_eq!(url, Some("/posts/hello-world".to_string()));
    }

    /// F5: path-segment values containing reserved characters are
    /// percent-encoded. Without this fix, a user-controlled slug
    /// containing `/`, `?`, `#`, or `&` would corrupt the generated URL
    /// (open-redirect / path-injection class).
    #[test]
    #[serial_test::serial(route_registry)]
    fn route_percent_encodes_reserved_path_characters() {
        let _ = Router::new()
            .get("/users/{id}", h)
            .name("users.show.encoding");
        let url = route("users.show.encoding", &[("id", "../../etc/passwd")]);
        assert_eq!(
            url.as_deref(),
            Some("/users/..%2F..%2Fetc%2Fpasswd"),
            "slash characters must be percent-encoded out of path segments",
        );
    }

    /// F5: question-mark, hash, ampersand are not allowed to inject query
    /// string or fragment into the URL.
    #[test]
    #[serial_test::serial(route_registry)]
    fn route_percent_encodes_query_and_fragment_delimiters() {
        let _ = Router::new()
            .get("/posts/{slug}", h)
            .name("posts.show.frag");
        let url = route("posts.show.frag", &[("slug", "x?evil=1#hash&y=2")]);
        assert_eq!(
            url.as_deref(),
            Some("/posts/x%3Fevil%3D1%23hash%26y%3D2"),
            "?, #, =, & must be percent-encoded so they cannot inject \
             query string or fragment into the generated URL",
        );
    }

    /// F5: unreserved characters pass through unchanged so URLs stay
    /// readable when the slug is well-formed.
    #[test]
    #[serial_test::serial(route_registry)]
    fn route_preserves_unreserved_characters() {
        let _ = Router::new()
            .get("/posts/{slug}", h)
            .name("posts.show.safe");
        let url = route("posts.show.safe", &[("slug", "hello-world_42.html~tilde")]);
        assert_eq!(url.as_deref(), Some("/posts/hello-world_42.html~tilde"),);
    }

    /// F7: registering the same name to a different path panics. Two
    /// routes claiming the same name silently shadowing each other is a
    /// security-shaped bug.
    #[test]
    #[should_panic(expected = "Route name 'users.duplicate'")]
    #[serial_test::serial(route_registry)]
    fn duplicate_route_name_panics() {
        let _ = Router::new()
            .get("/a", h)
            .name("users.duplicate")
            .get("/b", h)
            .name("users.duplicate");
    }

    /// F7: registering the same `(name, path)` pair twice is idempotent.
    /// This matters for inventory-driven registration where the same
    /// route may be registered on every test that calls
    /// `routes::register()`.
    #[test]
    #[serial_test::serial(route_registry)]
    fn registering_same_name_same_path_is_idempotent() {
        register_route_name("idempotent.example", "/foo/{id}");
        register_route_name("idempotent.example", "/foo/{id}");
        let url = route("idempotent.example", &[("id", "1")]);
        assert_eq!(url, Some("/foo/1".to_string()));
    }

    /// F5: `route_with_params` (HashMap path) shares the same encoding.
    #[test]
    #[serial_test::serial(route_registry)]
    fn route_with_params_percent_encodes_values() {
        let _ = Router::new()
            .get("/redirect/{target}", h)
            .name("redirect.target");
        let params: HashMap<String, String> =
            [("target".to_string(), "https://evil.example/".to_string())]
                .into_iter()
                .collect();
        let url = route_with_params("redirect.target", &params);
        assert_eq!(
            url.as_deref(),
            Some("/redirect/https%3A%2F%2Fevil.example%2F"),
        );
    }

    /// F5: missing parameter values leave the `{name}` placeholder intact.
    /// Previously the substitution silently produced a partial URL with
    /// `{name}` still embedded; verify the behaviour stays the same
    /// (callers can detect the missing param visually) and that the
    /// re-encoded form contains the original placeholder unchanged.
    #[test]
    #[serial_test::serial(route_registry)]
    fn route_leaves_unfilled_placeholders_in_place() {
        let _ = Router::new().get("/{a}/{b}", h).name("two.params.test");
        let url = route("two.params.test", &[("a", "x")]);
        assert_eq!(url, Some("/x/{b}".to_string()));
    }

    /// Strict variant returns `Err(MissingParams)` listing the unfilled
    /// placeholders so callers (notably `Redirect::route`) can refuse to
    /// emit a `Location` header containing `{name}` instead of a real
    /// value.
    #[test]
    #[serial_test::serial(route_registry)]
    fn try_route_reports_missing_params() {
        let _ = Router::new().get("/{a}/{b}", h).name("try.two.params.test");
        let err = try_route("try.two.params.test", &[("a", "x")]).expect_err("must report missing");
        match err {
            RouteUrlError::MissingParams { name, missing } => {
                assert_eq!(name, "try.two.params.test");
                assert_eq!(missing, vec!["b".to_string()]);
            }
            other => panic!("expected MissingParams, got {other:?}"),
        }
    }

    /// Strict variant returns `Err(NameNotFound)` when the route is
    /// unregistered, instead of `None`.
    #[test]
    #[serial_test::serial(route_registry)]
    fn try_route_reports_unknown_name() {
        let err =
            try_route("try.absolutely.not.registered", &[]).expect_err("must report unknown name");
        assert!(
            matches!(err, RouteUrlError::NameNotFound(ref n) if n == "try.absolutely.not.registered")
        );
    }

    /// Happy path: strict variant returns `Ok(url)` matching the
    /// lenient form.
    #[test]
    #[serial_test::serial(route_registry)]
    fn try_route_happy_path_matches_lenient() {
        let _ = Router::new().get("/users/{id}", h).name("try.users.show");
        let url = try_route("try.users.show", &[("id", "42")]).expect("must resolve");
        assert_eq!(url, "/users/42");
    }

    /// HashMap-keyed strict variant reports missing parameters too.
    #[test]
    #[serial_test::serial(route_registry)]
    fn try_route_with_params_reports_missing() {
        let _ = Router::new().get("/posts/{slug}", h).name("try.posts.show");
        let params: HashMap<String, String> = HashMap::new();
        let err =
            try_route_with_params("try.posts.show", &params).expect_err("must report missing");
        match err {
            RouteUrlError::MissingParams { name, missing } => {
                assert_eq!(name, "try.posts.show");
                assert_eq!(missing, vec!["slug".to_string()]);
            }
            other => panic!("expected MissingParams, got {other:?}"),
        }
    }

    /// Percent-encoded captured params are decoded before reaching the
    /// handler. A slug `a/b` is encoded by `route()` to `a%2Fb`; the
    /// handler must see `a/b` when it reads `req.param("slug")` so the
    /// round-trip is observable to application code (Laravel parity:
    /// `Request::route('slug')` returns the decoded value).
    ///
    /// Matching itself still operates on the raw URI path — only the
    /// extracted *value* is decoded — so encoded segment delimiters
    /// (`%2F`) cannot inject an extra path separator into matchit's
    /// tree.
    #[test]
    fn match_route_decodes_percent_encoded_param_values() {
        let router: Router = Router::new().get("/posts/:slug", h).into();
        let m = router.match_route(&Method::GET, "/posts/a%2Fb");
        let (_pattern, _h, params) = m.expect("must match");
        assert_eq!(params.get("slug"), Some(&"a/b".to_string()));
    }

    /// Multi-byte UTF-8 values round-trip through encode/decode.
    #[test]
    fn match_route_decodes_utf8_param_values() {
        let router: Router = Router::new().get("/u/:name", h).into();
        // "café" → "caf%C3%A9"
        let m = router.match_route(&Method::GET, "/u/caf%C3%A9");
        let (_pattern, _h, params) = m.expect("must match");
        assert_eq!(params.get("name"), Some(&"café".to_string()));
    }

    /// HEAD-falls-back-to-GET path also decodes params.
    #[test]
    fn match_route_head_fallback_decodes_param_values() {
        let router: Router = Router::new().get("/h/:slug", h).into();
        let m = router.match_route(&Method::HEAD, "/h/a%2Fb");
        let (_pattern, _h, params) = m.expect("HEAD must fall back to GET");
        assert_eq!(params.get("slug"), Some(&"a/b".to_string()));
    }

    // ---- PATCH / HEAD / OPTIONS verb coverage -------------------------

    /// PATCH routes register through the fluent surface and match.
    #[test]
    fn patch_route_registers_and_matches() {
        let router: Router = Router::new().patch("/posts/:id", h).into();
        let m = router.match_route(&Method::PATCH, "/posts/42");
        let (pattern, _handler, params) = m.expect("PATCH must match");
        assert_eq!(pattern, "/posts/{id}");
        assert_eq!(params.get("id"), Some(&"42".to_string()));
    }

    /// HEAD with an explicit handler matches it directly; the GET
    /// fallback is not consulted.
    #[test]
    fn head_route_matches_explicit_registration() {
        let router: Router = Router::new()
            .head("/explicit", h)
            .get("/explicit", h)
            .into();
        assert!(router.has_explicit_head("/explicit"));
        let m = router.match_route(&Method::HEAD, "/explicit");
        let (pattern, _h, _p) = m.expect("HEAD must match its explicit handler");
        assert_eq!(pattern, "/explicit");
    }

    /// HEAD with no explicit handler falls back to the GET registry.
    /// RFC 9110 §9.3.2: HEAD is identical to GET aside from the body.
    #[test]
    fn head_falls_back_to_get_when_no_explicit_head_route() {
        let router: Router = Router::new().get("/articles/:slug", h).into();
        assert!(!router.has_explicit_head("/articles/:slug"));
        let m = router.match_route(&Method::HEAD, "/articles/intro");
        let (pattern, _h, params) =
            m.expect("HEAD must fall back to GET when no explicit HEAD is registered");
        assert_eq!(pattern, "/articles/{slug}");
        assert_eq!(params.get("slug"), Some(&"intro".to_string()));
    }

    /// HEAD with neither HEAD nor GET registered still returns None
    /// (so the server can drop to the 404 / fallback chain).
    #[test]
    fn head_returns_none_when_neither_head_nor_get_match() {
        let router: Router = Router::new().post("/submit", h).into();
        let m = router.match_route(&Method::HEAD, "/submit");
        assert!(
            m.is_none(),
            "HEAD must not match a POST-only path even via fallback",
        );
    }

    /// OPTIONS routes register and match. CORS preflight remains a
    /// middleware-layer concern; explicit OPTIONS handlers serve
    /// non-preflight discovery.
    #[test]
    fn options_route_registers_and_matches() {
        let router: Router = Router::new().options("/api/posts", h).into();
        let m = router.match_route(&Method::OPTIONS, "/api/posts");
        let (pattern, _h, _p) = m.expect("OPTIONS must match");
        assert_eq!(pattern, "/api/posts");
    }

    /// `try_patch` returns Err on duplicate registration; the Result
    /// reports the method + path so plugins / generators can surface
    /// the conflict.
    ///
    /// `RouteBuilder` does not implement `Debug` (handlers are boxed
    /// `dyn Fn`), so `Result<RouteBuilder, _>` can't use `.expect_err`.
    /// Destructure with `match` instead.
    #[test]
    fn try_patch_returns_err_on_duplicate() {
        let router = Router::new();
        let router = match router.try_patch("/u", h) {
            Ok(b) => b.router,
            Err(e) => panic!("first PATCH must register: {e}"),
        };
        let err = match router.try_patch("/u", h) {
            Err(e) => e,
            Ok(_) => panic!("duplicate must fail"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("PATCH route '/u'"),
            "error must name the verb + path; got {msg}",
        );
    }

    /// `try_head` and `try_options` share the dual-API surface: each
    /// has a fallible sibling that reports duplicates rather than
    /// panicking.
    #[test]
    fn try_head_and_try_options_return_err_on_duplicate() {
        let router = Router::new();
        let router = match router.try_head("/h", h) {
            Ok(b) => b.router,
            Err(e) => panic!("first HEAD must register: {e}"),
        };
        let err = match router.try_head("/h", h) {
            Err(e) => e,
            Ok(_) => panic!("dup HEAD must fail"),
        };
        assert!(err.to_string().contains("HEAD route '/h'"));

        let router = Router::new();
        let router = match router.try_options("/o", h) {
            Ok(b) => b.router,
            Err(e) => panic!("first OPTIONS must register: {e}"),
        };
        let err = match router.try_options("/o", h) {
            Err(e) => e,
            Ok(_) => panic!("dup OPTIONS must fail"),
        };
        assert!(err.to_string().contains("OPTIONS route '/o'"));
    }

    /// RouteBuilder chains through the new verbs (mirroring
    /// `get`/`post`/`put`/`delete`) so a mixed-verb router builds
    /// cleanly in one expression.
    #[test]
    fn route_builder_chains_patch_head_options() {
        let router: Router = Router::new()
            .get("/r", h)
            .patch("/r/edit", h)
            .head("/r/probe", h)
            .options("/r/meta", h)
            .into();
        assert!(router.match_route(&Method::GET, "/r").is_some());
        assert!(router.match_route(&Method::PATCH, "/r/edit").is_some());
        assert!(router.match_route(&Method::HEAD, "/r/probe").is_some());
        assert!(router.match_route(&Method::OPTIONS, "/r/meta").is_some());
    }

    // ---- any / methods fan-out coverage -------------------------------

    /// `Router::any` registers the handler against every common HTTP
    /// method (GET / POST / PUT / PATCH / DELETE / HEAD / OPTIONS).
    /// Pins the seven-method fan-out — if a verb is missed, `match_route`
    /// against it returns `None` and the request 404s.
    #[test]
    fn any_route_registers_all_seven_methods() {
        let router: Router = Router::new().any("/x", h).into();
        for m in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
        ] {
            assert!(
                router.match_route(&m, "/x").is_some(),
                "any() must register {m}, but match_route returned None",
            );
        }
    }

    /// `Router::methods(&[…])` registers exactly the verbs in the list
    /// and no others. Pins the partial-fan-out behavior.
    #[test]
    fn methods_registers_only_requested_verbs() {
        let router: Router = Router::new()
            .methods(&[Method::GET, Method::POST], "/y", h)
            .into();
        assert!(router.match_route(&Method::GET, "/y").is_some());
        assert!(router.match_route(&Method::POST, "/y").is_some());
        assert!(
            router.match_route(&Method::PUT, "/y").is_none(),
            "methods(&[GET, POST]) must NOT register PUT",
        );
        assert!(
            router.match_route(&Method::DELETE, "/y").is_none(),
            "methods(&[GET, POST]) must NOT register DELETE",
        );
    }

    /// `Router::methods(&[])` returns Err (rather than registering
    /// nothing silently).
    #[test]
    fn methods_with_empty_list_returns_err() {
        let router = Router::new();
        match router.try_methods(&[], "/z", h) {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("at least one HTTP method"),
                    "error must explain the empty-list rejection; got {msg}",
                );
            }
            Ok(_) => panic!("methods(&[], …) must fail"),
        }
    }

    /// `Router::methods` with an unsupported method (e.g. CONNECT, TRACE)
    /// returns Err naming the offender. Prevents silent acceptance of
    /// a method we don't have a matchit registry for.
    #[test]
    fn methods_with_unsupported_verb_returns_err() {
        let router = Router::new();
        let bad = Method::from_bytes(b"CONNECT").expect("valid CONNECT");
        match router.try_methods(&[Method::GET, bad], "/y", h) {
            Err(e) => assert!(
                e.to_string().contains("CONNECT"),
                "error must name the unsupported verb",
            ),
            Ok(_) => panic!("methods with unsupported verb must fail"),
        }
    }

    /// `.name(...)` on a MultiMethodRouteBuilder registers the name
    /// once, and the path resolves correctly. The advisor flagged
    /// this explicitly: "one name binding per `any` registration
    /// (not seven)".
    #[test]
    #[serial_test::serial(route_registry)]
    fn any_route_name_registers_once_and_resolves() {
        let _ = Router::new()
            .any("/webhooks/inbound", h)
            .name("webhooks.any.test");
        let url = route("webhooks.any.test", &[]);
        assert_eq!(url, Some("/webhooks/inbound".to_string()));
    }

    /// `.middleware(M)` on a MultiMethodRouteBuilder fans the same
    /// middleware out across every `(method, path)` key. The advisor
    /// flagged this explicitly: "middleware count grows; tests should
    /// pin it." We count the resulting middleware-map entries by
    /// querying per-method.
    #[test]
    fn any_route_middleware_fans_out_across_all_methods() {
        use crate::middleware::{Middleware, Next};
        use async_trait::async_trait;

        #[derive(Clone)]
        struct NoopMw;
        #[async_trait]
        impl Middleware for NoopMw {
            async fn handle(&self, request: Request, next: Next) -> Response {
                next(request).await
            }
        }

        let router: Router = Router::new().any("/m", h).middleware(NoopMw).into();
        for m in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
        ] {
            assert_eq!(
                router.get_route_middleware(&m, "/m").len(),
                1,
                "any().middleware(M) must register on ({m}, /m) — \
                 fan-out missed this verb",
            );
        }
    }

    /// `clear_route_names_for_test` drains the process-global
    /// registry: a name registered, then cleared, no longer resolves.
    ///
    /// `clear()` wipes the *entire* table, not just this test's
    /// `clear.test.*` keys — so the `route_registry` serial key is what
    /// actually isolates it. Every other registry-touching test shares
    /// that key (see the module docstring); without that, this global
    /// drain could land between another test's registration and its
    /// lookup or conflict panic and make it spuriously fail. The unique
    /// names only keep *this* test's own assertions unambiguous.
    #[test]
    #[serial_test::serial(route_registry)]
    fn clear_route_names_drains_the_process_global_registry() {
        // Land a binding so the registry has something to drain.
        let _ = Router::new()
            .get("/before-clear", h)
            .name("clear.test.before");
        assert!(
            route("clear.test.before", &[]).is_some(),
            "pre-clear name must be resolvable",
        );

        clear_route_names_for_test();

        assert!(
            route("clear.test.before", &[]).is_none(),
            "clear_route_names_for_test must drain prior bindings",
        );

        // Subsequent registrations work normally — the OnceLock holds
        // the same RwLock<HashMap>, only its contents got cleared.
        let _ = Router::new()
            .get("/after-clear", h)
            .name("clear.test.after");
        assert_eq!(
            route("clear.test.after", &[]),
            Some("/after-clear".to_string()),
        );
    }

    /// `RouteBuilder::any` and `RouteBuilder::methods` chain off a
    /// prior route registration.
    #[test]
    fn route_builder_chains_any_and_methods() {
        let router: Router = Router::new().get("/r", h).any("/any", h).into();
        assert!(router.match_route(&Method::GET, "/r").is_some());
        for m in [Method::GET, Method::POST, Method::PATCH, Method::DELETE] {
            assert!(
                router.match_route(&m, "/any").is_some(),
                "any chain must register {m}",
            );
        }

        let router: Router = Router::new()
            .get("/r", h)
            .methods(&[Method::PUT, Method::PATCH], "/m", h)
            .into();
        assert!(router.match_route(&Method::PUT, "/m").is_some());
        assert!(router.match_route(&Method::PATCH, "/m").is_some());
        assert!(router.match_route(&Method::GET, "/m").is_none());
    }
}
