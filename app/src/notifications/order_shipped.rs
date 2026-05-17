//! OrderShipped notification — dogfood for the multi-channel
//! Notification subsystem (Phase 5B Tasks 16-19, polished by Item 2).
//!
//! Channels: `mail` AND `database`. The mail variant is produced by
//! `NotificationMailable::to_mail` (auto-deserialized from the
//! `Notification::data()` payload at dispatch time); the database
//! variant writes a row into the `notifications` table.
//!
//! Note: the `Notification` trait is sync — no `#[async_trait]` here.

use serde::{Deserialize, Serialize};
use suprnova::mail::Address;
use suprnova::notifications::channels::mail::{MailRendering, NotificationMailable};
use suprnova::notifications::Notification;
use suprnova::serde_json;
use suprnova::FrameworkError;

#[derive(Serialize, Deserialize, Debug, Clone)]
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

impl NotificationMailable for OrderShipped {
    fn to_mail(&self) -> Result<MailRendering, FrameworkError> {
        Ok(MailRendering {
            subject: format!("Your order shipped — tracking {}", self.tracking),
            html: Some(format!(
                "<p>Your order is on its way.</p><p>Tracking: <code>{}</code></p>",
                self.tracking
            )),
            text: Some(format!(
                "Your order is on its way.\nTracking: {}",
                self.tracking
            )),
            from: Some(Address::new("orders@suprnova.dev").with_name("Suprnova Orders")),
            ..Default::default()
        })
    }
}

