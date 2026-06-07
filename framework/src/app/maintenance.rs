//! Maintenance mode — Suprnova's analogue of Laravel's `down` / `up`.
//!
//! While the application is "down", [`MaintenanceMiddleware`] short-circuits
//! every request with a `503 Service Unavailable` (configurable status code),
//! optional `Retry-After` / `Refresh` headers, an optional redirect, and an
//! optional bypass: visiting the secret URL sets an encrypted cookie that lets
//! that browser through.
//!
//! State lives behind the [`MaintenanceMode`] trait with two drivers:
//! [`FileMaintenanceMode`] (a JSON file at `storage_path("framework/down")`,
//! the default) and [`CacheMaintenanceMode`] (a key in the shared [`Cache`],
//! for multi-node deployments without a shared filesystem). The driver is
//! chosen by the `MAINTENANCE_DRIVER` environment variable (`file` | `cache`).
//!
//! ```rust,ignore
//! use suprnova::{global_middleware, MaintenanceMiddleware};
//!
//! // In bootstrap.rs — health checks stay reachable while down.
//! global_middleware!(MaintenanceMiddleware::new().except(["api/health"]));
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::cache::Cache;
use crate::error::FrameworkError;
use crate::http::{Cookie, HttpResponse, Request, Response, SameSite};
use crate::middleware::{Middleware, Next};

/// Bypass cookie name (Laravel uses `laravel_maintenance`).
const BYPASS_COOKIE: &str = "suprnova_maintenance";

/// Cache key used by [`CacheMaintenanceMode`].
const CACHE_KEY: &str = "suprnova:maintenance";

/// How long a bypass cookie stays valid after visiting the secret URL.
const BYPASS_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// The data recorded when the application is taken down. Mirrors the fields
/// Laravel writes to its "down" file.
///
/// The `Debug` impl is hand-written rather than derived so a stray
/// `dbg!()` or `tracing::info!(?payload)` does not leak the bypass
/// `secret` (anyone who possesses it can issue themselves the bypass
/// cookie). Pattern mirrors [`crate::EncryptionKey`]'s redacting
/// `Debug`.
#[derive(Clone, Serialize, Deserialize)]
pub struct MaintenancePayload {
    /// Request paths that stay reachable while down — exact match or a
    /// trailing-`*` prefix (e.g. `"api/health"`, `"webhooks/*"`).
    #[serde(default)]
    pub except: Vec<String>,
    /// If set, requests are redirected here (`302`) instead of served the
    /// maintenance response.
    #[serde(default)]
    pub redirect: Option<String>,
    /// Seconds for the `Retry-After` header.
    #[serde(default)]
    pub retry: Option<u64>,
    /// Seconds for the `Refresh` header (browser auto-refresh).
    #[serde(default)]
    pub refresh: Option<u64>,
    /// Secret URL segment that, when visited, installs the bypass cookie.
    #[serde(default)]
    pub secret: Option<String>,
    /// Status code for the maintenance response (default `503`).
    #[serde(default = "default_status")]
    pub status: u16,
    /// Pre-rendered HTML body served instead of the plain text response.
    #[serde(default)]
    pub template: Option<String>,
}

impl std::fmt::Debug for MaintenancePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaintenancePayload")
            .field("except", &self.except)
            .field("redirect", &self.redirect)
            .field("retry", &self.retry)
            .field("refresh", &self.refresh)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .field("status", &self.status)
            .field("template", &self.template)
            .finish()
    }
}

fn default_status() -> u16 {
    503
}

impl Default for MaintenancePayload {
    fn default() -> Self {
        Self {
            except: Vec::new(),
            redirect: None,
            retry: None,
            refresh: None,
            secret: None,
            status: 503,
            template: None,
        }
    }
}

impl MaintenancePayload {
    /// A fresh payload: status `503`, no options set.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Storage backend for maintenance-mode state.
#[async_trait]
pub trait MaintenanceMode: Send + Sync {
    /// Record `payload` and put the application into maintenance mode.
    async fn activate(&self, payload: &MaintenancePayload) -> Result<(), FrameworkError>;
    /// Bring the application back up.
    async fn deactivate(&self) -> Result<(), FrameworkError>;
    /// Whether the application is currently down.
    async fn active(&self) -> Result<bool, FrameworkError>;
    /// The payload recorded by [`activate`](Self::activate).
    async fn data(&self) -> Result<MaintenancePayload, FrameworkError>;
}

/// File-backed maintenance state: a JSON file (default
/// `storage_path("framework/down")`). The default driver.
pub struct FileMaintenanceMode {
    path: PathBuf,
}

impl FileMaintenanceMode {
    /// Use the default path, `storage_path("framework/down")`.
    pub fn new() -> Self {
        Self {
            path: super::paths::storage_path("framework/down"),
        }
    }

    /// Use an explicit path (primarily for tests).
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Default for FileMaintenanceMode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MaintenanceMode for FileMaintenanceMode {
    async fn activate(&self, payload: &MaintenancePayload) -> Result<(), FrameworkError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                FrameworkError::internal(format!("maintenance: create {}: {e}", parent.display()))
            })?;
        }
        let json = serde_json::to_string_pretty(payload)
            .map_err(|e| FrameworkError::internal(format!("maintenance: serialize: {e}")))?;
        // Write to a sibling temp file then rename into place. `rename` is
        // atomic on the same filesystem, so a request reading the down file
        // concurrently with `down` never observes a half-written (and thus
        // unparseable) file — which would otherwise surface as a 500.
        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, json).await.map_err(|e| {
            FrameworkError::internal(format!("maintenance: write {}: {e}", tmp.display()))
        })?;
        tokio::fs::rename(&tmp, &self.path).await.map_err(|e| {
            FrameworkError::internal(format!(
                "maintenance: rename into {}: {e}",
                self.path.display()
            ))
        })
    }

    async fn deactivate(&self) -> Result<(), FrameworkError> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            // Already up — idempotent.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(FrameworkError::internal(format!(
                "maintenance: remove {}: {e}",
                self.path.display()
            ))),
        }
    }

    async fn active(&self) -> Result<bool, FrameworkError> {
        // tokio::fs::try_exists returns Ok(false) on NotFound (mirroring the
        // std::path::Path::exists semantics we used before) and Err on other
        // IO errors — surface those so a flaky FS shows up rather than being
        // silently treated as "up". Callers that want the prior fail-open
        // behaviour wrap with `.unwrap_or(false)` (see MaintenanceMiddleware).
        tokio::fs::try_exists(&self.path).await.map_err(|e| {
            FrameworkError::internal(format!("maintenance: probe {}: {e}", self.path.display()))
        })
    }

    async fn data(&self) -> Result<MaintenancePayload, FrameworkError> {
        let raw = tokio::fs::read_to_string(&self.path).await.map_err(|e| {
            FrameworkError::internal(format!("maintenance: read {}: {e}", self.path.display()))
        })?;
        serde_json::from_str(&raw).map_err(|e| {
            FrameworkError::internal(format!("maintenance: parse {}: {e}", self.path.display()))
        })
    }
}

/// Cache-backed maintenance state: a single key in the shared [`Cache`]. Use
/// this when multiple nodes must observe `down` / `up` without a shared
/// filesystem.
pub struct CacheMaintenanceMode {
    key: String,
}

impl CacheMaintenanceMode {
    /// Use the default cache key.
    pub fn new() -> Self {
        Self {
            key: CACHE_KEY.to_string(),
        }
    }

    /// Use an explicit cache key (primarily for tests).
    pub fn with_key(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl Default for CacheMaintenanceMode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MaintenanceMode for CacheMaintenanceMode {
    async fn activate(&self, payload: &MaintenancePayload) -> Result<(), FrameworkError> {
        Cache::put(&self.key, payload, None).await
    }

    async fn deactivate(&self) -> Result<(), FrameworkError> {
        // `forget` reports whether the key existed; deactivation is
        // idempotent and doesn't care, so discard it.
        Cache::forget(&self.key).await.map(|_| ())
    }

    async fn active(&self) -> Result<bool, FrameworkError> {
        Cache::has(&self.key).await
    }

    async fn data(&self) -> Result<MaintenancePayload, FrameworkError> {
        Cache::get::<MaintenancePayload>(&self.key)
            .await?
            .ok_or_else(|| FrameworkError::internal("maintenance: cache key absent"))
    }
}

/// The configured maintenance driver, chosen by `MAINTENANCE_DRIVER`
/// (`file` — default — or `cache`).
pub fn maintenance_mode() -> Arc<dyn MaintenanceMode> {
    match std::env::var("MAINTENANCE_DRIVER").as_deref() {
        Ok("cache") => Arc::new(CacheMaintenanceMode::new()),
        _ => Arc::new(FileMaintenanceMode::new()),
    }
}

/// Generate a random hex bypass secret (16 bytes → 32 hex chars), used by
/// `down --with-secret`. Hex keeps it safe as a URL path segment.
pub(crate) fn random_secret() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS RNG must be available to mint a maintenance secret");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Outcome of evaluating a request against maintenance state.
///
/// Split out of [`MaintenanceMiddleware::handle`] as a pure function so the
/// full decision matrix is unit-testable without a live server or an
/// encryption key.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Let the request reach the handler.
    Pass,
    /// The request hit the secret URL: install the bypass cookie, redirect home.
    GrantBypass,
    /// Redirect to the configured maintenance path.
    Redirect(String),
    /// Serve the maintenance response.
    Unavailable,
}

/// Pure maintenance decision. `has_valid_bypass_cookie` is computed by the
/// caller (it needs the encryption key) so this stays side-effect free.
fn decide(
    path: &str,
    payload: &MaintenancePayload,
    middleware_except: &[String],
    has_valid_bypass_cookie: bool,
) -> Decision {
    if middleware_except.iter().any(|p| path_matches(path, p))
        || payload.except.iter().any(|p| path_matches(path, p))
    {
        return Decision::Pass;
    }

    if let Some(secret) = payload.secret.as_deref().filter(|s| !s.is_empty()) {
        if path == secret.trim_start_matches('/') {
            return Decision::GrantBypass;
        }
        if has_valid_bypass_cookie {
            return Decision::Pass;
        }
    }

    if let Some(redirect) = payload.redirect.as_deref()
        && path != redirect.trim_start_matches('/')
    {
        return Decision::Redirect(redirect.to_string());
    }

    Decision::Unavailable
}

/// Global middleware that short-circuits requests while the application is in
/// maintenance mode. Register it with `global_middleware!`.
pub struct MaintenanceMiddleware {
    driver: Arc<dyn MaintenanceMode>,
    except: Vec<String>,
}

impl MaintenanceMiddleware {
    /// Build using the env-configured driver ([`maintenance_mode`]).
    pub fn new() -> Self {
        Self {
            driver: maintenance_mode(),
            except: Vec::new(),
        }
    }

    /// Build with an explicit driver.
    pub fn with_driver(driver: Arc<dyn MaintenanceMode>) -> Self {
        Self {
            driver,
            except: Vec::new(),
        }
    }

    /// Paths that stay reachable while down — exact match or a trailing-`*`
    /// prefix. Merged with any `except` recorded at `down` time.
    pub fn except<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.except.extend(paths.into_iter().map(Into::into));
        self
    }
}

impl Default for MaintenanceMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for MaintenanceMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let path = request.path().trim_start_matches('/').to_string();

        // Fast path: a middleware-level exception skips the backend probe.
        if self.except.iter().any(|p| path_matches(&path, p)) {
            return next(request).await;
        }

        // A backend error on the active() probe fails open: a flaky
        // maintenance store must not 503 an app that was never taken down.
        if !self.driver.active().await.unwrap_or(false) {
            return next(request).await;
        }

        let payload = match self.driver.data().await {
            Ok(p) => p,
            Err(e) => {
                // Race: state cleared between the active() probe and the
                // data() read. If we're up now, pass; else surface the error.
                if !self.driver.active().await.unwrap_or(false) {
                    return next(request).await;
                }
                return Err(HttpResponse::from(e));
            }
        };

        let has_cookie = payload
            .secret
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_some_and(|secret| has_valid_bypass_cookie(&request, secret));

        match decide(&path, &payload, &self.except, has_cookie) {
            Decision::Pass => next(request).await,
            Decision::GrantBypass => bypass_response(payload.secret.as_deref().unwrap_or_default()),
            Decision::Redirect(location) => {
                Err(HttpResponse::new().status(302).header("Location", location))
            }
            Decision::Unavailable => Err(service_unavailable(&payload)),
        }
    }
}

/// Build the maintenance response: the configured status (default `503`), the
/// template or a plain body, and the `Retry-After` / `Refresh` headers.
fn service_unavailable(payload: &MaintenancePayload) -> HttpResponse {
    let status = if payload.status == 0 {
        503
    } else {
        payload.status
    };
    let mut resp = match &payload.template {
        Some(html) => HttpResponse::html(html.clone()),
        None => HttpResponse::text("503 Service Unavailable"),
    }
    .status(status);
    if let Some(retry) = payload.retry {
        resp = resp.header("Retry-After", retry.to_string());
    }
    if let Some(refresh) = payload.refresh {
        resp = resp.header("Refresh", refresh.to_string());
    }
    resp
}

/// Redirect to the intended destination with the encrypted bypass cookie set.
fn bypass_response(secret: &str) -> Response {
    let cookie = Cookie::encrypted(BYPASS_COOKIE, secret)?
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(BYPASS_TTL);
    Err(HttpResponse::new()
        .status(302)
        .header("Location", "/")
        .cookie(cookie))
}

/// Whether the request carries a bypass cookie that decrypts to `secret`.
fn has_valid_bypass_cookie(request: &Request, secret: &str) -> bool {
    request
        .cookie(BYPASS_COOKIE)
        .and_then(|wire| Cookie::read_encrypted(&wire).ok())
        .is_some_and(|plain| plain == secret)
}

/// Match a normalized request path (no leading `/`) against an `except`
/// pattern: exact, or a trailing-`*` prefix. `"*"` matches everything.
fn path_matches(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_start_matches('/');
    match pattern.strip_suffix('*') {
        Some(prefix) => {
            let prefix = prefix.trim_end_matches('/');
            prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
        }
        None => path == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_down_path() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("suprnova-maint-{}-{unique}", std::process::id()));
        p.push("framework/down");
        p
    }

    fn down(secret: Option<&str>) -> MaintenancePayload {
        MaintenancePayload {
            secret: secret.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn payload_deserializes_with_default_status_503() {
        let p: MaintenancePayload = serde_json::from_str(r#"{"secret":"abc"}"#).unwrap();
        assert_eq!(p.status, 503);
        assert_eq!(p.secret.as_deref(), Some("abc"));
        assert!(p.except.is_empty());
        assert_eq!(p.retry, None);
    }

    #[test]
    fn path_matching_exact_and_wildcard() {
        assert!(path_matches("api/health", "api/health"));
        assert!(!path_matches("api/health", "api/status"));
        assert!(path_matches("webhooks/stripe", "webhooks/*"));
        assert!(path_matches("webhooks", "webhooks/*")); // the prefix itself
        assert!(!path_matches("web", "webhooks/*"));
        assert!(path_matches("anything/here", "*"));
        assert!(path_matches("admin", "/admin")); // leading slash tolerated
    }

    #[test]
    fn decide_serves_503_for_a_plain_request_while_down() {
        assert_eq!(
            decide("dashboard", &down(None), &[], false),
            Decision::Unavailable
        );
    }

    #[test]
    fn decide_passes_payload_and_middleware_exceptions() {
        let mut p = down(None);
        p.except = vec!["api/health".into()];
        assert_eq!(decide("api/health", &p, &[], false), Decision::Pass);
        assert_eq!(
            decide("status", &down(None), &["status".to_string()], false),
            Decision::Pass
        );
    }

    #[test]
    fn decide_grants_bypass_only_on_the_secret_url() {
        let p = down(Some("let-me-in"));
        assert_eq!(decide("let-me-in", &p, &[], false), Decision::GrantBypass);
        assert_eq!(decide("elsewhere", &p, &[], false), Decision::Unavailable);
    }

    #[test]
    fn decide_passes_when_bypass_cookie_is_valid() {
        let p = down(Some("let-me-in"));
        assert_eq!(decide("dashboard", &p, &[], true), Decision::Pass);
    }

    #[test]
    fn decide_ignores_an_empty_secret() {
        let p = down(Some(""));
        assert_eq!(decide("", &p, &[], false), Decision::Unavailable);
    }

    #[test]
    fn decide_redirects_except_for_the_redirect_target_itself() {
        let mut p = down(None);
        p.redirect = Some("/maintenance".into());
        assert_eq!(
            decide("dashboard", &p, &[], false),
            Decision::Redirect("/maintenance".to_string())
        );
        // The redirect target serves the page rather than looping.
        assert_eq!(decide("maintenance", &p, &[], false), Decision::Unavailable);
    }

    #[tokio::test]
    async fn file_driver_full_lifecycle() {
        let driver = FileMaintenanceMode::with_path(temp_down_path());
        assert!(!driver.active().await.unwrap());

        let payload = MaintenancePayload {
            retry: Some(60),
            secret: Some("letmein".into()),
            except: vec!["api/health".into()],
            ..Default::default()
        };
        driver.activate(&payload).await.unwrap();
        assert!(driver.active().await.unwrap());

        let read = driver.data().await.unwrap();
        assert_eq!(read.retry, Some(60));
        assert_eq!(read.secret.as_deref(), Some("letmein"));
        assert_eq!(read.except, vec!["api/health".to_string()]);
        assert_eq!(read.status, 503);

        driver.deactivate().await.unwrap();
        assert!(!driver.active().await.unwrap());
        // Idempotent: deactivating when already up is fine.
        driver.deactivate().await.unwrap();
    }

    #[tokio::test]
    async fn cache_driver_full_lifecycle() {
        // Memory cache by default; ignore an already-bootstrapped error.
        let _ = Cache::bootstrap().await;
        let driver =
            CacheMaintenanceMode::with_key(format!("test:maint:{}", temp_down_path().display()));
        assert!(!driver.active().await.unwrap());

        let payload = MaintenancePayload {
            refresh: Some(15),
            status: 418,
            ..Default::default()
        };
        driver.activate(&payload).await.unwrap();
        assert!(driver.active().await.unwrap());
        let read = driver.data().await.unwrap();
        assert_eq!(read.refresh, Some(15));
        assert_eq!(read.status, 418);

        driver.deactivate().await.unwrap();
        assert!(!driver.active().await.unwrap());
    }

    #[test]
    fn service_unavailable_uses_configured_status_with_503_fallback() {
        // Explicit status is honored (Laravel's `down --status`).
        let mut payload = MaintenancePayload {
            status: 418,
            ..Default::default()
        };
        assert_eq!(service_unavailable(&payload).status_code(), 418);
        // An unset (0) status falls back to 503.
        payload.status = 0;
        assert_eq!(service_unavailable(&payload).status_code(), 503);
        payload.status = 503;
        assert_eq!(service_unavailable(&payload).status_code(), 503);
    }
}
