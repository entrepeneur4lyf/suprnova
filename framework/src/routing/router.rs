use crate::http::{Request, Response};
use crate::middleware::{into_boxed, BoxedMiddleware, Middleware};
use matchit::Router as MatchitRouter;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

/// Global registry mapping route names to path patterns
static ROUTE_REGISTRY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Register a route name -> path mapping
pub fn register_route_name(name: &str, path: &str) {
    let registry = ROUTE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()));
    if let Ok(mut map) = registry.write() {
        map.insert(name.to_string(), path.to_string());
    }
}

/// Generate a URL for a named route with parameters
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
/// ```
pub fn route(name: &str, params: &[(&str, &str)]) -> Option<String> {
    let registry = ROUTE_REGISTRY.get()?.read().ok()?;
    let path_pattern = registry.get(name)?;

    let mut url = path_pattern.clone();
    for (key, value) in params {
        url = url.replace(&format!("{{{}}}", key), value);
    }
    Some(url)
}

/// Generate URL with HashMap parameters (used internally by Redirect)
pub fn route_with_params(name: &str, params: &HashMap<String, String>) -> Option<String> {
    let registry = ROUTE_REGISTRY.get()?.read().ok()?;
    let path_pattern = registry.get(name)?;

    let mut url = path_pattern.clone();
    for (key, value) in params {
        url = url.replace(&format!("{{{}}}", key), value);
    }
    Some(url)
}

/// HTTP method for tracking the last registered route
#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
    Put,
    Delete,
}

/// Type alias for route handlers
pub type BoxedHandler =
    Box<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

/// HTTP Router with Laravel-like route registration
pub struct Router {
    get_routes: MatchitRouter<Arc<BoxedHandler>>,
    post_routes: MatchitRouter<Arc<BoxedHandler>>,
    put_routes: MatchitRouter<Arc<BoxedHandler>>,
    delete_routes: MatchitRouter<Arc<BoxedHandler>>,
    /// Middleware assignments: path -> boxed middleware instances
    route_middleware: HashMap<String, Vec<BoxedMiddleware>>,
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
            delete_routes: MatchitRouter::new(),
            route_middleware: HashMap::new(),
            fallback_handler: None,
            fallback_middleware: Vec::new(),
        }
    }

    /// Get middleware for a specific route path
    pub fn get_route_middleware(&self, path: &str) -> Vec<BoxedMiddleware> {
        self.route_middleware.get(path).cloned().unwrap_or_default()
    }

    /// Register middleware for a path (internal use)
    pub(crate) fn add_middleware(&mut self, path: &str, middleware: BoxedMiddleware) {
        self.route_middleware
            .entry(path.to_string())
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

    /// Insert a GET route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_get(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.get_routes.insert(path, handler).ok();
    }

    /// Insert a POST route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_post(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.post_routes.insert(path, handler).ok();
    }

    /// Insert a PUT route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_put(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.put_routes.insert(path, handler).ok();
    }

    /// Insert a DELETE route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_delete(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.delete_routes.insert(path, handler).ok();
    }

    /// Register a GET route
    pub fn get<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.get_routes.insert(path, Arc::new(handler)).ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            _last_method: Method::Get,
        }
    }

    /// Register a POST route
    pub fn post<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.post_routes.insert(path, Arc::new(handler)).ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            _last_method: Method::Post,
        }
    }

    /// Register a PUT route
    pub fn put<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.put_routes.insert(path, Arc::new(handler)).ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            _last_method: Method::Put,
        }
    }

    /// Register a DELETE route
    pub fn delete<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.delete_routes.insert(path, Arc::new(handler)).ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            _last_method: Method::Delete,
        }
    }

    /// Match a request and return the handler with extracted params
    pub fn match_route(
        &self,
        method: &hyper::Method,
        path: &str,
    ) -> Option<(Arc<BoxedHandler>, HashMap<String, String>)> {
        let router = match *method {
            hyper::Method::GET => &self.get_routes,
            hyper::Method::POST => &self.post_routes,
            hyper::Method::PUT => &self.put_routes,
            hyper::Method::DELETE => &self.delete_routes,
            _ => return None,
        };

        router.at(path).ok().map(|matched| {
            let params: HashMap<String, String> = matched
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            (matched.value.clone(), params)
        })
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
    #[allow(dead_code)]
    _last_method: Method,
}

impl RouteBuilder {
    /// Name the most recently registered route
    pub fn name(self, name: &str) -> Router {
        register_route_name(name, &self.last_path);
        self.router
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
        self.router
            .add_middleware(&self.last_path, into_boxed(middleware));
        self
    }

    /// Apply pre-boxed middleware to the most recently registered route
    /// (Used internally by route macros)
    pub fn middleware_boxed(mut self, middleware: BoxedMiddleware) -> RouteBuilder {
        self.router
            .route_middleware
            .entry(self.last_path.clone())
            .or_default()
            .push(middleware);
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
}

impl From<RouteBuilder> for Router {
    fn from(builder: RouteBuilder) -> Self {
        builder.router
    }
}
