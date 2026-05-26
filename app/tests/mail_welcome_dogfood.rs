//! Phase 5B Task 20 dogfood — pin that `Mail::queue(WelcomeEmail{..})`
//! pushes a `SendMailJob` onto the queue with the right envelope.
//!
//! Test exercises the `Mail::queue` plumbing directly. End-to-end
//! route -> handler -> queue -> worker -> transport coverage lands when
//! the full HTTP test harness comes online in Phase 6.
//!
//! Marked `#[serial]` because `install_fake` swaps the global queue
//! driver — running this concurrently with other queue tests would clobber
//! the capture buffer.

use serial_test::serial;
use suprnova::SendMailJob;
use suprnova::queue::testing::{assert_pushed, install_fake};

#[tokio::test]
#[serial]
async fn welcome_route_queues_welcome_mailable() {
    let _qg = install_fake();

    // Build the WelcomeEmail directly through Mail::queue from the app crate.
    app::mail::welcome::queue_welcome("alice@example.org", "Alice")
        .await
        .unwrap();

    // The Mail::queue path pushes a SendMailJob — the WelcomeEmail itself
    // lives inside `SendMailJob.mailable_payload` until the worker rehydrates
    // it. Assert on that envelope shape, not the mailable type directly.
    assert_pushed::<SendMailJob>(|job| {
        job.mailable_name == "WelcomeEmail"
            && job.to.iter().any(|a| a.email == "alice@example.org")
            && job.mailable_payload["name"] == "Alice"
    });
}
