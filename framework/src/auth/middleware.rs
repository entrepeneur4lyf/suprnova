//! Authentication middleware

use crate::Request;
use crate::http::{HttpResponse, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

use super::contract::Credentials;
use super::guard::Auth;

/// Authentication middleware
///
/// Protects routes that require authentication. Unauthenticated requests
/// are either redirected to a login page or receive a 401 response.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{AuthMiddleware, group, get};
///
/// // API routes - return 401 for unauthenticated
/// group!("/api")
///     .middleware(AuthMiddleware::new())
///     .routes([...]);
///
/// // Web routes - redirect to login
/// group!("/dashboard")
///     .middleware(AuthMiddleware::redirect_to("/login"))
///     .routes([...]);
/// ```
pub struct AuthMiddleware {
    /// Path to redirect to if not authenticated (None = return 401)
    redirect_to: Option<String>,
    /// Named guard to check (None = the sync session-backed default-guard
    /// fast path; `Some(name)` checks that guard via the `AuthManager`).
    guard: Option<String>,
}

impl AuthMiddleware {
    /// Create middleware that returns 401 Unauthorized if not authenticated
    ///
    /// Best for API routes.
    pub fn new() -> Self {
        Self {
            redirect_to: None,
            guard: None,
        }
    }

    /// Create middleware that redirects to a login page if not authenticated
    ///
    /// Best for web routes.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// AuthMiddleware::redirect_to("/login")
    /// ```
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
            guard: None,
        }
    }

    /// Check a named guard instead of the default. Chainable on `new()` /
    /// `redirect_to(...)`:
    ///
    /// ```rust,ignore
    /// AuthMiddleware::new().for_guard("api")                  // 401 if the api guard is a guest
    /// AuthMiddleware::redirect_to("/login").for_guard("web")  // otherwise redirect
    /// ```
    ///
    /// Note: a token guard (e.g. `for_guard("api")`) expects the bearer-token
    /// middleware to have run earlier in the chain to populate the request's
    /// auth id; without it the guard always reports unauthenticated.
    pub fn for_guard(mut self, name: impl Into<String>) -> Self {
        self.guard = Some(name.into());
        self
    }
}

impl Default for AuthMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let authenticated = match &self.guard {
            Some(name) => Auth::guard(name)?.check().await?,
            None => Auth::check(),
        };
        if authenticated {
            // User is authenticated, proceed
            return next(request).await;
        }

        // User is not authenticated
        match &self.redirect_to {
            Some(path) => {
                // For Inertia requests, return 409 with redirect location
                // This tells Inertia to do a full page visit to the login page
                if request.is_inertia() {
                    Err(HttpResponse::text("")
                        .status(409)
                        .header("X-Inertia-Location", path.clone()))
                } else {
                    // Regular redirect for non-Inertia requests
                    Err(HttpResponse::new()
                        .status(302)
                        .header("Location", path.clone()))
                }
            }
            None => {
                // Return 401 Unauthorized
                Err(HttpResponse::json(serde_json::json!({
                    "message": "Unauthenticated."
                }))
                .status(401))
            }
        }
    }
}

/// Guest middleware
///
/// Protects routes that should only be accessible to guests (non-authenticated users).
/// Useful for login and registration pages.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{GuestMiddleware, group, get};
///
/// group!("/")
///     .middleware(GuestMiddleware::redirect_to("/dashboard"))
///     .routes([
///         get!("/login", auth::show_login),
///         get!("/register", auth::show_register),
///     ]);
/// ```
pub struct GuestMiddleware {
    /// Path to redirect to if authenticated
    redirect_to: String,
    /// Named guard to check (None = the sync session-backed default-guard
    /// fast path; `Some(name)` checks that guard via the `AuthManager`).
    guard: Option<String>,
}

impl GuestMiddleware {
    /// Create middleware that redirects authenticated users
    ///
    /// # Arguments
    ///
    /// * `redirect_to` - Path to redirect authenticated users to
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: path.into(),
            guard: None,
        }
    }

    /// Alias for `redirect_to` with a default path
    pub fn new() -> Self {
        Self::redirect_to("/")
    }

    /// Check a named guard instead of the default (chainable on
    /// `redirect_to(...)` / `new()`).
    pub fn for_guard(mut self, name: impl Into<String>) -> Self {
        self.guard = Some(name.into());
        self
    }
}

impl Default for GuestMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for GuestMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let is_guest = match &self.guard {
            Some(name) => Auth::guard(name)?.guest().await?,
            None => Auth::guest(),
        };
        if is_guest {
            // User is a guest, proceed
            return next(request).await;
        }

        // User is authenticated, redirect them away
        if request.is_inertia() {
            // For Inertia requests, return 409 with redirect location
            Err(HttpResponse::text("")
                .status(409)
                .header("X-Inertia-Location", &self.redirect_to))
        } else {
            // Regular redirect for non-Inertia requests
            Err(HttpResponse::new()
                .status(302)
                .header("Location", &self.redirect_to))
        }
    }
}

/// HTTP Basic authentication middleware.
///
/// Authenticates requests from the `Authorization: Basic <base64(user:pass)>`
/// header against a guard — mirroring Laravel's `Auth::basic` / `onceBasic`.
/// The decoded username is matched against the `field` credential (default
/// `"email"`); the password is verified by the guard's provider.
///
/// Every failure mode — a missing or malformed header, or credentials that do
/// not resolve a user — returns `401 Unauthorized` with a
/// `WWW-Authenticate: Basic realm="..."` challenge so a browser/client can
/// prompt for (new) credentials.
///
/// ```rust,ignore
/// use suprnova::{BasicAuthMiddleware, group};
///
/// // Stateful — logs the user into the session on success (Auth::basic):
/// group!("/admin").middleware(BasicAuthMiddleware::new()).routes([...]);
///
/// // Stateless — authenticates for this request only (Auth::onceBasic):
/// group!("/api").middleware(BasicAuthMiddleware::once()).routes([...]);
/// ```
pub struct BasicAuthMiddleware {
    /// Credential field the decoded username is matched against (default `email`).
    field: String,
    /// Realm advertised in the `WWW-Authenticate` challenge.
    realm: String,
    /// Named guard to authenticate against (None = the default guard).
    guard: Option<String>,
    /// When true, authenticate for this request only (`once`); when false,
    /// persist to the session (`attempt`).
    stateless: bool,
}

impl BasicAuthMiddleware {
    fn build(stateless: bool) -> Self {
        Self {
            field: "email".to_string(),
            // Default realm: the app name when set, else a neutral fallback.
            realm: std::env::var("APP_NAME").unwrap_or_else(|_| "Restricted".to_string()),
            guard: None,
            stateless,
        }
    }

    /// Stateful HTTP Basic auth against the default guard — logs the user into
    /// the session on success. Mirrors Laravel's `Auth::basic()`.
    ///
    /// Persisting the login requires `SessionMiddleware` earlier in the chain
    /// (the session write is a no-op without it); [`once`](Self::once) has no
    /// such dependency.
    pub fn new() -> Self {
        Self::build(false)
    }

    /// Stateless HTTP Basic auth against the default guard — authenticates for
    /// the current request only (no session). Mirrors Laravel's `Auth::onceBasic()`.
    pub fn once() -> Self {
        Self::build(true)
    }

    /// Set the credential field the decoded username is matched against
    /// (default `"email"`).
    pub fn field(mut self, field: impl Into<String>) -> Self {
        self.field = field.into();
        self
    }

    /// Set the realm advertised in the `WWW-Authenticate` challenge (default:
    /// the `APP_NAME` env var, else `"Restricted"`).
    pub fn realm(mut self, realm: impl Into<String>) -> Self {
        self.realm = realm.into();
        self
    }

    /// Authenticate against a named guard instead of the default.
    pub fn for_guard(mut self, name: impl Into<String>) -> Self {
        self.guard = Some(name.into());
        self
    }

    /// Decode `Authorization: Basic <base64(user:password)>` into credentials
    /// keyed by `field` + `password`. `None` when the header is absent, not the
    /// `Basic` scheme, not valid base64/UTF-8, or missing the `:` separator.
    fn decode(&self, request: &Request) -> Option<Credentials> {
        use base64::Engine as _;

        let header = request.header("authorization")?;
        let (scheme, encoded) = header.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("Basic") {
            return None;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        let (user, password) = decoded.split_once(':')?;
        Some(
            Credentials::new()
                .insert(self.field.clone(), user.to_string())
                .insert("password", password.to_string()),
        )
    }

    /// The `401 Unauthorized` challenge response.
    fn challenge(&self) -> HttpResponse {
        HttpResponse::json(serde_json::json!({ "message": "Invalid credentials." }))
            .status(401)
            .header(
                "WWW-Authenticate",
                format!("Basic realm=\"{}\"", self.realm),
            )
    }
}

impl Default for BasicAuthMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for BasicAuthMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Stateful basic short-circuits on an already-authenticated session,
        // matching Laravel's `basic()` (which returns early when `check()`
        // passes). Stateless `once` always re-reads the header.
        if !self.stateless {
            let already = match &self.guard {
                Some(name) => Auth::guard(name)?.check().await?,
                None => Auth::check(),
            };
            if already {
                return next(request).await;
            }
        }

        let credentials = match self.decode(&request) {
            Some(c) => c,
            None => return Err(self.challenge()),
        };

        let authenticated = match (&self.guard, self.stateless) {
            (Some(name), true) => Auth::stateful_guard(name)?.once(&credentials).await?,
            (Some(name), false) => Auth::stateful_guard(name)?
                .attempt(&credentials, false)
                .await?
                .is_some(),
            (None, true) => Auth::once(&credentials).await?,
            (None, false) => Auth::attempt(&credentials, false).await?.is_some(),
        };

        if authenticated {
            next(request).await
        } else {
            Err(self.challenge())
        }
    }
}
