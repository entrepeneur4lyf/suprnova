//! Notification lifecycle events.
//!
//! Three events surround every channel delivery:
//!
//! - [`NotificationSending`] — dispatched *before* the channel runs. If any
//!   listener returns an error, the channel is skipped (the dispatcher treats
//!   this as a veto rather than a propagated failure). This matches Laravel's
//!   `NotificationSending` cancellable-event semantics where a listener
//!   returning `false` short-circuits delivery for that channel.
//! - [`NotificationSent`] — dispatched after a successful channel delivery.
//! - [`NotificationFailed`] — dispatched when a channel returns an error.
//!   The dispatcher then propagates the underlying error per its existing
//!   first-failure-stops contract.
//!
//! Events carry only the notification *name* and JSON *payload* — not the
//! original `&dyn Notifiable` — because they cross the `Event` trait's
//! `Clone + Send + 'static + Debug` bound, which a borrowed dyn trait
//! object cannot satisfy. The recipient's per-channel `route` is included
//! verbatim so listeners can correlate without dipping back into the
//! `Notifiable`.

use crate::events::Event;
use serde_json::Value;

/// Dispatched immediately before a channel's `deliver` runs.
///
/// Listeners that error out are treated as a per-channel **veto** by the
/// dispatcher (the channel is skipped, the rest of the channels continue).
/// This is the Suprnova analogue of Laravel's `NotificationSending` event
/// whose listeners can return `false` to abort.
#[derive(Clone, Debug)]
pub struct NotificationSending {
    /// `Notification::notification_name()` of the notification being sent.
    pub notification: String,
    /// Name of the channel about to deliver (`"mail"`, `"database"`, …).
    pub channel: String,
    /// The route value returned by the recipient's `route_for(channel)`.
    pub route: String,
    /// The notification's JSON payload (the same blob that channels see).
    pub data: Value,
}

impl Event for NotificationSending {
    fn event_name() -> &'static str {
        "Suprnova::Notifications::Sending"
    }
}

/// Dispatched after a channel's `deliver` returned `Ok(())`.
#[derive(Clone, Debug)]
pub struct NotificationSent {
    /// `Notification::notification_name()` of the notification that was delivered.
    pub notification: String,
    /// Name of the channel that returned `Ok(())`.
    pub channel: String,
    /// The route value returned by the recipient's `route_for(channel)`.
    pub route: String,
    /// The notification's JSON payload (the same blob the channel saw).
    pub data: Value,
}

impl Event for NotificationSent {
    fn event_name() -> &'static str {
        "Suprnova::Notifications::Sent"
    }
}

/// Dispatched when a channel's `deliver` returned an error. The error itself
/// is stringified so the event remains `Clone + Send + 'static` (the
/// underlying `FrameworkError` keeps propagating to the caller per the
/// dispatcher's first-failure-stops contract).
#[derive(Clone, Debug)]
pub struct NotificationFailed {
    /// `Notification::notification_name()` of the notification that failed.
    pub notification: String,
    /// Name of the channel whose `deliver` returned an error.
    pub channel: String,
    /// The route value returned by the recipient's `route_for(channel)`.
    pub route: String,
    /// The notification's JSON payload (the same blob the channel saw).
    pub data: Value,
    /// Stringified channel error. Captured at event time so listeners can
    /// log without needing the `FrameworkError`-to-`String` conversion
    /// themselves.
    pub error: String,
}

impl Event for NotificationFailed {
    fn event_name() -> &'static str {
        "Suprnova::Notifications::Failed"
    }
}
