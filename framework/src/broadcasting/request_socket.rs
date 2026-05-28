//! Request-scoped broadcasting socket id.
//!
//! When a broadcasting WebSocket client connects it is assigned a `socket_id`
//! (see [`BroadcastingWsHandler`](super::BroadcastingWsHandler)) and told it via
//! the `connected` server frame. A browser client echoes that id back as the
//! `X-Socket-ID` header on HTTP requests that may trigger broadcasts. The server
//! reads the header once at request entry and stashes it in the task-local
//! installed here, so a [`Broadcastable`](super::Broadcastable) dispatched while
//! handling that request can exclude the originating connection
//! (`broadcast_to_others`).
//!
//! Mirrors [`crate::auth::request_state`]: a single [`tokio::task_local!`]
//! scoped once around request handling, with reads outside any scope (workers,
//! jobs, unit tests) degrading to `None` — so an off-request broadcast simply
//! reaches everyone.

tokio::task_local! {
    // The originating connection's socket id for the current request, if the
    // client sent `X-Socket-ID`. `None` when absent or off-request.
    static REQUEST_SOCKET: Option<String>;
}

/// Run `fut` with the request's originating socket id installed. Scoped in
/// `server.rs` alongside the auth / flash / SSR per-request task-locals.
pub(crate) async fn scope<F: std::future::Future>(socket_id: Option<String>, fut: F) -> F::Output {
    REQUEST_SOCKET.scope(socket_id, fut).await
}

/// The originating connection's socket id for the current request, or `None`
/// outside a request scope (the safe default — broadcast to everyone).
pub(crate) fn current() -> Option<String> {
    REQUEST_SOCKET.try_with(|s| s.clone()).ok().flatten()
}
