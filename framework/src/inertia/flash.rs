//! Per-request flash data.
//!
//! Inertia v3's `page.flash` field carries one-shot data — toasts,
//! success messages, newly-created IDs — that should appear on the
//! current page but not persist across navigations.
//!
//! ## Storage model
//!
//! Flash data lives in a `tokio::task_local!` set up at the request
//! boundary by `Server::handle_request`. Within a request, anywhere
//! that can `.await` can call [`App::flash`](crate::App::flash) to
//! push values; [`InertiaResponse::resolve`](crate::InertiaResponse::resolve)
//! drains the bag at response build time and emits the contents under
//! the top-level `flash` field of the page object.
//!
//! `task_local!` (rather than `thread_local!`) is the correct primitive
//! for per-request state under Tokio: the binding follows the task
//! across `.await` points even when the runtime moves it to a different
//! worker thread. The thread-local InertiaContext bug we fixed in Tier 0
//! is exactly the kind of problem this avoids.
//!
//! ## Cross-redirect persistence
//!
//! Laravel's flash semantics include **cross-redirect persistence**:
//! controller A flashes a value and redirects to controller B; the
//! flash data appears on B's response. Suprnova implements this by
//! bridging the per-request flash bag into the session on every
//! [`Redirect`](crate::http::Redirect) → [`Response`](crate::http::Response)
//! conversion. The receiving request's
//! [`SessionMiddleware`](crate::session::SessionMiddleware) ages the
//! flashed values into `_flash.old.*`, and
//! [`InertiaResponse::resolve`](crate::InertiaResponse::resolve)
//! merges them into the page object's top-level `flash` field
//! alongside same-request flashes from [`App::flash`](crate::App::flash)
//! and [`InertiaResponse::flash`](crate::InertiaResponse::flash).
//!
//! ### Precedence on key collision
//!
//! Same-request flash (task-local bag + builder) wins over session
//! `_flash.old.*` so a destination handler can override an inherited
//! value just by re-flashing the same key.
//!
//! ### Internal session keys are filtered
//!
//! Session flash is shared with the framework's own one-shot signals
//! (`_old_input` for form repopulation, `_inertia.*` for protocol
//! flags). Only user-visible keys are surfaced to `page.flash` — keys
//! prefixed with `_` are filtered out.

use crate::lock;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

tokio::task_local! {
    /// Per-request flash bag. Scoped by `Server::handle_request`.
    pub(crate) static FLASH_BAG: Arc<Mutex<HashMap<String, Value>>>;
}

tokio::task_local! {
    /// Per-request history-encryption flag set by
    /// [`EncryptHistoryMiddleware`](crate::inertia::EncryptHistoryMiddleware).
    /// Read by `InertiaResponse::resolve` alongside the per-response
    /// override and the config default. See the v3 history-encryption
    /// docs for protocol details.
    pub(crate) static ENCRYPT_HISTORY: bool;
}

/// Whether the active request has been marked for history encryption
/// by [`EncryptHistoryMiddleware`]. Returns `None` when no middleware
/// has set the flag; the caller should fall back to the config default.
pub(crate) fn encrypt_history_flag() -> Option<bool> {
    ENCRYPT_HISTORY.try_with(|b| *b).ok()
}

/// Push a value into the current request's flash bag.
///
/// Silently no-ops when there is no active flash scope (e.g. called
/// outside an HTTP handler in tests that don't set up the scope).
///
/// **Poison policy** (Domain 20 audit D20-A): the per-request flash
/// `Mutex` is scoped to a single request and recreated on the next
/// one, so poison only affects the request that experienced the
/// upstream panic. On poison the push is dropped silently and a
/// `tracing::error!` is emitted — the request is already failing,
/// so silent loss matches the documented "no active scope" no-op.
pub fn push(key: impl Into<String>, value: Value) {
    let _ = FLASH_BAG.try_with(|bag| match lock::lock(bag, "inertia flash bag") {
        Ok(mut guard) => {
            guard.insert(key.into(), value);
        }
        Err(_) => {
            tracing::error!(
                "Inertia flash bag lock poisoned; dropping push (the upstream \
                 panic that poisoned the lock is already converted to a 500 \
                 by the panic-catch middleware)."
            );
        }
    });
}

/// Drain the current request's flash bag into a JSON map. Returns an
/// empty map when no scope is active. Called by
/// [`InertiaResponse::resolve`](crate::InertiaResponse::resolve) when
/// assembling the page object.
///
/// **Poison policy** (Domain 20 audit D20-A): on per-request Mutex
/// poison the drain returns an empty map and logs at `error` level.
/// Same per-request-scoped reasoning as [`push`].
pub fn drain() -> serde_json::Map<String, Value> {
    FLASH_BAG
        .try_with(|bag| match lock::lock(bag, "inertia flash bag") {
            Ok(mut guard) => {
                let entries = std::mem::take(&mut *guard);
                entries.into_iter().collect()
            }
            Err(_) => {
                tracing::error!("Inertia flash bag lock poisoned; returning empty drain.");
                serde_json::Map::new()
            }
        })
        .unwrap_or_default()
}

/// Create a fresh flash bag suitable for scoping into [`FLASH_BAG`].
///
/// Used by [`Server::handle_request`] when wrapping each request in
/// the flash scope.
pub(crate) fn new_bag() -> Arc<Mutex<HashMap<String, Value>>> {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Bridge the per-request flash bag into the active session as
/// `_flash.new.*` so the values survive an outgoing redirect.
///
/// Called by `From<Redirect> for Response` immediately before the HTTP
/// response is built. On the receiving request the
/// [`SessionMiddleware`](crate::session::SessionMiddleware) ages the
/// values into `_flash.old.*`, and
/// [`drain_session_flash_for_page`] surfaces them under the page
/// object's top-level `flash` field.
///
/// No-op when no session scope is active (e.g. the route is outside
/// the session middleware) — the values remain in the task-local bag
/// and still appear on the *current* response via [`drain`], but they
/// cannot persist past the redirect because there is no session to
/// persist them into.
///
/// **Move semantics**: the task-local bag is drained on transfer so a
/// redirect handler that returns a non-redirect response after calling
/// [`push`] still sees the values in [`drain`]. The double-drain risk
/// only applies on the redirect path, where the drained values are
/// transferred to the session and the current response is discarded by
/// the client following the `Location` header.
pub fn transfer_to_session() {
    let entries = drain();
    if entries.is_empty() {
        return;
    }
    crate::session::session_mut(|s| {
        for (k, v) in entries {
            s.flash(&k, v);
        }
    });
}

/// Collect the receiving request's session `_flash.old.*` entries
/// that should surface under the page object's top-level `flash`
/// field.
///
/// Filters out internal session keys (anything `_`-prefixed) so the
/// `_old_input` form-repopulation bag and the `_inertia.*` protocol
/// flags don't leak to the client. The unprefixed `_old_input` itself
/// is also filtered as belt-and-suspenders against a future move of
/// the constant.
///
/// Returns an empty map outside a `SessionMiddleware` scope.
pub fn drain_session_flash_for_page() -> serde_json::Map<String, Value> {
    crate::session::session()
        .map(|s| {
            let mut out = serde_json::Map::new();
            for (key, value) in &s.data {
                let Some(name) = key.strip_prefix("_flash.old.") else {
                    continue;
                };
                if name.starts_with('_') {
                    continue;
                }
                out.insert(name.to_string(), value.clone());
            }
            out
        })
        .unwrap_or_default()
}
