//! OrderShipped notification — dogfood for the multi-channel
//! Notification subsystem (Phase 5B Tasks 16-19, polished by Item 2)
//! and for the v2 `#[derive(NotificationMailable)]` macro.
//!
//! Channels: `mail` AND `database`. The mail variant is produced by
//! `NotificationMailable::to_mail` — generated here by the derive from
//! the `#[mail(...)]` attribute. The database variant writes a row
//! into the `notifications` table.
//!
//! Switching from a hand-written `impl NotificationMailable` to the
//! derive is intentional: the multi-channel integration test in
//! `app/tests/notification_order_shipped_multi_channel.rs` continues
//! to pass byte-for-byte, proving the derive's generated rendering
//! matches the previous manual impl.

use serde::{Deserialize, Serialize};
use suprnova::serde_json;
use suprnova::NotificationMailable;
use suprnova::Notification;

#[derive(Serialize, Deserialize, Debug, Clone, NotificationMailable)]
#[mail(
    subject = "Your order shipped — tracking {{ tracking }}",
    html = "<p>Your order is on its way.</p><p>Tracking: <code>{{ tracking }}</code></p>",
    text = "Your order is on its way.\nTracking: {{ tracking }}",
    from = "orders@suprnova.dev",
    from_name = "Suprnova Orders",
)]
pub struct OrderShipped {
    pub tracking: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str {
        "OrderShipped"
    }

    fn channels(&self) -> Vec<&'static str> {
        vec!["mail", "database"]
    }

    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}
