//! SSR client + per-request opt-out.
//!
//! Inertia v3 SSR runs as a separate process (Node/Bun/Deno) using
//! `@inertiajs/{vue3,react,svelte}/server` `createServer()`. The worker
//! listens on HTTP and accepts the page object as JSON; we POST it and
//! receive `{ head: string[], body: string }` back.
//!
//! Suprnova talks to that worker over loopback HTTP. We don't manage
//! the worker process from the framework — `suprnova-cli` ships
//! `ssr:start` for that, and operators are free to use their own
//! supervisor.

use serde::Deserialize;
use std::time::Duration;

use crate::error::FrameworkError;
use crate::inertia::config::SsrConfig;

// Note: we don't define a typed request struct — the `@inertiajs/*/server`
// `createServer()` workers accept the raw page object JSON envelope.
// We send `serde_json::Value` directly to avoid an extra serialize step.

/// Response from the SSR worker. Heads is a list of `<head>` snippets
/// (e.g. `<title>...</title>`, `<meta ...>`); body is the prerendered
/// app shell.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SsrResponse {
    #[serde(default)]
    pub head: Vec<String>,
    #[serde(default)]
    pub body: String,
}

// Per-request opt-out for SSR. Mirrors Laravel's
// `Inertia::disable_ssr()`. The flag is an `Arc<AtomicBool>` so the
// scope is set once (by the server when wrapping each request) and
// the handler can flip it during execution without needing to
// re-enter a new scope.
tokio::task_local! {
    pub(crate) static DISABLE_SSR: std::sync::Arc<std::sync::atomic::AtomicBool>;
}

/// Disable SSR for the rest of this request. Idempotent. No-op when
/// called outside a request scope (e.g. unit tests that don't wire up
/// the server's task-local scope).
pub fn disable_ssr_for_request() {
    let _ = DISABLE_SSR.try_with(|flag| {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
}

/// Check whether SSR has been disabled for the current task. Returns
/// `false` outside any scope (the default — caller's config wins).
pub fn is_disabled_for_request() -> bool {
    DISABLE_SSR
        .try_with(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
        .unwrap_or(false)
}

/// Initial scope value used by the server. Public so `crate::server`
/// can wrap each request without having to touch the internals.
#[doc(hidden)]
pub fn new_disable_ssr_flag() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
}

/// Render via the SSR worker. Returns `Ok(Some(_))` when SSR succeeded,
/// `Ok(None)` when SSR was disabled or the path was excluded (caller
/// falls back to CSR), and `Err` only when `throw_on_error` is true.
pub(crate) async fn render(
    config: &SsrConfig,
    path: &str,
    page: &serde_json::Value,
) -> Result<Option<SsrResponse>, FrameworkError> {
    if !config.enabled {
        return Ok(None);
    }
    if is_disabled_for_request() {
        return Ok(None);
    }
    if config.is_path_excluded(path) {
        return Ok(None);
    }

    let body = serde_json::to_vec(page).map_err(|e| {
        FrameworkError::internal(format!("SSR page serialization failed: {e}"))
    })?;
    let url = format!("{}/render", config.url.trim_end_matches('/'));

    let result = post_json(&url, body, config.timeout).await;
    match result {
        Ok(resp) => Ok(Some(resp)),
        Err(e) => {
            if config.throw_on_error {
                Err(FrameworkError::internal(format!(
                    "SSR render failed: {e}"
                )))
            } else {
                let msg = format!(
                    "SSR worker unreachable at {} ({}); falling back to CSR",
                    url, e
                );
                if let Some(cb) = &config.on_error {
                    cb(&msg);
                } else {
                    eprintln!("[inertia] {}", msg);
                }
                Ok(None)
            }
        }
    }
}

/// Process-global hyper client shared across all SSR calls.
///
/// Constructing a `Client` is expensive — it sets up a connection pool
/// and an HTTP/1.1 handshake state. A per-request `Client` resets the
/// pool every time, so we keep one for the lifetime of the process.
/// `hyper_util::client::legacy::Client` is `Clone`-cheap (`Arc` inside)
/// and `Send + Sync`, so a `OnceLock` works.
fn shared_client() -> &'static hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    http_body_util::Full<bytes::Bytes>,
> {
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;
    use std::sync::OnceLock;

    static SSR_CLIENT: OnceLock<
        Client<
            hyper_util::client::legacy::connect::HttpConnector,
            http_body_util::Full<bytes::Bytes>,
        >,
    > = OnceLock::new();
    SSR_CLIENT.get_or_init(|| Client::builder(TokioExecutor::new()).build_http())
}

/// POST JSON to the SSR worker and deserialize the response. Uses
/// `hyper` directly — we already depend on it, so no extra crate.
async fn post_json(
    url: &str,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<SsrResponse, String> {
    use http_body_util::{BodyExt, Full};
    use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
    use hyper::Request;

    let parsed = hyper::Uri::try_from(url).map_err(|e| format!("invalid url: {e}"))?;

    let host_port = format!(
        "{}:{}",
        parsed.host().ok_or("missing host")?,
        parsed.port_u16().unwrap_or(80)
    );

    let req = Request::builder()
        .method("POST")
        .uri(url)
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_LENGTH, body.len())
        .header("Host", host_port)
        .body(Full::new(bytes::Bytes::from(body)))
        .map_err(|e| format!("request build: {e}"))?;

    let client = shared_client();
    let fut = client.request(req);
    let resp = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| format!("timeout after {:?}", timeout))?
        .map_err(|e| format!("hyper: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("ssr worker returned {}", status));
    }

    let collected = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("read body: {e}"))?;
    let bytes = collected.to_bytes();
    serde_json::from_slice::<SsrResponse>(&bytes)
        .map_err(|e| format!("deserialize response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssr_disabled_when_config_disabled() {
        let cfg = SsrConfig::default();
        assert!(!cfg.enabled);
    }

    #[tokio::test]
    async fn render_returns_none_when_disabled() {
        let cfg = SsrConfig::default();
        let page = serde_json::json!({"component": "Home"});
        let result = render(&cfg, "/foo", &page).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn render_returns_none_when_path_excluded() {
        let cfg = SsrConfig {
            enabled: true,
            excluded_paths: vec!["/admin/**".to_string()],
            ..SsrConfig::default()
        };
        let page = serde_json::json!({"component": "Admin"});
        let result = render(&cfg, "/admin/users", &page).await.unwrap();
        assert!(result.is_none());
    }
}
