mod gate;
mod registry;

pub use gate::Gate;

/// Trait-based policy convenience. Implement `Policy` once on a
/// resource type and the `#[policy]` macro will wire it up.
///
/// Seven defaults mirror Laravel's resource policy conventions
/// (`viewAny`, `view`, `create`, `update`, `delete`, `restore`,
/// `forceDelete`) with Rust-native snake_case naming. Default
/// behaviour is "view/create allows, mutation denies" — override
/// the methods you actually want to gate. The `#[policy]` macro
/// registers only methods present in your `impl` block, so omitting
/// a default keeps the trait's behaviour active without producing
/// a registered gate.
///
/// Naming divergence from Laravel: snake_case (`view_any`/
/// `force_delete`) rather than camelCase. The `#[policy]` macro
/// derives the action string directly from the method name, so
/// `Gate::allows("view_any", &user, &post)` is the call shape.
pub trait Policy<U: 'static>: 'static {
    /// Authorize listing the collection of this resource type.
    fn view_any(user: &U) -> bool {
        let _ = user;
        true
    }
    /// Authorize viewing a single instance.
    fn view(&self, user: &U) -> bool {
        let _ = user;
        true
    }
    /// Authorize creating a new instance.
    fn create(_: &U) -> bool {
        true
    }
    /// Authorize updating this instance.
    fn update(&self, user: &U) -> bool {
        let _ = user;
        false
    }
    /// Authorize deleting this instance.
    fn delete(&self, user: &U) -> bool {
        let _ = user;
        false
    }
    /// Authorize restoring a soft-deleted instance.
    fn restore(&self, user: &U) -> bool {
        let _ = user;
        false
    }
    /// Authorize permanently destroying a soft-deleted instance.
    fn force_delete(&self, user: &U) -> bool {
        let _ = user;
        false
    }
}

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
    /// Return `Err(FrameworkError::Unauthorized)` when denied.
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
    /// Async sibling of [`Authorizable::authorize`].
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
/// `register` is a zero-arg closure that calls `Gate::define` for one action.
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
