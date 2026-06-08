//! Framework-emitted events. Consumers can listen to these the same
//! way they listen to their own events.

use super::Event;

/// Dispatched **best-effort** when a `FrameworkError` converts to a 5xx
/// response, so listeners can ship to Sentry, Datadog, Slack, etc.
///
/// Delivery is not guaranteed. The dispatch is spawned (not awaited) so it
/// never blocks response conversion, and if no Tokio runtime is active at
/// conversion time — e.g. a 5xx path exercised by a non-async unit test — it
/// is dropped. Inside a running server every 5xx triggers it. Treat it as a
/// high-signal error feed, not a complete audit log: a listener that must
/// observe every error should also consume the structured `tracing` 5xx logs,
/// which are emitted unconditionally on the same path.
#[derive(Debug, Clone)]
pub struct ErrorOccurred {
    /// Sanitized error message (the same body the client received).
    pub error_message: String,
    /// HTTP status code of the response (always 5xx in current usage).
    pub status_code: u16,
    /// Request id of the failing request, when one was installed.
    pub request_id: Option<String>,
}

impl Event for ErrorOccurred {
    fn event_name() -> &'static str {
        "ErrorOccurred"
    }
}
