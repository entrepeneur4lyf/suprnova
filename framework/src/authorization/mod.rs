mod gate;
mod registry;
mod response;

pub use gate::Gate;
pub use response::Response;

/// User-side ergonomic shim for [`Gate`]: `user.can(action, &resource)`
/// instead of `Gate::allows(action, &user, &resource)`. Mirrors
/// Laravel's `Authorizable` trait.
///
/// `impl Authorizable for YourUser {}` is enough — every method has
/// a default body that delegates to [`Gate`]. The trait requires
/// `Sized + 'static` so the type-erased registry can dispatch via
/// `TypeId` (same constraints [`Gate::define`] imposes).
pub trait Authorizable: Sized + 'static {
    /// `true` iff the gate registered for `(action, Self, R)` allows.
    /// Missing gates deny by default.
    fn can<R: 'static>(&self, action: &str, resource: &R) -> bool {
        Gate::allows(action, self, resource)
    }
    /// Opposite of [`Authorizable::can`].
    fn cannot<R: 'static>(&self, action: &str, resource: &R) -> bool {
        Gate::denies(action, self, resource)
    }
    /// Authorize the action, returning the denial as an error.
    ///
    /// A bare denial maps to `FrameworkError::Unauthorized` (403). A rich
    /// denial — from a [`Gate::define_with`] gate that returned a [`Response`]
    /// with a custom message/status — maps to `FrameworkError::Domain`
    /// carrying that message and status (e.g. 404 from
    /// `Response::deny_as_not_found()`).
    fn authorize<R: 'static>(
        &self,
        action: &str,
        resource: &R,
    ) -> Result<(), crate::FrameworkError> {
        Gate::authorize(action, self, resource)
    }
    /// Async sibling of [`Authorizable::can`].
    fn can_async<'a, R>(
        &'a self,
        action: &'a str,
        resource: &'a R,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>>
    where
        Self: Sync,
        R: 'static + Sync,
    {
        Box::pin(Gate::allows_async(action, self, resource))
    }
    /// Async sibling of [`Authorizable::cannot`].
    fn cannot_async<'a, R>(
        &'a self,
        action: &'a str,
        resource: &'a R,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>>
    where
        Self: Sync,
        R: 'static + Sync,
    {
        Box::pin(Gate::denies_async(action, self, resource))
    }
    /// Async sibling of [`Authorizable::authorize`]. Same error mapping:
    /// bare denials become `FrameworkError::Unauthorized`, rich denials
    /// (from `Gate::define_async_with`) become `FrameworkError::Domain`
    /// preserving the message and status.
    fn authorize_async<'a, R>(
        &'a self,
        action: &'a str,
        resource: &'a R,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), crate::FrameworkError>> + Send + 'a>,
    >
    where
        Self: Sync,
        R: 'static + Sync,
    {
        Box::pin(Gate::authorize_async(action, self, resource))
    }
}

// ── inventory-based policy registration ──────────────────────────────────────

/// Registration record emitted by `#[policy]` via `inventory::submit!`.
///
/// `register` is a zero-arg closure that calls `Gate::define` (for a `bool`
/// method) or `Gate::define_with` (for a `Response` method) for one action.
#[doc(hidden)]
pub struct __PolicyRegistration {
    pub register: fn(),
}

inventory::collect!(__PolicyRegistration);

/// Eagerly run all `#[policy]` gate registrations.
///
/// Called automatically from `Server::serve`. May also be called manually in
/// tests. Safe to call multiple times — the inner `Once` ensures each
/// registered closure runs exactly once.
pub fn init_policies() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for reg in inventory::iter::<__PolicyRegistration> {
            (reg.register)();
        }
    });
}
