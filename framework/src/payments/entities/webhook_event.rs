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

/// Note: no `timestamps` flag — this entity uses `received_at` /
/// `processed_at` instead of the standard `created_at` / `updated_at`
/// pair. The macro's auto-touch logic is not applied.
#[suprnova::model(table = "payments_webhook_events")]
pub struct WebhookEvent {
    pub id: i64,
    pub provider: String,
    pub provider_event_id: String,
    pub provider_event_type: String,
    pub neutral_event_kind: Option<String>,
    pub payload: serde_json::Value,
    pub received_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    pub process_error: Option<String>,
}

pub use webhook_event::{ActiveModel, Column, Entity};
pub use webhook_event::Model;
