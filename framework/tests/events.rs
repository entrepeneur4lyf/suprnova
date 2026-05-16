//! Integration tests for the events subsystem and error→event bridge.

use suprnova::{ErrorOccurred, EventFacade, FrameworkError, HttpResponse};
use tokio::sync::Mutex;

// Tests in this file share the global event fake store, so they must
// run serially. Use `tokio::sync::Mutex` so the guard can be safely
// held across `.await` points.
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn server_error_dispatches_error_occurred() {
    let _serial = TEST_LOCK.lock().await;
    let _guard = EventFacade::fake();

    let err = FrameworkError::internal("boom");
    let _resp: HttpResponse = err.into();

    // The dispatch is spawned; yield + small sleep to let it land.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    suprnova::events::testing::assert_dispatched::<ErrorOccurred>(|e| {
        e.status_code == 500 && e.error_message.contains("boom")
    });
}

#[tokio::test]
async fn client_error_does_not_dispatch_error_occurred() {
    let _serial = TEST_LOCK.lock().await;
    let _guard = EventFacade::fake();

    let err = FrameworkError::param("name");
    let _resp: HttpResponse = err.into();

    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    suprnova::events::testing::assert_not_dispatched::<ErrorOccurred>(|_| true);
}
