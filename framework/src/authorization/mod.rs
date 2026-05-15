mod gate;
mod registry;

pub use gate::Gate;

/// Trait-based policy convenience. Implement `Policy` once on a
/// resource type and the `#[policy]` macro will wire it up.
pub trait Policy<U: 'static>: 'static {
    fn view(&self, user: &U) -> bool {
        let _ = user;
        true
    }
    fn create(_: &U) -> bool {
        true
    }
    fn update(&self, user: &U) -> bool {
        let _ = user;
        false
    }
    fn delete(&self, user: &U) -> bool {
        let _ = user;
        false
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
