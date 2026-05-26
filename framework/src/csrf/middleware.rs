//! CSRF protection middleware

use crate::Request;
use crate::http::{HttpResponse, Response};
use crate::middleware::{Middleware, Next};
use crate::session::get_csrf_token;
use async_trait::async_trait;

/// Maximum bytes we will buffer from a form-urlencoded request body to
/// look for the `_token` field. A `_token` field is a 40-char hex
/// string; the rest of the form might contain reasonably-sized fields
/// (login form, contact form, etc.). 64 KiB is comfortable for those
/// cases and small enough that a malicious large form won't pin the
/// server's memory waiting on CSRF validation.
const CSRF_BODY_BUFFER_CAP: usize = 64 * 1024;

/// CSRF protection middleware
///
/// Validates CSRF tokens on state-changing requests (POST, PUT, PATCH, DELETE).
///
/// # Token Sources
///
/// The middleware looks for the CSRF token in the following order:
/// 1. `X-CSRF-TOKEN` header (used by Inertia.js)
/// 2. `X-XSRF-TOKEN` header (Laravel convention)
/// 3. `_token` form field (traditional forms)
///
/// # Usage
///
/// ```rust,ignore
/// use suprnova::{global_middleware, CsrfMiddleware};
///
/// global_middleware!(CsrfMiddleware::new());
/// ```
pub struct CsrfMiddleware {
    /// HTTP methods that require CSRF validation
    protected_methods: Vec<&'static str>,
    /// Paths to exclude from CSRF validation (e.g., webhooks)
    except: Vec<String>,
}

impl CsrfMiddleware {
    /// Create a new CSRF middleware with default settings
    ///
    /// Protects: POST, PUT, PATCH, DELETE
    pub fn new() -> Self {
        Self {
            protected_methods: vec!["POST", "PUT", "PATCH", "DELETE"],
            except: Vec::new(),
        }
    }

    /// Add paths to exclude from CSRF validation
    ///
    /// Useful for webhooks or API endpoints that use other authentication.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let csrf = CsrfMiddleware::new()
    ///     .except(vec!["/webhooks/*", "/api/external/*"]);
    /// ```
    pub fn except(mut self, paths: Vec<impl Into<String>>) -> Self {
        self.except = paths.into_iter().map(|p| p.into()).collect();
        self
    }

    /// Check if a path should be excluded from CSRF validation
    fn is_excluded(&self, path: &str) -> bool {
        for pattern in &self.except {
            if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len() - 1];
                if path.starts_with(prefix) {
                    return true;
                }
            } else if pattern == path {
                return true;
            }
        }
        false
    }
}

impl Default for CsrfMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for CsrfMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let method = request.method().as_str();

        // Only validate state-changing requests
        if !self.protected_methods.contains(&method) {
            return next(request).await;
        }

        // Check if path is excluded
        if self.is_excluded(request.path()) {
            return next(request).await;
        }

        // Get expected token from session
        let expected_token = match get_csrf_token() {
            Some(token) => token,
            None => {
                return Err(HttpResponse::json(serde_json::json!({
                    "message": "Session not found. CSRF validation failed."
                }))
                .status(500));
            }
        };

        // Header tokens (AJAX / Inertia / framework conventions) are
        // always checked first — they don't require body buffering.
        if let Some(token) = request
            .header("X-CSRF-TOKEN")
            .or_else(|| request.header("X-XSRF-TOKEN"))
        {
            if constant_time_compare(token, &expected_token) {
                return next(request).await;
            }
            // Header was present but wrong — reject without parsing the
            // body. A correct client picks one location for the token;
            // we don't combine header + body to avoid token-splitting
            // surprises.
            return reject_with_419();
        }

        // No header — for `application/x-www-form-urlencoded` bodies
        // we honor the documented `_token` field (the value emitted by
        // `csrf_field()` in HTML forms). We buffer the body so the
        // downstream handler can still read its form data.
        let is_form_body = request
            .content_type()
            .map(|ct| ct.starts_with("application/x-www-form-urlencoded"))
            .unwrap_or(false);

        if !is_form_body {
            return reject_with_419();
        }

        // Buffer the body. CSRF_BODY_BUFFER_CAP caps this at 64 KiB —
        // forms with `_token` are well under that, and a malicious large
        // form won't pin memory on CSRF validation alone.
        let request = match request.buffer_body(CSRF_BODY_BUFFER_CAP).await {
            Ok(r) => r,
            Err(_) => return reject_with_419(),
        };

        let Some(body) = request.cached_body() else {
            return reject_with_419();
        };

        // Parse `_token=...` out of the form bag. `form_urlencoded::parse`
        // URL-decodes values; the token is hex so decoding is a no-op,
        // but using the parser keeps us consistent with how `req.form()`
        // would later see the same body.
        let token_field = url::form_urlencoded::parse(body).find_map(|(k, v)| {
            if k == "_token" {
                Some(v.into_owned())
            } else {
                None
            }
        });

        match token_field {
            Some(token) if constant_time_compare(&token, &expected_token) => next(request).await,
            _ => reject_with_419(),
        }
    }
}

fn reject_with_419() -> Response {
    Err(HttpResponse::json(serde_json::json!({
        "message": "CSRF token mismatch."
    }))
    .status(419))
}

/// Constant-time string comparison to prevent timing attacks
///
/// This ensures an attacker can't determine how much of the token is correct
/// based on response time.
fn constant_time_compare(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.bytes()
        .zip(b.bytes())
        .fold(0, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_compare() {
        assert!(constant_time_compare("abc123", "abc123"));
        assert!(!constant_time_compare("abc123", "abc124"));
        assert!(!constant_time_compare("abc123", "abc12"));
        assert!(!constant_time_compare("", "a"));
    }

    #[test]
    fn test_is_excluded() {
        let csrf = CsrfMiddleware::new().except(vec!["/webhooks/*", "/api/public"]);

        assert!(csrf.is_excluded("/webhooks/stripe"));
        assert!(csrf.is_excluded("/webhooks/github/events"));
        assert!(csrf.is_excluded("/api/public"));
        assert!(!csrf.is_excluded("/api/private"));
        assert!(!csrf.is_excluded("/login"));
    }

    // ----------------------------------------------------------------
    // Regression: HIGH audit finding `csrf` #335 — documented `_token`
    // form-field validation was not implemented; only headers were read.
    //
    // These tests install a fake session in `SESSION_CONTEXT`, drive a
    // real `Request` through `CsrfMiddleware::handle`, and verify:
    //   (a) a matching `_token` in a form-urlencoded body passes
    //   (b) a wrong `_token` rejects with 419
    //   (c) downstream handler still sees the full form body after the
    //       middleware buffered it (the load-bearing piece — without
    //       this, the fix moved the bug instead of solving it)
    // ----------------------------------------------------------------

    use crate::Request;
    use crate::session::middleware::SESSION_CONTEXT;
    use crate::session::store::SessionData;
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    /// Spawn a one-shot hyper server that scopes a session containing
    /// `expected_token`, runs `CsrfMiddleware` around a handler that
    /// records what form fields it saw, and returns the response.
    /// Returns (response status, fields the downstream handler observed).
    async fn drive_form_post(
        expected_token: &str,
        form_body: String,
    ) -> (u16, std::collections::HashMap<String, String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();

        let captured: Arc<Mutex<std::collections::HashMap<String, String>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let server_captured = captured.clone();

        let expected_token = expected_token.to_string();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            let server_captured = server_captured.clone();
            let expected_token = expected_token.clone();
            let service = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
                let server_captured = server_captured.clone();
                let expected_token = expected_token.clone();
                async move {
                    // Install a session with the expected CSRF token.
                    let session = SessionData {
                        csrf_token: expected_token,
                        ..Default::default()
                    };
                    let slot = Arc::new(Mutex::new(Some(session)));

                    let response = SESSION_CONTEXT
                        .scope(slot, async move {
                            let req = Request::new(hyper_req);
                            let mw = Arc::new(CsrfMiddleware::new());
                            let next: Next = Arc::new(move |req| {
                                let server_captured = server_captured.clone();
                                Box::pin(async move {
                                    // The handler reads the form body — this
                                    // proves the CSRF middleware's body
                                    // buffering keeps the body readable for
                                    // downstream consumers.
                                    let (_, bytes) = req.body_bytes().await?;
                                    let mut map = server_captured.lock().unwrap();
                                    for (k, v) in url::form_urlencoded::parse(&bytes).into_owned() {
                                        map.insert(k, v);
                                    }
                                    Ok(HttpResponse::text("ok"))
                                })
                            });
                            mw.handle(req, next).await
                        })
                        .await;

                    let http = response.unwrap_or_else(|e| e);
                    Ok::<_, Infallible>(http.into_hyper())
                }
            });
            let _ = http1::Builder::new().serve_connection(io, service).await;
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let req = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("content-length", form_body.len().to_string())
            .body(Full::new(Bytes::from(form_body)))
            .unwrap();

        let resp = sender.send_request(req).await.unwrap();
        let status = resp.status().as_u16();
        let (_parts, body) = resp.into_parts();
        let _ = body.collect().await.unwrap();
        let fields = captured.lock().unwrap().clone();
        (status, fields)
    }

    #[tokio::test]
    async fn form_post_with_matching_token_in_body_passes_and_handler_sees_body() {
        // The load-bearing regression test: a real HTTP POST with
        // _token in the body must (a) pass CSRF validation and (b)
        // leave the form body intact for the downstream handler.
        let token = "matching-token-fixture-1234567890";
        let body = format!("_token={token}&username=alice&password=hunter2");

        let (status, fields) = drive_form_post(token, body).await;

        assert_eq!(
            status, 200,
            "form POST with matching _token must pass CSRF (no 419)"
        );
        assert_eq!(
            fields.get("username").map(|s| s.as_str()),
            Some("alice"),
            "downstream handler must still see the form body after CSRF \
             buffered it — without this, we moved the bug instead of fixing it"
        );
        assert_eq!(fields.get("password").map(|s| s.as_str()), Some("hunter2"));
        assert_eq!(
            fields.get("_token").map(|s| s.as_str()),
            Some(token),
            "the _token field stays in the form bag for the handler — \
             CSRF doesn't strip it"
        );
    }

    #[tokio::test]
    async fn form_post_with_wrong_token_in_body_rejects_with_419() {
        let session_token = "real-session-token-xyz";
        let body = "_token=wrong-attacker-token&action=transfer".to_string();

        let (status, _fields) = drive_form_post(session_token, body).await;

        assert_eq!(
            status, 419,
            "form POST with mismatched _token must reject with 419"
        );
    }

    #[tokio::test]
    async fn form_post_with_no_token_at_all_rejects_with_419() {
        let session_token = "real-session-token-xyz";
        let body = "action=transfer&amount=100".to_string();

        let (status, _fields) = drive_form_post(session_token, body).await;

        assert_eq!(
            status, 419,
            "form POST with no _token (and no header) must reject with 419"
        );
    }

    // Silence unused-import warnings when only some of these are used.
    #[allow(dead_code)]
    fn _unused_imports_keep() {
        let _ = Empty::<Bytes>::new();
    }
}
