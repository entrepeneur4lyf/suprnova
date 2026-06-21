//! Resource routing — Laravel `Route::resource(...)` parity.
//!
//! Laravel's `ResourceRegistrar` (`Illuminate/Routing/ResourceRegistrar.php`)
//! generates the standard 7-action REST surface from a controller class
//! name + a base path. Suprnova ships a Rust-shaped equivalent:
//! consumers implement the [`ResourceController`] trait and pass an
//! instance through [`Router::resource`] / [`Router::api_resource`] to
//! generate the same 7 (or 4, for API) routes plus their conventional
//! route names.
//!
//! ## Default routes
//!
//! For a resource named `posts` at path `/posts`:
//!
//! | Verb   | Path                    | Trait method     | Name           |
//! |--------|-------------------------|------------------|----------------|
//! | GET    | `/posts`                | `index`          | `posts.index`  |
//! | GET    | `/posts/create`         | `create`         | `posts.create` |
//! | POST   | `/posts`                | `store`          | `posts.store`  |
//! | GET    | `/posts/{post}`         | `show`           | `posts.show`   |
//! | GET    | `/posts/{post}/edit`    | `edit`           | `posts.edit`   |
//! | PUT    | `/posts/{post}`         | `update`         | `posts.update` |
//! | PATCH  | `/posts/{post}`         | `update`         | _shares update_|
//! | DELETE | `/posts/{post}`         | `destroy`        | `posts.destroy`|
//!
//! `api_resource` drops `create` and `edit` (the form-rendering routes
//! that an API doesn't need), matching Laravel's
//! `ResourceRegistrar::apiResourceDefaults`.
//!
//! ## Customizing the surface
//!
//! [`ResourceRoutes`] (returned by [`Router::resource`]) supports the
//! Laravel-shaped chain:
//!
//! ```rust,ignore
//! Router::new().resource("posts", PostsCtl)
//!     .only(&[ResourceAction::Index, ResourceAction::Show])
//!     .names(&[("index", "posts.list")])
//!     .parameters(&[("posts", "post_id")])
//! ```
//!
//! - [`ResourceRoutes::only`] restricts the generated set to a list.
//! - [`ResourceRoutes::except`] excludes a list from the default set.
//! - [`ResourceRoutes::names`] overrides route names per action.
//! - [`ResourceRoutes::parameters`] renames the path parameter
//!   (Laravel's `parameters(['users' => 'user_id'])`).
//!
//! ## Dual-API
//!
//! - `only` (Laravel) + `keep` (Rust) — both alias.
//! - `except` (Laravel) + `drop` (Rust) — both alias.
//! - `names` (Laravel) + `rename` (Rust) — both alias.

use super::router::{BoxedHandler, Router};
use crate::FrameworkError;
use crate::auth::{Auth, Authenticatable};
use crate::authorization::Gate;
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{BoxedMiddleware, Middleware, Next, into_boxed};
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// One action in the standard REST resource surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceAction {
    /// `GET /<base>` — list resources.
    Index,
    /// `GET /<base>/create` — show a creation form (web-only).
    Create,
    /// `POST /<base>` — persist a new resource.
    Store,
    /// `GET /<base>/{id}` — show a single resource.
    Show,
    /// `GET /<base>/{id}/edit` — show an edit form (web-only).
    Edit,
    /// `PUT|PATCH /<base>/{id}` — update.
    Update,
    /// `DELETE /<base>/{id}` — destroy.
    Destroy,
}

impl ResourceAction {
    /// Stable string name used for `.names()` overrides and `parameters()`
    /// lookups. Matches Laravel's lowercase action keys.
    pub fn key(self) -> &'static str {
        match self {
            ResourceAction::Index => "index",
            ResourceAction::Create => "create",
            ResourceAction::Store => "store",
            ResourceAction::Show => "show",
            ResourceAction::Edit => "edit",
            ResourceAction::Update => "update",
            ResourceAction::Destroy => "destroy",
        }
    }

    /// The seven web-resource defaults in canonical order. Mirrors
    /// `ResourceRegistrar::$resourceDefaults`.
    pub fn web_defaults() -> &'static [ResourceAction] {
        &[
            ResourceAction::Index,
            ResourceAction::Create,
            ResourceAction::Store,
            ResourceAction::Show,
            ResourceAction::Edit,
            ResourceAction::Update,
            ResourceAction::Destroy,
        ]
    }

    /// The five API-resource defaults (drops `create` and `edit`).
    /// Mirrors `ResourceRegistrar::apiResourceDefaults`.
    pub fn api_defaults() -> &'static [ResourceAction] {
        &[
            ResourceAction::Index,
            ResourceAction::Store,
            ResourceAction::Show,
            ResourceAction::Update,
            ResourceAction::Destroy,
        ]
    }

    fn from_key(s: &str) -> Option<Self> {
        Some(match s {
            "index" => ResourceAction::Index,
            "create" => ResourceAction::Create,
            "store" => ResourceAction::Store,
            "show" => ResourceAction::Show,
            "edit" => ResourceAction::Edit,
            "update" => ResourceAction::Update,
            "destroy" => ResourceAction::Destroy,
            _ => return None,
        })
    }
}

/// Type alias for an async resource handler.
type ResourceHandlerFn =
    dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync;

/// Factory that produces the authorization middleware for one resource
/// action. Built by [`ResourceRoutes::authorize_resource`], invoked once
/// per generated route at registration time. Returns `None` for actions
/// that have no authorization mapping (none currently — every standard
/// action maps to an ability).
type AuthorizeFactory = Box<dyn Fn(ResourceAction) -> Option<BoxedMiddleware> + Send + Sync>;

/// Map a resource action to the Gate ability it authorizes against,
/// matching Laravel's `authorizeResource` table:
///
/// | Action  | Ability |
/// |---------|---------|
/// | index   | view    |
/// | show    | view    |
/// | create  | create  |
/// | store   | create  |
/// | edit    | update  |
/// | update  | update  |
/// | destroy | delete  |
fn ability_for(action: ResourceAction) -> &'static str {
    match action {
        ResourceAction::Index | ResourceAction::Show => "view",
        ResourceAction::Create | ResourceAction::Store => "create",
        ResourceAction::Edit | ResourceAction::Update => "update",
        ResourceAction::Destroy => "delete",
    }
}

/// Per-route authorization middleware generated by
/// [`ResourceRoutes::authorize_resource`].
///
/// Resolves the authenticated user as the concrete type `U` and runs the
/// resource action's mapped ability through the [`Gate`] against a default
/// instance of the resource marker `R`. A denial — or an unauthenticated
/// request — short-circuits the chain before the resource handler runs,
/// closing the "forgotten `Gate::authorize` in the handler body" gap.
///
/// The resource marker is a [`Default`] value of `R` (Laravel passes the
/// model class for the class-level abilities and a route-bound instance for
/// instance abilities; Suprnova's type-keyed gate discriminates on the
/// `R` *type*, so a default marker carries the same routing information for
/// the policy that the type does).
struct ResourceAuthorizeMiddleware<U, R> {
    ability: &'static str,
    _user: std::marker::PhantomData<fn() -> U>,
    _resource: std::marker::PhantomData<fn() -> R>,
}

#[async_trait]
impl<U, R> Middleware for ResourceAuthorizeMiddleware<U, R>
where
    U: Authenticatable + Clone + 'static,
    R: Default + Send + Sync + 'static,
{
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Fail closed: no authenticated user (or the user is not a `U`) is a
        // denial, never a pass-through.
        let user = match Auth::user_as::<U>().await {
            Ok(Some(user)) => user,
            Ok(None) => return Err(HttpResponse::from(FrameworkError::Unauthorized)),
            Err(err) => return Err(HttpResponse::from(err)),
        };
        let resource = R::default();
        match Gate::authorize::<U, R>(self.ability, &user, &resource) {
            Ok(()) => next(request).await,
            Err(err) => Err(HttpResponse::from(err)),
        }
    }
}

/// REST resource controller. Implement on a unit struct (or anything
/// `Send + Sync + 'static`) and pass to [`Router::resource`] /
/// [`Router::api_resource`].
///
/// All seven methods have a default implementation that returns
/// `404 Not Found`. Override the ones your resource supports;
/// `only`/`except` on the generated [`ResourceRoutes`] determine
/// which routes are actually registered.
///
/// Methods take a [`Request`] and return a [`Response`]. They run inside
/// the framework's normal middleware / handler pipeline — there is no
/// magic dependency injection; pull form data via `request.form_data()`
/// etc. just as you would in a function handler.
pub trait ResourceController: Send + Sync + 'static {
    /// `GET /<base>` — list resources.
    fn index(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let _ = request;
        Box::pin(async { not_implemented("index") })
    }

    /// `GET /<base>/create` — show a creation form.
    fn create(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let _ = request;
        Box::pin(async { not_implemented("create") })
    }

    /// `POST /<base>` — store a new resource.
    fn store(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let _ = request;
        Box::pin(async { not_implemented("store") })
    }

    /// `GET /<base>/{id}` — show a single resource.
    fn show(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let _ = request;
        Box::pin(async { not_implemented("show") })
    }

    /// `GET /<base>/{id}/edit` — show an edit form.
    fn edit(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let _ = request;
        Box::pin(async { not_implemented("edit") })
    }

    /// `PUT|PATCH /<base>/{id}` — update an existing resource.
    fn update(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let _ = request;
        Box::pin(async { not_implemented("update") })
    }

    /// `DELETE /<base>/{id}` — destroy a resource.
    fn destroy(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let _ = request;
        Box::pin(async { not_implemented("destroy") })
    }
}

fn not_implemented(action: &str) -> Response {
    Err(
        crate::http::HttpResponse::text(format!("Resource action '{action}' not implemented"))
            .status(404),
    )
}

/// Pending resource registration. Returned by [`Router::resource`] /
/// [`Router::api_resource`]; absorbs Laravel-shaped chains
/// (`only`/`except`/`names`/`parameters`) before finalizing into a
/// [`Router`].
///
/// The router consumes the builder either via the conversion
/// `Router::from(routes)` / `routes.into()` or explicitly via
/// [`ResourceRoutes::register`]. Both paths panic on collision with an
/// existing route, mirroring [`Router::get`]'s boot-time-fail-loud
/// policy. Use [`ResourceRoutes::try_register`] for a fallible variant.
pub struct ResourceRoutes {
    router: Router,
    name: String,
    controller: Arc<dyn ResourceController>,
    actions: Vec<ResourceAction>,
    name_overrides: std::collections::HashMap<String, String>,
    parameter: Option<String>,
    /// When `true`, route names are not registered. Used by
    /// nested-or-skip flows that want raw paths without polluting the
    /// process-global registry.
    suppress_names: bool,
    /// When set by [`ResourceRoutes::authorize_resource`], produces the
    /// per-action authorization middleware attached to each generated route.
    authorize: Option<AuthorizeFactory>,
}

impl ResourceRoutes {
    fn new(
        router: Router,
        name: &str,
        controller: Arc<dyn ResourceController>,
        defaults: &[ResourceAction],
    ) -> Self {
        Self {
            router,
            name: name.to_string(),
            controller,
            actions: defaults.to_vec(),
            name_overrides: std::collections::HashMap::new(),
            parameter: None,
            suppress_names: false,
            authorize: None,
        }
    }

    /// Restrict the generated routes to `actions`. Mirrors Laravel's
    /// `only(['index', 'show'])`. Duplicates are de-duplicated.
    pub fn only(mut self, actions: &[ResourceAction]) -> Self {
        let allowed: std::collections::HashSet<ResourceAction> = actions.iter().copied().collect();
        self.actions.retain(|a| allowed.contains(a));
        self
    }

    /// Rust-side alias of [`Self::only`]. Same behaviour; lets call sites
    /// pick the idiom that reads better in context.
    pub fn keep(self, actions: &[ResourceAction]) -> Self {
        self.only(actions)
    }

    /// Remove the listed actions from the generated set. Mirrors
    /// Laravel's `except(['destroy'])`.
    pub fn except(mut self, actions: &[ResourceAction]) -> Self {
        let blocked: std::collections::HashSet<ResourceAction> = actions.iter().copied().collect();
        self.actions.retain(|a| !blocked.contains(a));
        self
    }

    /// Rust-side alias of [`Self::except`].
    pub fn drop(self, actions: &[ResourceAction]) -> Self {
        self.except(actions)
    }

    /// Override route names per action. Pairs are
    /// `(action_key, new_name)`, e.g. `("index", "posts.list")`.
    /// Mirrors Laravel's `names(['index' => 'posts.list'])`.
    ///
    /// Unknown action keys are silently ignored (matching Laravel's
    /// permissiveness — typos surface as the default name still being
    /// registered).
    pub fn names<'a, I>(mut self, overrides: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        for (key, name) in overrides {
            if ResourceAction::from_key(key).is_some() {
                self.name_overrides
                    .insert(key.to_string(), name.to_string());
            }
        }
        self
    }

    /// Rust-side alias of [`Self::names`].
    pub fn rename<'a, I>(self, overrides: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        self.names(overrides)
    }

    /// Override the path-parameter name for `show`/`update`/`destroy`/
    /// `edit` from the default (the singular of the resource name —
    /// e.g. `posts` → `{post}`) to a custom value. Mirrors a single-pair
    /// `parameters(['posts' => 'post_id'])` call.
    pub fn parameter(mut self, name: &str) -> Self {
        self.parameter = Some(name.to_string());
        self
    }

    /// Suppress route-name registration entirely. Useful for nested
    /// resources where the parent already owns the namespace, or for
    /// tests that don't want the process-global registry touched.
    /// No Laravel analogue (Laravel has no opt-out flag); Rust-side
    /// convenience.
    pub fn unnamed(mut self) -> Self {
        self.suppress_names = true;
        self
    }

    /// Gate every generated resource route behind its conventional ability,
    /// matching Laravel's `authorizeResource`.
    ///
    /// Without this, each generated `index`/`show`/`store`/`update`/`destroy`
    /// route is ungated unless the controller body remembers to call
    /// [`Gate::authorize`] itself — and a single forgotten `destroy` ships an
    /// ungated delete. `authorize_resource` closes that gap by attaching an
    /// authorization middleware to every route, mapping each action to its
    /// ability:
    ///
    /// | Action          | Ability |
    /// |-----------------|---------|
    /// | index / show    | `view`  |
    /// | create / store  | `create`|
    /// | edit / update   | `update`|
    /// | destroy         | `delete`|
    ///
    /// The middleware resolves the authenticated user as `U` and runs the
    /// mapped ability through the [`Gate`] against a [`Default`] value of the
    /// resource marker `R` (the gate discriminates on the `R` *type*, so the
    /// marker carries the same routing information for the policy that a model
    /// class would in Laravel). An unauthenticated request, a user that is not
    /// a `U`, or a denied ability short-circuits the chain with `403` (or the
    /// gate's custom status) **before** the resource handler runs — fail-closed.
    ///
    /// Define the abilities with [`Gate::define`] /
    /// [`Gate::define_with`] (or a `#[policy]`) keyed on `(ability, U, R)`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Gate::define::<User, Post>("view",   |u, _p| u.is_member);
    /// Gate::define::<User, Post>("create", |u, _p| u.is_author);
    /// Gate::define::<User, Post>("update", |u, _p| u.is_author);
    /// Gate::define::<User, Post>("delete", |u, _p| u.is_admin);
    ///
    /// let router: Router = Router::new()
    ///     .resource("posts", PostsCtl)
    ///     .authorize_resource::<User, Post>()
    ///     .into();
    /// ```
    pub fn authorize_resource<U, R>(mut self) -> Self
    where
        U: Authenticatable + Clone + 'static,
        R: Default + Send + Sync + 'static,
    {
        self.authorize = Some(Box::new(|action: ResourceAction| {
            let mw = ResourceAuthorizeMiddleware::<U, R> {
                ability: ability_for(action),
                _user: std::marker::PhantomData,
                _resource: std::marker::PhantomData,
            };
            Some(into_boxed(mw))
        }));
        self
    }

    /// Finalize the resource registration into a [`Router`].
    ///
    /// # Panics
    ///
    /// Panics on a duplicate route registration (matching
    /// [`Router::get`]'s boot-time-fail-loud policy) or a duplicate
    /// route name. Use [`Self::try_register`] for a fallible variant.
    pub fn register(self) -> Router {
        self.try_register().unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Self::register`]. Returns
    /// `Err(FrameworkError)` on duplicate registration; otherwise
    /// identical.
    pub fn try_register(self) -> Result<Router, FrameworkError> {
        let Self {
            mut router,
            name,
            controller,
            actions,
            name_overrides,
            parameter,
            suppress_names,
            authorize,
        } = self;

        let base = if name.starts_with('/') {
            name.clone()
        } else {
            format!("/{name}")
        };
        // Default param name is the resource name itself, sans trailing 's'
        // if present — keeps single-segment paths Laravel-shaped
        // (`posts` → `{post}`).
        let param = parameter.unwrap_or_else(|| default_param_name(&name));

        // Captured before the loop consumes `actions` so the post-loop
        // PATCH-alongside-PUT registration knows whether Update was in
        // the set without re-walking.
        let had_update = actions.contains(&ResourceAction::Update);

        for action in actions {
            let (method, path, default_name) = resource_route(&base, &param, action);
            let handler = make_handler(controller.clone(), action);
            match method {
                hyper::Method::GET => router.try_insert_get(&path, handler)?,
                hyper::Method::POST => router.try_insert_post(&path, handler)?,
                hyper::Method::PUT => router.try_insert_put(&path, handler)?,
                hyper::Method::PATCH => router.try_insert_patch(&path, handler)?,
                hyper::Method::DELETE => router.try_insert_delete(&path, handler)?,
                ref m => {
                    return Err(FrameworkError::internal(format!(
                        "ResourceRoutes: unexpected method '{m}' for action '{}'",
                        action.key()
                    )));
                }
            }

            if !suppress_names {
                let effective_name = name_overrides
                    .get(action.key())
                    .cloned()
                    .unwrap_or_else(|| default_name.clone());
                super::router::try_register_route_name(&effective_name, &path)?;
            }

            // Attach the per-action authorization middleware (if
            // `authorize_resource` was called) keyed by the matched pattern,
            // the same key the dispatcher recovers via `match_route`.
            if let Some(factory) = authorize.as_ref()
                && let Some(mw) = factory(action)
            {
                router.add_middleware(method, &path, mw);
            }
        }

        // PUT and PATCH share the update action — Laravel registers both
        // by default. The action-loop above registers PUT (the verb
        // returned by `resource_route(Update)`); layer a parallel PATCH
        // entry on the same path against the same handler so callers
        // can use either verb. The route NAME has already been claimed
        // by the PUT registration above — re-registering it would
        // conflict, so we only insert the PATCH verb here. The PATCH verb
        // gets the same authorization middleware as the PUT verb so neither
        // verb is an ungated bypass of the other.
        if had_update {
            let (_method, path, _name) = resource_route(&base, &param, ResourceAction::Update);
            let handler = make_handler(controller.clone(), ResourceAction::Update);
            router.try_insert_patch(&path, handler)?;
            if let Some(factory) = authorize.as_ref()
                && let Some(mw) = factory(ResourceAction::Update)
            {
                router.add_middleware(hyper::Method::PATCH, &path, mw);
            }
        }
        Ok(router)
    }
}

impl From<ResourceRoutes> for Router {
    fn from(routes: ResourceRoutes) -> Self {
        routes.register()
    }
}

/// Build the `(method, path, default_name)` tuple for a single resource
/// action. Default names follow the Laravel convention
/// `<resource>.<action>`.
fn resource_route(
    base: &str,
    param: &str,
    action: ResourceAction,
) -> (hyper::Method, String, String) {
    let resource_name = base.trim_start_matches('/').replace('/', ".");
    match action {
        ResourceAction::Index => (
            hyper::Method::GET,
            base.to_string(),
            format!("{resource_name}.index"),
        ),
        ResourceAction::Create => (
            hyper::Method::GET,
            format!("{base}/create"),
            format!("{resource_name}.create"),
        ),
        ResourceAction::Store => (
            hyper::Method::POST,
            base.to_string(),
            format!("{resource_name}.store"),
        ),
        ResourceAction::Show => (
            hyper::Method::GET,
            format!("{base}/{{{param}}}"),
            format!("{resource_name}.show"),
        ),
        ResourceAction::Edit => (
            hyper::Method::GET,
            format!("{base}/{{{param}}}/edit"),
            format!("{resource_name}.edit"),
        ),
        ResourceAction::Update => (
            hyper::Method::PUT,
            format!("{base}/{{{param}}}"),
            format!("{resource_name}.update"),
        ),
        ResourceAction::Destroy => (
            hyper::Method::DELETE,
            format!("{base}/{{{param}}}"),
            format!("{resource_name}.destroy"),
        ),
    }
}

/// Build a [`BoxedHandler`] that dispatches into one of the trait
/// methods. Each handler closes over the shared `Arc<controller>` so
/// the same instance services every action; concurrent handlers see
/// the same `&self` (the trait requires `Send + Sync`).
fn make_handler(
    controller: Arc<dyn ResourceController>,
    action: ResourceAction,
) -> Arc<BoxedHandler> {
    let inner: BoxedHandler = Box::new(move |req: Request| {
        let c = controller.clone();
        let fut: Pin<Box<dyn Future<Output = Response> + Send>> = match action {
            ResourceAction::Index => c.index(req),
            ResourceAction::Create => c.create(req),
            ResourceAction::Store => c.store(req),
            ResourceAction::Show => c.show(req),
            ResourceAction::Edit => c.edit(req),
            ResourceAction::Update => c.update(req),
            ResourceAction::Destroy => c.destroy(req),
        };
        fut
    });
    // Silence unused: the type alias is here for future call sites
    // that want a concrete `Arc<ResourceHandlerFn>` to share across
    // verbs without rebuilding.
    let _: Arc<ResourceHandlerFn> = Arc::new(|_req| Box::pin(async { not_implemented("") }));
    Arc::new(inner)
}

/// Pluralise → singular for the default path parameter name.
/// "posts" → "post", "categories" → "category", "people" → "people"
/// (no rule covers irregular plurals — pass `parameter("person_id")`
/// at the call site for those).
fn default_param_name(resource: &str) -> String {
    // Take the last path segment (we may have called with `admin/posts`).
    let last = resource.rsplit('/').next().unwrap_or(resource);
    if let Some(stripped) = last.strip_suffix("ies") {
        // categories -> category
        format!("{stripped}y")
    } else if let Some(stripped) = last.strip_suffix('s') {
        // posts -> post
        stripped.to_string()
    } else {
        last.to_string()
    }
}

impl Router {
    /// Register a standard 7-action REST resource at `path`.
    ///
    /// `controller` must implement [`ResourceController`]. Generates
    /// `index`/`create`/`store`/`show`/`edit`/`update`/`destroy`
    /// with conventional route names (`<resource>.<action>`). Use the
    /// returned [`ResourceRoutes`] to restrict / rename / re-parameterise.
    ///
    /// Mirrors Laravel's `Route::resource($name, $controller)` from
    /// `Illuminate/Routing/Router.php:347`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// struct PostsCtl;
    /// impl ResourceController for PostsCtl {
    ///     fn index(&self, _req: Request) -> _ { Box::pin(async { Ok(text("...")) }) }
    ///     fn show(&self, _req: Request) -> _ { Box::pin(async { Ok(text("...")) }) }
    /// }
    ///
    /// let router: Router = Router::new()
    ///     .resource("posts", PostsCtl)
    ///     .only(&[ResourceAction::Index, ResourceAction::Show])
    ///     .into();
    /// ```
    pub fn resource<C: ResourceController>(self, name: &str, controller: C) -> ResourceRoutes {
        ResourceRoutes::new(
            self,
            name,
            Arc::new(controller),
            ResourceAction::web_defaults(),
        )
    }

    /// API-flavoured resource registration. Drops `create` and `edit`
    /// (form-rendering routes that an API doesn't need), matching
    /// Laravel's `Route::apiResource($name, $controller)`.
    pub fn api_resource<C: ResourceController>(self, name: &str, controller: C) -> ResourceRoutes {
        ResourceRoutes::new(
            self,
            name,
            Arc::new(controller),
            ResourceAction::api_defaults(),
        )
    }

    /// Bulk-register multiple resources from `(name, controller)` pairs.
    /// Mirrors `Route::resources(['posts' => PostsCtl::class, ...])`.
    ///
    /// All resources are registered with default actions. Use individual
    /// `resource()` calls when per-resource customisation is needed.
    pub fn resources<I>(mut self, resources: I) -> Self
    where
        I: IntoIterator<Item = (&'static str, Box<dyn ResourceController>)>,
    {
        for (name, ctl) in resources {
            self = ResourceRoutes {
                router: self,
                name: name.to_string(),
                controller: Arc::from(ctl),
                actions: ResourceAction::web_defaults().to_vec(),
                name_overrides: Default::default(),
                parameter: None,
                suppress_names: false,
                authorize: None,
            }
            .register();
        }
        self
    }

    /// Bulk API variant of [`Self::resources`]. Mirrors
    /// `Route::apiResources(...)`.
    pub fn api_resources<I>(mut self, resources: I) -> Self
    where
        I: IntoIterator<Item = (&'static str, Box<dyn ResourceController>)>,
    {
        for (name, ctl) in resources {
            self = ResourceRoutes {
                router: self,
                name: name.to_string(),
                controller: Arc::from(ctl),
                actions: ResourceAction::api_defaults().to_vec(),
                name_overrides: Default::default(),
                parameter: None,
                suppress_names: false,
                authorize: None,
            }
            .register();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Request, Response, text};
    use hyper::Method;

    struct Ctl;
    impl ResourceController for Ctl {
        fn index(&self, _req: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
            Box::pin(async { text("index") })
        }
        fn show(&self, _req: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
            Box::pin(async { text("show") })
        }
    }

    #[test]
    fn web_resource_registers_seven_routes() {
        let router: Router = Router::new().resource("posts", Ctl).unnamed().into();
        assert!(router.match_route(&Method::GET, "/posts").is_some());
        assert!(router.match_route(&Method::GET, "/posts/create").is_some());
        assert!(router.match_route(&Method::POST, "/posts").is_some());
        assert!(router.match_route(&Method::GET, "/posts/42").is_some());
        assert!(router.match_route(&Method::GET, "/posts/42/edit").is_some());
        assert!(router.match_route(&Method::PUT, "/posts/42").is_some());
        assert!(router.match_route(&Method::DELETE, "/posts/42").is_some());
    }

    #[test]
    fn update_action_registers_both_put_and_patch() {
        // Laravel registers PUT and PATCH for the update action; both
        // verbs must route to the same handler. The action-loop
        // dispatches PUT directly; the PATCH-alongside step layers a
        // parallel entry on the same path. Without it, a client sending
        // `PATCH /posts/42` would 404 even though the resource declared
        // an `update` action.
        let router: Router = Router::new().resource("posts", Ctl).unnamed().into();
        assert!(
            router.match_route(&Method::PUT, "/posts/42").is_some(),
            "PUT /posts/{{id}} must route to update"
        );
        assert!(
            router.match_route(&Method::PATCH, "/posts/42").is_some(),
            "PATCH /posts/{{id}} must route to update"
        );
    }

    #[test]
    fn update_dropped_does_not_register_patch() {
        // The PATCH-alongside-PUT step is gated on Update being in the
        // action set. If a caller drops Update via `.except`, neither
        // PUT nor PATCH should be registered.
        let router: Router = Router::new()
            .resource("posts", Ctl)
            .except(&[ResourceAction::Update])
            .unnamed()
            .into();
        assert!(router.match_route(&Method::PUT, "/posts/42").is_none());
        assert!(router.match_route(&Method::PATCH, "/posts/42").is_none());
    }

    #[test]
    fn api_resource_drops_create_and_edit() {
        let router: Router = Router::new().api_resource("posts", Ctl).unnamed().into();
        assert!(router.match_route(&Method::GET, "/posts").is_some());
        assert!(router.match_route(&Method::POST, "/posts").is_some());
        assert!(router.match_route(&Method::GET, "/posts/42").is_some());
        assert!(router.match_route(&Method::PUT, "/posts/42").is_some());
        assert!(router.match_route(&Method::DELETE, "/posts/42").is_some());

        // No `/posts/{id}/edit` because the Edit action wasn't registered.
        // `/posts/create` would match Show via the `{post}` capture (matchit
        // is path-shape-based), but that's the same shadowing Laravel
        // exhibits and is the reason web `resource()` registers the explicit
        // `/create` route BEFORE `/{post}`. For API-mode resources callers
        // accept "create looks like a resource id" by design.
        assert!(router.match_route(&Method::GET, "/posts/42/edit").is_none());
    }

    #[test]
    fn only_restricts_to_listed_actions() {
        let router: Router = Router::new()
            .resource("posts", Ctl)
            .only(&[ResourceAction::Index, ResourceAction::Show])
            .unnamed()
            .into();
        assert!(router.match_route(&Method::GET, "/posts").is_some());
        assert!(router.match_route(&Method::GET, "/posts/1").is_some());
        // Store / update / destroy / create / edit must NOT be registered.
        assert!(router.match_route(&Method::POST, "/posts").is_none());
        assert!(router.match_route(&Method::PUT, "/posts/1").is_none());
        assert!(router.match_route(&Method::DELETE, "/posts/1").is_none());
    }

    #[test]
    fn except_drops_listed_actions() {
        let router: Router = Router::new()
            .resource("posts", Ctl)
            .except(&[ResourceAction::Destroy])
            .unnamed()
            .into();
        assert!(router.match_route(&Method::DELETE, "/posts/1").is_none());
        assert!(router.match_route(&Method::PUT, "/posts/1").is_some());
    }

    #[test]
    fn keep_is_alias_for_only() {
        let router: Router = Router::new()
            .resource("widgets", Ctl)
            .keep(&[ResourceAction::Index])
            .unnamed()
            .into();
        assert!(router.match_route(&Method::GET, "/widgets").is_some());
        assert!(router.match_route(&Method::POST, "/widgets").is_none());
    }

    #[test]
    fn drop_is_alias_for_except() {
        let router: Router = Router::new()
            .resource("widgets", Ctl)
            .drop(&[ResourceAction::Index])
            .unnamed()
            .into();
        assert!(router.match_route(&Method::GET, "/widgets").is_none());
        assert!(router.match_route(&Method::POST, "/widgets").is_some());
    }

    #[test]
    fn parameter_overrides_path_parameter_name() {
        let router: Router = Router::new()
            .resource("posts", Ctl)
            .parameter("post_id")
            .only(&[ResourceAction::Show])
            .unnamed()
            .into();
        let m = router.match_route(&Method::GET, "/posts/42");
        let (pattern, _h, params) = m.expect("show must match");
        assert_eq!(pattern, "/posts/{post_id}");
        assert_eq!(params.get("post_id"), Some(&"42".to_string()));
    }

    #[test]
    #[serial_test::serial(route_registry)]
    fn default_route_names_register() {
        crate::routing::clear_route_names_for_test();
        let _router: Router = Router::new().resource("posts", Ctl).into();
        let u = crate::routing::route("posts.index", &[]);
        assert_eq!(u.as_deref(), Some("/posts"));
        let u = crate::routing::route("posts.show", &[("post", "42")]);
        assert_eq!(u.as_deref(), Some("/posts/42"));
    }

    #[test]
    #[serial_test::serial(route_registry)]
    fn names_overrides_default_name() {
        crate::routing::clear_route_names_for_test();
        let _router: Router = Router::new()
            .resource("posts", Ctl)
            .only(&[ResourceAction::Index])
            .names([("index", "posts.list")])
            .into();
        assert_eq!(
            crate::routing::route("posts.list", &[]).as_deref(),
            Some("/posts"),
        );
        // Default name is NOT registered when overridden.
        assert!(crate::routing::route("posts.index", &[]).is_none());
    }

    #[test]
    fn default_param_singularises_common_plurals() {
        assert_eq!(default_param_name("posts"), "post");
        assert_eq!(default_param_name("categories"), "category");
        assert_eq!(default_param_name("people"), "people"); // irregular, untouched
        assert_eq!(default_param_name("admin/posts"), "post");
    }
}
