//! Notifications subsystem.
//!
//! A `Notification` declares which channels it should be sent to plus the
//! data it carries. A `Notifiable` (the recipient — typically a user model)
//! exposes how to address that recipient on each channel (email address,
//! database id, push subscription endpoint, etc.). A `Channel` knows how to
//! deliver a notification to a routed address.
//!
//! The `NotificationDispatcher` ties them together: it fans out a single
//! notification across every channel the notification declares, skipping
//! channels for which the recipient has no route.
//!
//! Concrete channels (Mail, Database, WebPush) land in Tasks 17 and 18.

pub mod channels;
pub mod notify_job;

pub use notify_job::SendNotificationJob;

use crate::error::FrameworkError;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A target of notifications — a `User`, an `Order`, etc.
///
/// Exposes per-channel addressing: `route_for("mail")` yields the email
/// address, `route_for("database")` yields the entity id as a string,
/// `route_for("webpush")` yields a serialized subscription endpoint, etc.
/// Returning `None` for a channel causes the dispatcher to skip that
/// channel for this recipient — useful for "email-only" or "push-only"
/// users.
pub trait Notifiable: Send + Sync {
    /// Return the addressable route for the named channel, if any.
    fn route_for(&self, channel: &str) -> Option<String>;
}

/// A notification — declares its channels and the serializable data it
/// carries.
///
/// `notification_name` is the stable identifier that channels (notably the
/// database channel) persist alongside the data so callers can filter by
/// notification type later.
pub trait Notification: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// A stable name for this notification type. Persisted by the database
    /// channel; used by other channels for logging and metrics.
    fn notification_name() -> &'static str
    where
        Self: Sized;

    /// Channels this notification should be dispatched to.
    fn channels(&self) -> Vec<&'static str>;

    /// JSON-serializable payload the channel will deliver / persist.
    fn data(&self) -> serde_json::Value;
}

/// Object-safe view of a [`Notification`].
///
/// Channels receive `&dyn DynNotification` so the dispatcher can fan a
/// single notification out across multiple channels without cloning or
/// re-serializing. The blanket impl below means every type that implements
/// `Notification` is automatically a `DynNotification` — consumers do not
/// implement this trait directly.
pub trait DynNotification: Send + Sync {
    /// The stable name of the underlying notification type.
    fn name(&self) -> &'static str;
    /// The JSON-serializable payload.
    fn data(&self) -> serde_json::Value;
}

impl<N: Notification> DynNotification for N {
    fn name(&self) -> &'static str {
        <N as Notification>::notification_name()
    }
    fn data(&self) -> serde_json::Value {
        <N as Notification>::data(self)
    }
}

/// A channel — knows how to deliver a notification to a routed address.
///
/// Implementors live in [`channels`]: `MailChannel` writes to the configured
/// mail transport, `DatabaseChannel` inserts a row into the `notifications`
/// table, `WebPushChannel` sends a push to a stored subscription endpoint.
#[async_trait]
pub trait Channel: Send + Sync {
    /// The name this channel registers under (e.g. `"mail"`, `"database"`,
    /// `"webpush"`). Notifications opt in by listing this name in their
    /// [`Notification::channels`] vector.
    fn name(&self) -> &'static str;

    /// Deliver `notification` to `route`. `route` is whatever the
    /// `Notifiable` returned from `route_for(self.name())`.
    async fn deliver(
        &self,
        route: &str,
        notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError>;
}

/// Fans out a notification across all matching registered channels.
///
/// Channels are registered by name; the dispatcher walks the channel list
/// declared by the notification and invokes each registered channel with
/// the route returned by the recipient. Channels declared by the
/// notification but not registered with the dispatcher are logged at WARN
/// level and skipped; channels for which the recipient returns no route
/// are skipped silently.
#[derive(Default)]
pub struct NotificationDispatcher {
    channels: HashMap<&'static str, Arc<dyn Channel>>,
}

impl NotificationDispatcher {
    /// Create an empty dispatcher.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a channel under its declared name.
    ///
    /// Last-write-wins: registering two channels with the same `name()`
    /// silently overrides the first. This makes the builder ergonomic for
    /// tests (swap a real channel for a stub) and matches the idiomatic
    /// builder pattern.
    pub fn register_channel(mut self, channel: Arc<dyn Channel>) -> Self {
        self.channels.insert(channel.name(), channel);
        self
    }

    /// Dispatch `notification` to `recipient` across every channel the
    /// notification declares.
    ///
    /// Returns on the first channel error; channels that already succeeded
    /// are not rolled back. For at-least-once semantics across multiple
    /// channels, dispatch each side via the queue (idempotency keys at the
    /// envelope layer protect against double-sends on retry).
    pub async fn notify<N, R>(
        &self,
        recipient: &R,
        notification: &N,
    ) -> Result<(), FrameworkError>
    where
        N: Notification,
        R: Notifiable + ?Sized,
    {
        for channel_name in notification.channels() {
            let Some(channel) = self.channels.get(channel_name) else {
                tracing::warn!(
                    channel = %channel_name,
                    notification = %N::notification_name(),
                    "no channel registered; skipping"
                );
                continue;
            };
            let Some(route) = recipient.route_for(channel_name) else {
                continue;
            };
            channel.deliver(&route, notification).await?;
        }
        Ok(())
    }

    /// Look up a registered channel by name. Used by
    /// [`SendNotificationJob`] to fan out a deserialized notification at
    /// dispatch time.
    pub fn channel(&self, name: &str) -> Option<Arc<dyn Channel>> {
        self.channels.get(name).cloned()
    }
}

// ============================================================================
// Queue integration: dispatcher binding, factory registry, Notify facade
// ============================================================================

/// Factory type for the notification registry. v1 uses `fn(...)` rather
/// than `Arc<dyn Fn>` because registered factories are stateless and a
/// function pointer keeps clone/copy ergonomics trivial. Bump to
/// `Arc<dyn Fn>` if a future caller needs to capture state.
pub type NotificationFactory =
    fn(serde_json::Value) -> Result<Box<dyn DynNotification>, FrameworkError>;

static DISPATCHER: RwLock<Option<Arc<NotificationDispatcher>>> = RwLock::new(None);
static FACTORIES: RwLock<Option<HashMap<String, NotificationFactory>>> = RwLock::new(None);

/// Bind a dispatcher for queued and `Notify::send` delivery. Replaces any
/// previously-bound dispatcher (last-write-wins).
pub fn set_dispatcher(d: Arc<NotificationDispatcher>) {
    *DISPATCHER
        .write()
        .expect("notification dispatcher lock poisoned") = Some(d);
}

pub(crate) fn dispatcher_for_queue() -> Result<Arc<NotificationDispatcher>, FrameworkError> {
    DISPATCHER
        .read()
        .expect("notification dispatcher lock poisoned")
        .clone()
        .ok_or_else(|| {
            FrameworkError::internal(
                "no NotificationDispatcher registered; call notifications::set_dispatcher(...)",
            )
        })
}

/// Register a notification type for queue dispatch. Call once at boot for
/// every concrete notification that is reachable via `Notify::queue`. The
/// worker rebuilds the notification through this registry using
/// `notification_name` as the lookup key; an unregistered notification
/// surfaces as `unknown notification: {name}` and either retries or
/// dead-letters per the envelope's backoff policy.
///
/// Re-registering the same name silently replaces the existing factory
/// (last-write-wins) — matches the mailable registry and the dispatcher's
/// channel registration.
pub fn register_notification_factory<N: Notification>() {
    let factory: NotificationFactory = |payload| {
        let n: N = serde_json::from_value(payload).map_err(|e| {
            FrameworkError::internal(format!(
                "decode notification {}: {e}",
                N::notification_name()
            ))
        })?;
        Ok(Box::new(n))
    };
    let mut g = FACTORIES.write().expect("notification factory lock poisoned");
    g.get_or_insert_with(HashMap::new)
        .insert(N::notification_name().to_string(), factory);
}

pub(crate) fn factory_for(name: &str) -> Result<NotificationFactory, FrameworkError> {
    let g = FACTORIES.read().expect("notification factory lock poisoned");
    let map = g.as_ref().ok_or_else(|| {
        FrameworkError::internal(format!("unknown notification: {name}"))
    })?;
    map.get(name)
        .copied()
        .ok_or_else(|| FrameworkError::internal(format!("unknown notification: {name}")))
}

/// Notification facade — mirrors the [`Mail`](crate::mail::Mail),
/// [`Queue`](crate::queue::Queue), [`Bus`](crate::bus::Bus), and
/// [`Cache`](crate::cache::Cache) patterns.
///
/// `Notify::queue` builds a [`SendNotificationJob`] and pushes it onto the
/// Phase 5A queue. `Notify::send` is the synchronous, in-process sibling —
/// it delegates straight to the bound [`NotificationDispatcher`] with no
/// queueing.
pub struct Notify;

impl Notify {
    /// Queue a notification for asynchronous delivery via the bound
    /// dispatcher. Pre-resolves the per-channel routes from `recipient` so
    /// the worker does not need a `Notifiable` handle at execute time.
    pub async fn queue<N, R>(
        recipient: &R,
        notification: N,
    ) -> Result<(), FrameworkError>
    where
        N: Notification,
        R: Notifiable + ?Sized,
    {
        let channels = notification.channels();
        let mut routes: HashMap<String, String> = HashMap::new();
        for c in &channels {
            if let Some(r) = recipient.route_for(c) {
                routes.insert((*c).to_string(), r);
            }
        }
        let payload = serde_json::to_value(&notification).map_err(|e| {
            FrameworkError::internal(format!("Notify::queue encode: {e}"))
        })?;
        let job = SendNotificationJob {
            notifiable_route_per_channel: routes,
            notification_name: N::notification_name().to_string(),
            notification_payload: payload,
            channels: channels.into_iter().map(String::from).collect(),
        };
        crate::queue::Queue::push(job).await
    }

    /// Send a notification synchronously (in-process, no queue) via the
    /// bound dispatcher. Returns on the first channel error per the
    /// dispatcher contract — channels that already succeeded are not
    /// rolled back.
    pub async fn send<N, R>(
        recipient: &R,
        notification: &N,
    ) -> Result<(), FrameworkError>
    where
        N: Notification,
        R: Notifiable + ?Sized,
    {
        let dispatcher = dispatcher_for_queue()?;
        dispatcher.notify(recipient, notification).await
    }
}
