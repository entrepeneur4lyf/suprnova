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
//! ## What's NOT included (yet)
//!
//! Laravel's flash semantics include **cross-redirect persistence**:
//! controller A flashes a value and redirects to controller B; the
//! flash data appears on B's response. That requires session
//! integration — the framework serializes the flash bag to the session
//! store on redirect responses and reads it back on the next request.
//!
//! Suprnova's session domain is its own parity track. For Tier 2 we
//! ship the in-request flash bag and the protocol-level emission;
//! the cross-redirect persistence wires up when session-flash lands.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

tokio::task_local! {
    /// Per-request flash bag. Scoped by `Server::handle_request`.
    pub(crate) static FLASH_BAG: Arc<Mutex<HashMap<String, Value>>>;
}

/// Push a value into the current request's flash bag.
///
/// Silently no-ops when there is no active flash scope (e.g. called
/// outside an HTTP handler in tests that don't set up the scope).
pub fn push(key: impl Into<String>, value: Value) {
    let _ = FLASH_BAG.try_with(|bag| {
        bag.lock().expect("flash bag poisoned").insert(key.into(), value);
    });
}

/// Drain the current request's flash bag into a JSON map. Returns an
/// empty map when no scope is active. Called by
/// [`InertiaResponse::resolve`](crate::InertiaResponse::resolve) when
/// assembling the page object.
pub fn drain() -> serde_json::Map<String, Value> {
    FLASH_BAG
        .try_with(|bag| {
            let mut guard = bag.lock().expect("flash bag poisoned");
            let entries = std::mem::take(&mut *guard);
            entries.into_iter().collect()
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
