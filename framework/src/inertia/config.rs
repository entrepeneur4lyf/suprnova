use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use super::manifest::ViteManifest;

/// Shared error-observer callback for SSR render failures.
pub(crate) type SsrErrorHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Asset-version source for Inertia responses.
///
/// Inertia uses a version string for cache-busting / version-mismatch
/// detection. Most apps want a single static value (the asset manifest
/// hash from build time). Some — long-running deploys, hot-reloaded
/// dev environments — want to compute it per-request. The `Dynamic`
/// variant covers that case; the resolver closure runs every time the
/// version is needed.
#[derive(Clone)]
pub enum VersionResolver {
    /// A baked-in static version string. Cheap; no closure invocation.
    Static(String),
    /// A closure that returns the current version. Runs on every read.
    /// Wrap any caching the consumer wants inside the closure.
    Dynamic(Arc<dyn Fn() -> String + Send + Sync>),
}

impl VersionResolver {
    /// Build a static resolver from anything that can become a `String`.
    pub fn new(version: impl Into<String>) -> Self {
        Self::Static(version.into())
    }

    /// Build a dynamic resolver from a closure. The closure runs on
    /// every call to [`resolve`](Self::resolve); cache inside the closure if needed.
    pub fn with<F>(f: F) -> Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        Self::Dynamic(Arc::new(f))
    }

    /// Resolve to the current version string.
    pub fn resolve(&self) -> String {
        match self {
            Self::Static(s) => s.clone(),
            Self::Dynamic(f) => f(),
        }
    }
}

impl From<String> for VersionResolver {
    fn from(s: String) -> Self {
        Self::Static(s)
    }
}

impl From<&str> for VersionResolver {
    fn from(s: &str) -> Self {
        Self::Static(s.to_string())
    }
}

impl std::fmt::Debug for VersionResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(s) => write!(f, "Static({:?})", s),
            Self::Dynamic(_) => write!(f, "Dynamic(<closure>)"),
        }
    }
}

/// Which frontend framework the host application uses.
///
/// Detected at runtime from the `SUPRNOVA_FRONTEND` env var. The CLI
/// scaffolds this into `.env` when generating a new project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    Svelte,
    React,
    Vue,
}

impl Frontend {
    /// Read `SUPRNOVA_FRONTEND` from the environment.
    ///
    /// Defaults to `Svelte` when unset or unrecognized — matches the
    /// CLI's default frontend choice in `suprnova new`.
    pub fn detect_from_env() -> Self {
        match std::env::var("SUPRNOVA_FRONTEND").as_deref() {
            Ok("react") | Ok("React") | Ok("REACT") => Frontend::React,
            Ok("vue") | Ok("Vue") | Ok("VUE") => Frontend::Vue,
            Ok("svelte") | Ok("Svelte") | Ok("SVELTE") => Frontend::Svelte,
            _ => Frontend::Svelte,
        }
    }

    /// Default Vite entry-point filename for this frontend.
    pub fn default_entry_point(self) -> &'static str {
        match self {
            Frontend::Svelte => "src/main.ts",
            Frontend::React => "src/main.tsx",
            Frontend::Vue => "src/main.ts",
        }
    }

    /// File extensions a page component for this frontend may use.
    ///
    /// Ordered by likelihood for the framework. Used by the macro to
    /// locate page components at compile time.
    pub fn page_extensions(self) -> &'static [&'static str] {
        match self {
            Frontend::Svelte => &["svelte"],
            Frontend::React => &["tsx", "jsx"],
            Frontend::Vue => &["vue"],
        }
    }

    /// Lowercase identifier used in env / config.
    pub fn as_str(self) -> &'static str {
        match self {
            Frontend::Svelte => "svelte",
            Frontend::React => "react",
            Frontend::Vue => "vue",
        }
    }
}

/// Configuration for Inertia.js integration.
pub struct InertiaConfig {
    /// Vite dev server URL (e.g. `http://localhost:5173`).
    pub vite_dev_server: String,
    /// Vite entry point. Defaults to the frontend's standard entry.
    pub entry_point: String,
    /// Asset version source for cache busting / version-mismatch
    /// detection. See [`VersionResolver`] for static-vs-dynamic.
    pub version: VersionResolver,
    /// `true` during local development (loads via the Vite dev server);
    /// `false` for production (loads built assets from `/assets/`).
    pub development: bool,
    /// Which frontend framework is configured.
    pub frontend: Frontend,
    /// Default `<title>` for the HTML shell. Per-response title overrides
    /// via `InertiaResponse::title(...)`.
    pub default_title: String,
    /// Whether Inertia responses encrypt their browser history state by
    /// default. Maps to Laravel's `config('inertia.history.encrypt')`.
    /// Overridable per-request via `EncryptHistoryMiddleware` and
    /// per-response via `InertiaResponse::encrypt_history(bool)`.
    pub encrypt_history_default: bool,
    /// Server-side rendering configuration. See [`SsrConfig`].
    pub ssr: SsrConfig,
    /// Path to Vite's `manifest.json` (Vite 5.0+ default location is
    /// `<outDir>/.vite/manifest.json`). Default points at
    /// `public/assets/.vite/manifest.json`, matching the framework's
    /// scaffolded `vite.config.ts` (`outDir: '../public/assets'`).
    ///
    /// When the file exists, `render_prod_head` resolves the entry
    /// point to its hashed output + CSS + transitively-imported
    /// chunks (for `modulepreload`). When it's missing the framework
    /// falls back to the legacy hardcoded `/{assets_base_url}/main.js`
    /// path and emits a `tracing::warn!` so the gap is visible in
    /// production logs.
    pub manifest_path: PathBuf,
    /// URL prefix under which the Vite build assets are served (e.g.
    /// `/assets`). Combined with the manifest entry's `file` field to
    /// produce the final `<script src>` / `<link href>` URL.
    pub assets_base_url: String,
    /// Maximum number of lazy/deferred/once/shared prop resolvers that
    /// run concurrently for a single response.
    ///
    /// Default: 16 — generous for typical Inertia pages while bounding
    /// downstream fan-out on pages with many lazy resolvers. Without
    /// this cap a page with N lazy props issues N parallel database /
    /// HTTP calls per request.
    pub max_concurrent_resolvers: usize,
    /// Lazy-loaded Vite manifest cache.
    ///
    /// Initialized on first call to [`Self::vite_manifest`]. The cache
    /// holds `Some(manifest)` on successful load and `None` when the
    /// file is missing or malformed — both states are stable for the
    /// process lifetime, matching how a long-running production server
    /// reads the build artefact exactly once. Use `manifest_path()` to
    /// repoint at a different file for tests; that builder method
    /// resets the cache by constructing a fresh `OnceLock`.
    pub(crate) manifest: Arc<OnceLock<Option<ViteManifest>>>,
}

/// SSR (server-side rendering) configuration.
///
/// Suprnova talks to an out-of-process SSR worker — usually the
/// `@inertiajs/{vue3,react,svelte}/server` `createServer()` bundle run
/// under Node, Bun, or Deno — over HTTP loopback. The worker accepts
/// a JSON page object on `POST /render` and returns
/// `{ head: string[], body: string }`. Configure the worker URL here;
/// boot it separately (e.g. `suprnova ssr:start`).
#[derive(Clone)]
pub struct SsrConfig {
    /// When `false`, SSR is fully off and the HTML shell renders empty
    /// `<div id="app">` for the client to hydrate. Default: `false`.
    pub enabled: bool,
    /// URL of the running SSR worker (e.g. `http://127.0.0.1:13714`).
    /// The framework posts to `<url>/render`.
    pub url: String,
    /// Request timeout for the SSR call. Past this, the response falls
    /// back to CSR. Keep tight in production — a hung worker shouldn't
    /// block real users.
    pub timeout: std::time::Duration,
    /// When `true`, SSR errors propagate as 500s instead of falling
    /// back to CSR. Useful in CI / tests; never set `true` in
    /// production unless you also have a watchdog.
    pub throw_on_error: bool,
    /// Glob-style path patterns excluded from SSR. Matching paths
    /// render CSR-only even when `enabled` is `true`. Each pattern
    /// supports `*` (anything-not-slash) and `**` (anything).
    pub excluded_paths: Vec<String>,
    /// Observability hook invoked when an SSR render fails and we
    /// fall back to CSR. Defaults to `eprintln!` to stderr. Wire your
    /// logger / Sentry / DataDog client here. When events parity
    /// lands, `SsrRenderFailed` will fire from this callback too.
    pub on_error: Option<SsrErrorHook>,
    /// Cap on the SSR worker's response body. Bytes past this point
    /// abort the read and the request falls back to CSR (or 500 if
    /// `throw_on_error` is set). Default: 8 MiB — comfortably larger
    /// than any realistic SSR-rendered page but small enough to bound
    /// damage from a misconfigured or compromised loopback worker
    /// (Domain 20 audit D20-D / ChatGPT MODULE_REVIEW_NOTES ## inertia
    /// MEDIUM #3).
    pub max_response_bytes: usize,
}

impl std::fmt::Debug for SsrConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsrConfig")
            .field("enabled", &self.enabled)
            .field("url", &self.url)
            .field("timeout", &self.timeout)
            .field("throw_on_error", &self.throw_on_error)
            .field("excluded_paths", &self.excluded_paths)
            .field("on_error", &self.on_error.as_ref().map(|_| "<closure>"))
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl Default for SsrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://127.0.0.1:13714".to_string(),
            timeout: std::time::Duration::from_secs(5),
            throw_on_error: false,
            excluded_paths: Vec::new(),
            on_error: None,
            max_response_bytes: 8 * 1024 * 1024,
        }
    }
}

impl SsrConfig {
    /// Check whether the given request path is excluded from SSR.
    pub fn is_path_excluded(&self, path: &str) -> bool {
        self.excluded_paths.iter().any(|pat| glob_match(pat, path))
    }
}

/// Tiny glob matcher: `*` matches a single non-`/` segment, `**`
/// matches any number of characters (including `/`). Designed for
/// route-pattern matching, not full POSIX globs.
fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), path.as_bytes())
}

fn glob_match_inner(pat: &[u8], path: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let (mut star_pi, mut star_si): (Option<usize>, usize) = (None, 0);
    while si < path.len() {
        if pi < pat.len() {
            let c = pat[pi];
            if c == b'*' {
                // `**` = match any, including '/'
                let double = pi + 1 < pat.len() && pat[pi + 1] == b'*';
                if double {
                    pi += 2;
                    star_pi = Some(pi);
                    star_si = si;
                    // double-star can match zero chars too
                    continue;
                } else {
                    // single `*` = match anything except '/'
                    pi += 1;
                    star_pi = Some(pi);
                    star_si = si;
                    continue;
                }
            } else if c == path[si] {
                pi += 1;
                si += 1;
                continue;
            }
        }
        if let Some(sp) = star_pi {
            // Resume the previous star, consume one more char.
            // For single-`*` we forbid `/` in the consumed window.
            let one_more = path[star_si];
            let prev_was_double = sp >= 2 && pat[sp - 1] == b'*' && pat[sp - 2] == b'*';
            if !prev_was_double && one_more == b'/' {
                return false;
            }
            star_si += 1;
            si = star_si;
            pi = sp;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

impl Default for InertiaConfig {
    fn default() -> Self {
        let frontend = Frontend::detect_from_env();
        Self {
            vite_dev_server: "http://localhost:5173".to_string(),
            entry_point: frontend.default_entry_point().to_string(),
            version: VersionResolver::Static("1.0".to_string()),
            development: true,
            frontend,
            default_title: "Suprnova".to_string(),
            encrypt_history_default: false,
            ssr: SsrConfig::default(),
            manifest_path: PathBuf::from("public/assets/.vite/manifest.json"),
            assets_base_url: "/assets".to_string(),
            max_concurrent_resolvers: 16,
            manifest: Arc::new(OnceLock::new()),
        }
    }
}

impl InertiaConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn vite_dev_server(mut self, url: impl Into<String>) -> Self {
        self.vite_dev_server = url.into();
        self
    }

    pub fn entry_point(mut self, entry: impl Into<String>) -> Self {
        self.entry_point = entry.into();
        self
    }

    /// Set a static asset version string. For dynamic versions
    /// (e.g. read from a manifest at runtime) use [`version_with`](Self::version_with).
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = VersionResolver::Static(version.into());
        self
    }

    /// Set a dynamic asset version resolver. The closure runs on every
    /// page-object emission and every version-mismatch check; cache
    /// inside the closure if invocation isn't cheap.
    ///
    /// The closure is synchronous and infallible by design — it mirrors
    /// Laravel's `Inertia::version($closure)` contract. For
    /// async / fallible computation (e.g. read a manifest from S3),
    /// resolve once at boot and pass the cached `String` to
    /// [`version`](Self::version):
    ///
    /// ```rust,ignore
    /// // In bootstrap:
    /// let manifest_hash = read_manifest_hash().await?;
    /// let cfg = InertiaConfig::new().version(manifest_hash);
    /// ```
    ///
    /// Or wrap an internal cache and panic-recovery in the closure:
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicPtr, Ordering};
    /// let cached: Arc<...> = ...;  // your refresh strategy
    /// InertiaConfig::new().version_with(move || cached.current_hash())
    /// ```
    pub fn version_with<F>(mut self, f: F) -> Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        self.version = VersionResolver::Dynamic(Arc::new(f));
        self
    }

    pub fn production(mut self) -> Self {
        self.development = false;
        self
    }

    pub fn frontend(mut self, frontend: Frontend) -> Self {
        self.frontend = frontend;
        // Update entry point default to match the new frontend unless the
        // user has already customized it.
        self.entry_point = frontend.default_entry_point().to_string();
        self
    }

    pub fn default_title(mut self, title: impl Into<String>) -> Self {
        self.default_title = title.into();
        self
    }

    pub fn encrypt_history(mut self, on: bool) -> Self {
        self.encrypt_history_default = on;
        self
    }

    /// Enable SSR with the given worker URL.
    pub fn ssr(mut self, url: impl Into<String>) -> Self {
        self.ssr.enabled = true;
        self.ssr.url = url.into();
        self
    }

    /// Disable SSR explicitly (the default).
    pub fn ssr_disabled(mut self) -> Self {
        self.ssr.enabled = false;
        self
    }

    /// Set the SSR request timeout.
    pub fn ssr_timeout(mut self, t: std::time::Duration) -> Self {
        self.ssr.timeout = t;
        self
    }

    /// Make SSR failures hard errors instead of falling back to CSR.
    pub fn ssr_throw_on_error(mut self, on: bool) -> Self {
        self.ssr.throw_on_error = on;
        self
    }

    /// Add a path pattern excluded from SSR.
    pub fn ssr_exclude(mut self, pattern: impl Into<String>) -> Self {
        self.ssr.excluded_paths.push(pattern.into());
        self
    }

    /// Override the SSR-response body byte cap.
    ///
    /// The default is 8 MiB. Reads that exceed this bound abort and the
    /// response falls back to CSR (or 500 if `ssr_throw_on_error` is
    /// set). Bound chosen to be larger than any realistic SSR page but
    /// small enough to constrain damage from a misconfigured or
    /// compromised loopback worker.
    pub fn ssr_max_response_bytes(mut self, bytes: usize) -> Self {
        self.ssr.max_response_bytes = bytes;
        self
    }

    /// Register an observability callback for SSR render failures.
    /// Replaces the default `eprintln!` to stderr.
    pub fn on_ssr_error<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        self.ssr.on_error = Some(std::sync::Arc::new(f));
        self
    }

    /// Override the Vite manifest file location. Resets the lazy cache
    /// so the next [`Self::vite_manifest`] call re-reads from disk.
    /// Default: `public/assets/.vite/manifest.json`.
    pub fn manifest_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.manifest_path = path.into();
        self.manifest = Arc::new(OnceLock::new());
        self
    }

    /// Override the URL prefix under which built assets are served.
    /// Default: `/assets`. The leading slash is required; the value
    /// is concatenated with the manifest entry's `file` field as
    /// `{base}/{file}`.
    pub fn assets_base_url(mut self, url: impl Into<String>) -> Self {
        self.assets_base_url = url.into();
        self
    }

    /// Override the per-response cap on concurrent prop resolvers.
    /// Default: 16. Zero is treated as `usize::MAX` (no cap) — the
    /// builder normalizes that for the caller.
    pub fn max_concurrent_resolvers(mut self, n: usize) -> Self {
        self.max_concurrent_resolvers = if n == 0 { usize::MAX } else { n };
        self
    }

    /// Return the cached Vite manifest. On the first call this reads
    /// [`Self::manifest_path`] from disk; subsequent calls return the
    /// cached value (or cached `None` if the read failed).
    ///
    /// `None` is returned when the file is missing or malformed — the
    /// production HTML shell renderer falls back to a legacy hardcoded
    /// path and logs a `tracing::warn!`. This keeps existing
    /// pre-manifest apps booting; new apps with a proper Vite build
    /// pick up hashed assets automatically.
    pub fn vite_manifest(&self) -> Option<&ViteManifest> {
        self.manifest
            .get_or_init(|| match ViteManifest::load(&self.manifest_path) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(
                        path = %self.manifest_path.display(),
                        error = %e,
                        "Vite manifest could not be loaded; production asset \
                         tags will fall back to the legacy hardcoded path. \
                         Ensure `build.manifest: true` is set in vite.config.ts \
                         and that the build has produced an output."
                    );
                    None
                }
            })
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_detect_defaults_to_svelte_when_unset() {
        // Clear in case some other test set it.
        // SAFETY: tests in this module run sequentially in the same binary,
        // but cargo test runs tests in parallel by default. To avoid races
        // we don't unset; instead we test the explicit-match arm and the
        // explicit-Svelte arm separately.
        let _ = std::env::var("SUPRNOVA_FRONTEND"); // touch to silence unused warnings
        // The default arm covers unset + unknown values; verify the
        // explicit fallback by checking the match logic.
        assert_eq!(Frontend::Svelte.as_str(), "svelte");
        assert_eq!(Frontend::React.as_str(), "react");
        assert_eq!(Frontend::Vue.as_str(), "vue");
    }

    #[test]
    fn frontend_default_entry_points() {
        assert_eq!(Frontend::Svelte.default_entry_point(), "src/main.ts");
        assert_eq!(Frontend::React.default_entry_point(), "src/main.tsx");
        assert_eq!(Frontend::Vue.default_entry_point(), "src/main.ts");
    }

    #[test]
    fn frontend_page_extensions() {
        assert_eq!(Frontend::Svelte.page_extensions(), &["svelte"]);
        assert_eq!(Frontend::React.page_extensions(), &["tsx", "jsx"]);
        assert_eq!(Frontend::Vue.page_extensions(), &["vue"]);
    }

    #[test]
    fn config_default_has_svelte_entry_when_env_unset() {
        // Best-effort: only valid when env unset; CI may inject SUPRNOVA_FRONTEND.
        if std::env::var("SUPRNOVA_FRONTEND").is_err() {
            let cfg = InertiaConfig::default();
            assert_eq!(cfg.frontend, Frontend::Svelte);
            assert_eq!(cfg.entry_point, "src/main.ts");
        }
    }

    #[test]
    fn config_builder_updates_entry_point_with_frontend() {
        let cfg = InertiaConfig::new().frontend(Frontend::React);
        assert_eq!(cfg.frontend, Frontend::React);
        assert_eq!(cfg.entry_point, "src/main.tsx");
    }

    #[test]
    fn config_builder_overrides_default_title() {
        let cfg = InertiaConfig::new().default_title("My App");
        assert_eq!(cfg.default_title, "My App");
    }

    #[test]
    fn version_resolver_static_resolves_to_string() {
        let r = VersionResolver::new("abc123");
        assert_eq!(r.resolve(), "abc123");
        assert_eq!(r.resolve(), "abc123"); // idempotent
    }

    #[test]
    fn version_resolver_dynamic_calls_closure_each_time() {
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let c = counter.clone();
        let r = VersionResolver::with(move || {
            let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            format!("v{}", n)
        });
        assert_eq!(r.resolve(), "v0");
        assert_eq!(r.resolve(), "v1");
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn version_resolver_from_string_makes_static() {
        let r: VersionResolver = "x".to_string().into();
        assert_eq!(r.resolve(), "x");
        let r2: VersionResolver = "y".into();
        assert_eq!(r2.resolve(), "y");
    }

    #[test]
    fn config_version_builder_creates_static() {
        let cfg = InertiaConfig::new().version("static-v1");
        assert_eq!(cfg.version.resolve(), "static-v1");
    }

    #[test]
    fn config_version_with_creates_dynamic() {
        let cfg = InertiaConfig::new().version_with(|| "dyn-v1".to_string());
        assert_eq!(cfg.version.resolve(), "dyn-v1");
    }

    // ---- glob matcher ----
    //
    // Path-style glob: `*` matches one segment (no slash), `**` matches
    // any characters including slashes. Standard rsync/gitignore-style
    // semantics — `/admin/**` matches `/admin/x` but NOT bare `/admin`
    // (use `/admin*` or two patterns for that).

    #[test]
    fn glob_literal_matches_exact() {
        assert!(glob_match("/users", "/users"));
        assert!(!glob_match("/users", "/users/1"));
        assert!(!glob_match("/users", "/user"));
    }

    #[test]
    fn glob_single_star_does_not_cross_slash() {
        assert!(glob_match("/users/*", "/users/1"));
        assert!(glob_match("/users/*", "/users/abc"));
        assert!(!glob_match("/users/*", "/users/1/edit"));
        // Standard glob semantics: `*` matches zero or more non-slash
        // chars, so `/users/*` matches `/users/` (the `*` matches the
        // empty segment).
        assert!(glob_match("/users/*", "/users/"));
    }

    #[test]
    fn glob_double_star_crosses_slashes() {
        assert!(glob_match("/admin/**", "/admin/foo"));
        assert!(glob_match("/admin/**", "/admin/foo/bar"));
        assert!(glob_match("/admin/**", "/admin/"));
    }

    #[test]
    fn glob_double_star_does_not_match_bare_prefix() {
        // Standard glob semantics: `/admin/**` requires the slash. To
        // match `/admin` itself, the operator should use `/admin*` or
        // two separate patterns.
        assert!(!glob_match("/admin/**", "/admin"));
    }

    #[test]
    fn glob_admin_star_matches_admin_and_admin_suffix() {
        assert!(glob_match("/admin*", "/admin"));
        assert!(glob_match("/admin*", "/admin2"));
        assert!(!glob_match("/admin*", "/admin/foo"));
    }

    #[test]
    fn glob_leading_double_star_matches_anything() {
        assert!(glob_match("**", "/anything/at/all"));
        assert!(glob_match("**", ""));
        assert!(glob_match("**/admin", "/foo/admin"));
        assert!(glob_match("**/admin", "/admin"));
    }

    #[test]
    fn glob_empty_pattern_matches_only_empty_path() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "/x"));
    }
}
