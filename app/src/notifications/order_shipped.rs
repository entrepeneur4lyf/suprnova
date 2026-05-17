//! OrderShipped notification — dogfood for Phase 5B Task 20.
//!
//! Channels: `database` only for v1. The mail channel needs a
//! per-notification `MailRendering` factory; the future `POST /api/orders`
//! flow (Phase 11/12) will wire that — for now the dogfood keeps the
//! recipe minimal so the Notification trait, factory registration, and
//! `SendNotificationJob` re-hydration are pinned without dragging in the
//! mail-channel rendering surface.
//!
//! Note: the `Notification` trait is sync — no `#[async_trait]` here.

use serde::{Deserialize, Serialize};
use suprnova::notifications::Notification;
use suprnova::serde_json;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrderShipped {
    pub tracking: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str {
        "OrderShipped"
    }

    fn channels(&self) -> Vec<&'static str> {
        vec!["database"]
    }

    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}
