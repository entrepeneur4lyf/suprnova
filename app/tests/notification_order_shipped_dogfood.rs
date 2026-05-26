//! Phase 5B Task 20 dogfood — pin that `Notify::queue(...)` pushes a
//! `SendNotificationJob` for the registered `OrderShipped` notification.
//!
//! Symmetric to `mail_welcome_dogfood.rs` — proves the
//! `register_notification_factory` + `Notify::queue` wiring for the
//! `OrderShipped` type registered in `app/src/bootstrap.rs`. The factory
//! itself runs at worker-dispatch time; this test pins the queue-push
//! envelope so a future regression that drops the dispatch path
//! (renamed `notification_name`, dropped serde fields, missing channel
//! list) surfaces as a failing test rather than a silent dead bootstrap
//! call.
//!
//! Marked `#[serial]` because `install_fake` swaps the global queue
//! driver.

use serial_test::serial;
use suprnova::SendNotificationJob;
use suprnova::notifications::{Notifiable, Notify};
use suprnova::queue::testing::{assert_pushed, install_fake};

struct FakeUser;

impl Notifiable for FakeUser {
    fn route_for(&self, channel: &str) -> Option<String> {
        if channel == "database" {
            Some("user-42".into())
        } else {
            None
        }
    }
}

#[tokio::test]
#[serial]
async fn order_shipped_queues_send_notification_job() {
    let _qg = install_fake();

    Notify::queue(
        &FakeUser,
        app::notifications::order_shipped::OrderShipped {
            tracking: "1Z999AA10123456784".into(),
        },
    )
    .await
    .unwrap();

    assert_pushed::<SendNotificationJob>(|job| {
        job.notification_name == "OrderShipped"
            && job.channels.iter().any(|c| c == "database")
            && job
                .notifiable_route_per_channel
                .get("database")
                .map(String::as_str)
                == Some("user-42")
            && job.notification_payload["tracking"] == "1Z999AA10123456784"
    });
}
