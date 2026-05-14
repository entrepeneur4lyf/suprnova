//! Framework-emitted events. Consumers can listen to these the same
//! way they listen to their own events.

use super::Event;

/// Dispatched on every `FrameworkError` whose status code is >= 500.
/// Listeners can ship to Sentry, Datadog, Slack, etc.
#[derive(Debug, Clone)]
pub struct ErrorOccurred {
    pub error_message: String,
    pub status_code: u16,
    pub request_id: Option<String>,
}

impl Event for ErrorOccurred {
    fn event_name() -> &'static str {
        "ErrorOccurred"
    }
}
