//! Authentication lifecycle events.
//!
//! Five events mirroring `Illuminate\Auth\Events`, dispatched by the
//! guards during the login lifecycle:
//!
//! - [`Attempting`] — a credential login attempt began
//!   ([`StatefulGuard::attempt`](super::StatefulGuard::attempt) /
//!   [`once`](super::StatefulGuard::once)).
//! - [`Authenticated`] — a user was actively authenticated this request
//!   (`login`/`once`/`once_using_id`).
//! - [`Login`] — a user logged in with session persistence
//!   ([`login`](super::StatefulGuard::login)).
//! - [`Logout`] — a user logged out ([`logout`](super::StatefulGuard::logout)).
//! - [`Failed`] — a credential attempt failed (bad password or unknown
//!   identifier).
//!
//! Every event carries the guard name plus stringy identifiers only — no
//! plaintext passwords and no raw credential maps, matching the
//! `auth_flows` events' "no sensitive data on the wire" contract. Email-
//! keyed failed-attempt tracking is the job of
//! [`crate::auth_flows::BruteForce`], not these lifecycle events.
//!
//! # Divergence: `Authenticated` fires on active authentication only
//!
//! These guards dispatch [`Authenticated`] from the methods that
//! *establish* a user (`login`/`once`/`once_using_id`), not from a
//! passive `user()` resolution off an existing session. This keeps the
//! read path infallible and free of per-call event noise. A listener
//! that needs "user seen this request" should hook [`Login`] plus its
//! own request middleware rather than rely on `Authenticated` firing for
//! every already-logged-in request.

use crate::events::Event;

/// Fires at the start of a credential login attempt, before the
/// credentials are checked. Mirrors Laravel's `Attempting`.
#[derive(Debug, Clone)]
pub struct Attempting {
    /// The guard name the attempt ran against (e.g. `"web"`).
    pub guard: String,
    /// Whether a remember-me token was requested for this attempt.
    pub remember: bool,
}

impl Event for Attempting {
    fn event_name() -> &'static str {
        "Auth\\Attempting"
    }
}

/// Fires when a user is actively authenticated this request — after a
/// successful `login`, `once`, or `once_using_id`. Mirrors Laravel's
/// `Authenticated`.
#[derive(Debug, Clone)]
pub struct Authenticated {
    /// The guard name that authenticated the user.
    pub guard: String,
    /// The authenticated user's identifier.
    pub user_id: String,
}

impl Event for Authenticated {
    fn event_name() -> &'static str {
        "Auth\\Authenticated"
    }
}

/// Fires when a user logs in with session persistence. Mirrors
/// Laravel's `Login`.
#[derive(Debug, Clone)]
pub struct Login {
    /// The guard name the user logged in through.
    pub guard: String,
    /// The user's identifier.
    pub user_id: String,
    /// Whether a remember-me token was issued for this login.
    pub remember: bool,
}

impl Event for Login {
    fn event_name() -> &'static str {
        "Auth\\Login"
    }
}

/// Fires when a user logs out. Mirrors Laravel's `Logout`.
#[derive(Debug, Clone)]
pub struct Logout {
    /// The guard name the user logged out of.
    pub guard: String,
    /// The identifier of the user who was logged in, if one was.
    pub user_id: Option<String>,
}

impl Event for Logout {
    fn event_name() -> &'static str {
        "Auth\\Logout"
    }
}

/// Fires when a credential attempt fails. Mirrors Laravel's `Failed`.
///
/// `user_id` is `Some` when the identifier matched a real user but the
/// credentials were wrong (e.g. bad password), and `None` when no user
/// matched the supplied credentials at all.
#[derive(Debug, Clone)]
pub struct Failed {
    /// The guard name the attempt ran against.
    pub guard: String,
    /// The matched user's identifier, if the identifier was valid but the
    /// credentials were not.
    pub user_id: Option<String>,
}

impl Event for Failed {
    fn event_name() -> &'static str {
        "Auth\\Failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_distinct() {
        let mut names = vec![
            Attempting::event_name(),
            Authenticated::event_name(),
            Login::event_name(),
            Logout::event_name(),
            Failed::event_name(),
        ];
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(
            names.len(),
            before,
            "duplicate event_name() across auth events: {names:?}"
        );
    }
}
