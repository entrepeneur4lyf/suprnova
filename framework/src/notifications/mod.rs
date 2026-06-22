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

pub mod anonymous;
pub mod channels;
pub mod database_read;
pub mod events;
pub mod notify_job;
pub mod testing;

pub use anonymous::AnonymousNotifiable;
pub use database_read::{
    StoredNotification, all_for, delete_for, mark_all_as_read, mark_as_read, mark_as_unread,
    read_for, unread_for,
};
pub use events::{NotificationFailed, NotificationSending, NotificationSent};
pub use notify_job::SendNotificationJob;
pub use testing::{
    FakeRecord, NotifyFakeGuard, assert_count, assert_nothing_sent, assert_nothing_sent_to,
    assert_sent, assert_sent_named, assert_sent_times, assert_sent_to, assert_sent_to_on,
    recorded as recorded_notifications,
};

use crate::error::FrameworkError;
use crate::lock;
use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
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

    /// Per-channel veto. The dispatcher consults this immediately before
    /// invoking the channel; returning `false` skips that channel for this
    /// recipient. Default: always send. Mirrors Laravel's
    /// `Notification::shouldSend($notifiable, $channel)` short-circuit. The
    /// veto runs ahead of the `NotificationSending` event so the trait-level
    /// decision wins over listener-level veto without dispatching the event
    /// unnecessarily.
    fn should_send(&self, _channel: &str) -> bool {
        true
    }

    /// Post-send hook, invoked once per channel that completed successfully.
    /// Default: no-op. Mirrors Laravel's
    /// `Notification::afterSending($notifiable, $channel, $response)` — minus
    /// `$response` because Suprnova channels return `Result<(), …>` rather
    /// than a per-channel response object. Errors raised here propagate the
    /// same way as channel errors (short-circuits the remaining channels).
    fn after_sending(&self, _channel: &str) -> Result<(), FrameworkError> {
        Ok(())
    }
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
    /// Object-safe forward to [`Notification::should_send`]. The queued path
    /// consults this so consent / opt-out / quiet-hours vetoes are honoured
    /// on the worker exactly as they are on the synchronous dispatcher.
    fn should_send(&self, channel: &str) -> bool;
    /// Object-safe forward to [`Notification::after_sending`]. Invoked by the
    /// worker once per channel that delivered successfully.
    fn after_sending(&self, channel: &str) -> Result<(), FrameworkError>;
}

impl<N: Notification> DynNotification for N {
    fn name(&self) -> &'static str {
        <N as Notification>::notification_name()
    }
    fn data(&self) -> serde_json::Value {
        <N as Notification>::data(self)
    }
    fn should_send(&self, channel: &str) -> bool {
        <N as Notification>::should_send(self, channel)
    }
    fn after_sending(&self, channel: &str) -> Result<(), FrameworkError> {
        <N as Notification>::after_sending(self, channel)
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
    /// channels, dispatch via [`Notify::queue`] — which pushes one job per
    /// declared channel so a transient failure on channel B retries only
    /// channel B, never re-sending the channel-A side that already
    /// succeeded.
    ///
    /// Lifecycle events:
    /// - [`events::NotificationSending`] fires immediately before each channel's
    ///   `deliver` runs. A listener that returns an error is treated as a
    ///   per-channel veto — the channel is skipped, remaining channels
    ///   continue.
    /// - [`events::NotificationSent`] fires after a successful delivery.
    /// - [`events::NotificationFailed`] fires when delivery returned an error; the
    ///   underlying error then propagates per the first-failure-stops
    ///   contract.
    ///
    /// Telemetry: wraps the fan-out in a `notification.dispatch` info
    /// span. The span carries the notification name + declared channel
    /// count; per-channel sends emit their own events inside the span
    /// (`mail.send` for the mail channel; database/webpush channels do
    /// not currently span). On completion the span records a
    /// `duration_ms` event so an observability backend can measure
    /// fan-out latency end-to-end.
    pub async fn notify<N, R>(&self, recipient: &R, notification: &N) -> Result<(), FrameworkError>
    where
        N: Notification,
        R: Notifiable + ?Sized,
    {
        use tracing::Instrument;
        let channel_names = notification.channels();
        let span = tracing::info_span!(
            "notification.dispatch",
            notification = N::notification_name(),
            channel_count = channel_names.len(),
        );
        async move {
            let start = std::time::Instant::now();
            let mut result: Result<(), FrameworkError> = Ok(());
            for channel_name in channel_names {
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

                // Trait-level per-channel veto. Mirrors Laravel's
                // `shouldSend` short-circuit; runs ahead of the
                // `NotificationSending` event so listener veto and trait
                // veto are independent and the event isn't dispatched
                // when the trait already vetoed.
                if !notification.should_send(channel_name) {
                    continue;
                }

                let data = notification.data();
                let payload_name = N::notification_name().to_string();
                let channel_str = channel_name.to_string();
                let route_owned = route.clone();

                // Sending event — listener errors veto the channel.
                let sending = events::NotificationSending {
                    notification: payload_name.clone(),
                    channel: channel_str.clone(),
                    route: route_owned.clone(),
                    data: data.clone(),
                };
                if let Err(veto) = crate::events::EventFacade::dispatch(sending).await {
                    tracing::debug!(
                        channel = %channel_str,
                        notification = %payload_name,
                        reason = %veto,
                        "NotificationSending listener veto; skipping channel"
                    );
                    continue;
                }

                match channel.deliver(&route, notification).await {
                    Ok(()) => {
                        // Delivery succeeded — emit Sent before running the
                        // post-send hook, so a failing after_sending can't
                        // suppress the event for a notification that was in
                        // fact delivered.
                        let _ = crate::events::EventFacade::dispatch_best_effort(
                            events::NotificationSent {
                                notification: payload_name.clone(),
                                channel: channel_str.clone(),
                                route: route_owned.clone(),
                                data: data.clone(),
                            },
                        )
                        .await;
                        if let Err(e) = notification.after_sending(channel_name) {
                            result = Err(e);
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = crate::events::EventFacade::dispatch_best_effort(
                            events::NotificationFailed {
                                notification: payload_name.clone(),
                                channel: channel_str.clone(),
                                route: route_owned.clone(),
                                data: data.clone(),
                                error: e.to_string(),
                            },
                        )
                        .await;
                        result = Err(e);
                        break;
                    }
                }
            }
            let duration_ms = start.elapsed().as_millis() as u64;
            match &result {
                Ok(()) => tracing::info!(duration_ms, "notification dispatched"),
                Err(e) => tracing::warn!(duration_ms, error = %e, "notification dispatch failed"),
            }
            result
        }
        .instrument(span)
        .await
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
///
/// Returns [`FrameworkError::internal`] if the dispatcher registry lock is
/// poisoned (a prior writer panicked) rather than panicking — the crate-wide
/// write-poison policy lives in `crate::lock`.
pub fn set_dispatcher(d: Arc<NotificationDispatcher>) -> Result<(), FrameworkError> {
    *lock::write(&DISPATCHER, "notifications dispatcher")? = Some(d);
    Ok(())
}

pub(crate) fn dispatcher_for_queue() -> Result<Arc<NotificationDispatcher>, FrameworkError> {
    lock::read(&DISPATCHER, "notifications dispatcher")?
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
pub fn register_notification_factory<N: Notification>() -> Result<(), FrameworkError> {
    let factory: NotificationFactory = |payload| {
        let n: N = serde_json::from_value(payload).map_err(|e| {
            FrameworkError::internal(format!(
                "decode notification {}: {e}",
                N::notification_name()
            ))
        })?;
        Ok(Box::new(n))
    };
    let mut g = lock::write(&FACTORIES, "notification factory registry")?;
    g.get_or_insert_with(HashMap::new)
        .insert(N::notification_name().to_string(), factory);
    Ok(())
}

pub(crate) fn factory_for(name: &str) -> Result<NotificationFactory, FrameworkError> {
    let g = lock::read(&FACTORIES, "notification factory registry")?;
    let map = g
        .as_ref()
        .ok_or_else(|| FrameworkError::internal(format!("unknown notification: {name}")))?;
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
    /// dispatcher. Pre-resolves the per-channel route from `recipient` so
    /// the worker does not need a `Notifiable` handle at execute time.
    ///
    /// Pushes ONE [`SendNotificationJob`] per declared channel that
    /// resolves a route. This makes retries per-channel: if the mail
    /// channel fails after the database row was already inserted, only
    /// the mail job re-runs — the recipient does not get the database row
    /// inserted twice. Channels with no matching route on `recipient` are
    /// skipped, mirroring the dispatcher's `route_for(channel).is_none()`
    /// behaviour.
    ///
    /// The per-channel [`Notification::should_send`] veto is honoured here
    /// too: a channel the notification vetoes is never enqueued. The worker
    /// re-checks the veto before delivering and runs
    /// [`Notification::after_sending`] on success, so consent / opt-out /
    /// quiet-hours suppression behaves identically on the queued and
    /// synchronous paths even if state changes between enqueue and run.
    ///
    /// The push loop is non-atomic across channels: if the second of
    /// three pushes fails, the first channel is already queued and the
    /// caller sees `Err`. The trade-off is intentional — it is strictly
    /// better than the previous shape's worker-side double-send on
    /// partial failure.
    ///
    /// Under [`Notify::fake`] the notification is recorded for each
    /// declared channel that resolves a route — no queue push, no channel
    /// execution.
    pub async fn queue<N, R>(recipient: &R, notification: N) -> Result<(), FrameworkError>
    where
        N: Notification,
        R: Notifiable + ?Sized,
    {
        let channels = notification.channels();

        if testing::is_active() {
            let data = notification.data();
            for c in &channels {
                if let Some(route) = recipient.route_for(c) {
                    testing::record(testing::FakeRecord {
                        notification: N::notification_name().to_string(),
                        channel: (*c).to_string(),
                        route,
                        data: data.clone(),
                    });
                }
            }
            return Ok(());
        }

        let payload = serde_json::to_value(&notification)
            .map_err(|e| FrameworkError::internal(format!("Notify::queue encode: {e}")))?;
        let name = N::notification_name().to_string();

        for channel in &channels {
            let Some(route) = recipient.route_for(channel) else {
                continue;
            };
            // Trait-level per-channel veto, honoured before the job is
            // enqueued so a vetoed channel never reaches the worker.
            // Re-checked again in `SendNotificationJob::handle` because
            // consent state can change between enqueue and run.
            if !notification.should_send(channel) {
                continue;
            }
            let mut routes: HashMap<String, String> = HashMap::new();
            routes.insert((*channel).to_string(), route);
            let job = SendNotificationJob {
                notifiable_route_per_channel: routes,
                notification_name: name.clone(),
                notification_payload: payload.clone(),
                channels: vec![(*channel).to_string()],
            };
            crate::queue::Queue::push(job).await?;
        }
        Ok(())
    }

    /// Send a notification synchronously (in-process, no queue) via the
    /// bound dispatcher. Returns on the first channel error per the
    /// dispatcher contract — channels that already succeeded are not
    /// rolled back.
    ///
    /// Under [`Notify::fake`] the notification is recorded per declared
    /// channel that resolved a route; the bound dispatcher is never
    /// consulted. Channels whose `route_for(channel)` returns `None` are
    /// skipped to match the real dispatcher.
    pub async fn send<N, R>(recipient: &R, notification: &N) -> Result<(), FrameworkError>
    where
        N: Notification,
        R: Notifiable + ?Sized,
    {
        if testing::is_active() {
            let data = notification.data();
            for c in notification.channels() {
                if let Some(route) = recipient.route_for(c) {
                    testing::record(testing::FakeRecord {
                        notification: N::notification_name().to_string(),
                        channel: c.to_string(),
                        route,
                        data: data.clone(),
                    });
                }
            }
            return Ok(());
        }
        let dispatcher = dispatcher_for_queue()?;
        dispatcher.notify(recipient, notification).await
    }

    /// Install the notification fake for the current test. Returns an RAII
    /// guard that uninstalls on drop. While the guard is live, every
    /// `Notify::send` / `Notify::queue` records the dispatch instead of
    /// running channels or enqueuing a job.
    ///
    /// The guard holds a process-wide serialization mutex, so parallel
    /// tests cannot share the fake store.
    pub fn fake() -> NotifyFakeGuard {
        testing::install_fake()
    }

    /// Begin a one-off `(channel, route)` notification to an on-demand
    /// (`AnonymousNotifiable`) recipient. Returns an
    /// [`AnonymousNotifiable`] (or an error if the channel is `"database"`,
    /// which has no on-demand semantics). Send the notification with
    /// [`Notify::send`] / [`Notify::queue`] just like a model-backed
    /// recipient.
    ///
    /// ```ignore
    /// let recipient = Notify::route("mail", "ops@example.com")?;
    /// Notify::send(&recipient, &IncidentNotification { id: 7 }).await?;
    /// ```
    pub fn route(
        channel: impl Into<String>,
        route: impl Into<String>,
    ) -> Result<AnonymousNotifiable, FrameworkError> {
        AnonymousNotifiable::new().route(channel, route)
    }

    /// Like [`Notify::route`] but accepts multiple `(channel, route)` pairs
    /// in one call. The `"database"` channel is rejected verbatim, matching
    /// Laravel's `Notification::routes(...)` semantics.
    pub fn routes<I, C, R>(pairs: I) -> Result<AnonymousNotifiable, FrameworkError>
    where
        I: IntoIterator<Item = (C, R)>,
        C: Into<String>,
        R: Into<String>,
    {
        AnonymousNotifiable::new().routes(pairs)
    }
}
