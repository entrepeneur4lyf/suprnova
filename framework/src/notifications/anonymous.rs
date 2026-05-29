//! On-demand / anonymous notifications.
//!
//! `AnonymousNotifiable` is a "user without a row" — it carries a per-channel
//! route map directly, so callers can fire a notification at an arbitrary
//! email/endpoint/channel without needing a database-backed recipient.
//!
//! Pair with [`crate::notifications::Notify::route`] /
//! [`crate::notifications::Notify::routes`] to build one fluently and
//! dispatch it via the bound `NotificationDispatcher`.

use crate::notifications::Notifiable;
use std::collections::HashMap;

/// A notifiable target without a backing model — built from per-channel
/// `(channel, route)` pairs.
///
/// Mirrors Laravel's `Illuminate\Notifications\AnonymousNotifiable`. Use it
/// when the recipient isn't a stored user (e.g. a one-off email confirmation
/// to a non-account address, a webhook receiver, a Slack channel).
///
/// # Database channel is rejected
///
/// Laravel throws `InvalidArgumentException` when `route('database', …)` is
/// called on an anonymous notifiable, because the database channel needs a
/// `(notifiable_type, notifiable_id)` polymorphic recipient that an
/// anonymous target cannot provide. Suprnova matches that contract:
/// `AnonymousNotifiable::route("database", …)` returns an `Err` rather than
/// silently misroute.
#[derive(Debug, Clone)]
pub struct AnonymousNotifiable {
    routes: HashMap<String, String>,
}

impl AnonymousNotifiable {
    /// Construct an empty anonymous notifiable. Add routes with
    /// [`Self::route`] / [`Self::routes`].
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
        }
    }

    /// Add a per-channel route. Returns `self` for chaining.
    ///
    /// Returns an error when the channel name is `"database"` — see the
    /// type-level docs for why.
    pub fn route(
        mut self,
        channel: impl Into<String>,
        route: impl Into<String>,
    ) -> Result<Self, crate::FrameworkError> {
        let channel = channel.into();
        if channel == "database" {
            return Err(crate::FrameworkError::internal(
                "the database channel does not support on-demand notifications \
                 — an anonymous notifiable has no polymorphic id to attach the \
                 row to",
            ));
        }
        self.routes.insert(channel, route.into());
        Ok(self)
    }

    /// Add multiple `(channel, route)` pairs at once. Equivalent to a fold
    /// over [`Self::route`] — same database-channel rejection applies.
    pub fn routes<I, C, R>(mut self, pairs: I) -> Result<Self, crate::FrameworkError>
    where
        I: IntoIterator<Item = (C, R)>,
        C: Into<String>,
        R: Into<String>,
    {
        for (c, r) in pairs {
            self = self.route(c, r)?;
        }
        Ok(self)
    }

    /// Return the raw route map. Mainly useful for tests that want to
    /// inspect what was configured before dispatch.
    pub fn raw_routes(&self) -> &HashMap<String, String> {
        &self.routes
    }
}

impl Default for AnonymousNotifiable {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifiable for AnonymousNotifiable {
    fn route_for(&self, channel: &str) -> Option<String> {
        self.routes.get(channel).cloned()
    }
}
