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
