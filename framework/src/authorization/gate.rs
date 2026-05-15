use super::registry::global;
use crate::FrameworkError;

/// Authorization gate facade.
///
/// ```ignore
/// Gate::define::<User, Post>("view", |user, post| post.is_public || user.is_admin);
///
/// if Gate::allows("view", &user, &post) {
///     // ...
/// }
/// ```
pub struct Gate;

impl Gate {
    /// Define an authorization closure for a given action on a user–resource pair.
    pub fn define<U: 'static, R: 'static>(
        action: &str,
        f: impl Fn(&U, &R) -> bool + Send + Sync + 'static,
    ) {
        global().register::<U, R>(action, f);
    }

    /// Returns `true` when the gate exists and allows the action.
    /// Missing gates **deny by default**.
    pub fn allows<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> bool {
        global()
            .invoke(action, user, resource)
            .unwrap_or(false)
    }

    pub fn denies<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> bool {
        !Self::allows(action, user, resource)
    }

    /// Return `Err(FrameworkError::Unauthorized)` when denied.
    pub fn authorize<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> Result<(), FrameworkError> {
        if Self::allows(action, user, resource) {
            Ok(())
        } else {
            Err(FrameworkError::Unauthorized)
        }
    }
}
