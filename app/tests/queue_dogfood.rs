use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::atomic::{AtomicI64, Ordering};
use suprnova::queue::testing::{assert_pushed, install_fake};
use suprnova::{FrameworkError, Job, Queue, async_trait};

static SEEN: AtomicI64 = AtomicI64::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WelcomeLog {
    pub user_id: i64,
}

#[async_trait]
impl Job for WelcomeLog {
    fn job_name() -> &'static str {
        "WelcomeLog"
    }

    async fn handle(self) -> Result<(), FrameworkError> {
        SEEN.fetch_add(self.user_id, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn dogfood_dispatches_welcome_log_under_fake() {
    SEEN.store(0, Ordering::SeqCst);
    let _guard = install_fake();
    Queue::push(WelcomeLog { user_id: 42 }).await.unwrap();
    assert_pushed::<WelcomeLog>(|w| w.user_id == 42);
}
