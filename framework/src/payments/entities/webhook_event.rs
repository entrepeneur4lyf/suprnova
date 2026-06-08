//! SeaORM mirror entity for `payments_webhook_events`.
//!
//! Records every inbound provider webhook for idempotency and audit.
//! `provider_event_id` + `provider` uniquely identify an event — the
//! unique index on the table enforces this at the DB level.
//!
//! `processed_at` is `None` until the event is dispatched through the
//! neutral-event mapping layer. `process_error` records the last error
//! string when processing fails, for operator review.

use chrono::{DateTime, Utc};

/// Mirror row for an inbound provider webhook.
///
/// `(provider, provider_event_id)` is unique — the DB index doubles as the
/// idempotency guard for replay protection.
///
/// Note: no `timestamps` flag — this entity uses `received_at` /
/// `processed_at` instead of the standard `created_at` / `updated_at`
/// pair. The macro's auto-touch logic is not applied.
#[suprnova::model(table = "payments_webhook_events")]
pub struct WebhookEvent {
    /// Surrogate primary key.
    pub id: i64,
    /// Provider name (kebab-case — `"stripe"`, `"paddle"`, etc.).
    pub provider: String,
    /// Provider-issued event identifier (e.g. Stripe's `evt_…`). Unique
    /// with `provider`.
    pub provider_event_id: String,
    /// Provider's raw event-type string (e.g. `"payment_intent.succeeded"`).
    pub provider_event_type: String,
    /// Optional [`super::super::dto::NeutralEventKind`] mapping for events
    /// the framework can normalise; `None` when only the provider-specific
    /// type is meaningful.
    pub neutral_event_kind: Option<String>,
    /// Raw JSON payload as received from the provider — kept verbatim for
    /// audit and reprocessing.
    pub payload: serde_json::Value,
    /// Wall-clock time the framework received and persisted the event.
    pub received_at: DateTime<Utc>,
    /// Wall-clock time event dispatch succeeded. `None` until the event
    /// is successfully processed.
    pub processed_at: Option<DateTime<Utc>>,
    /// Last error string emitted while processing this event. `None` on
    /// success.
    pub process_error: Option<String>,
}

/// SeaORM `Model` re-exported from the inner macro-generated module.
pub use webhook_event::Model;
/// SeaORM `ActiveModel`, `Column`, and `Entity` from the inner module.
pub use webhook_event::{ActiveModel, Column, Entity};
