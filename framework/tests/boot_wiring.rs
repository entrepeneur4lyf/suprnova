use std::sync::Arc;
use std::time::Duration;
use suprnova::container::App;
use suprnova::queue::Queue;
use suprnova::rate_limit::{RateLimiter, SlidingWindowConfig};

use serde::{Deserialize, Serialize};
use suprnova::{FrameworkError, Job, async_trait};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct NoopJob;

#[async_trait]
impl Job for NoopJob {
    fn job_name() -> &'static str {
        "NoopJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[tokio::test]
#[serial_test::serial]
async fn defaults_bind_in_memory_queue_and_rate_limiter() {
    // Wipe any prior env vars so we exercise the default path.
    // SAFETY: #[serial] ensures no other thread reads these vars concurrently.
    unsafe {
        std::env::remove_var("QUEUE_DRIVER");
        std::env::remove_var("RATE_LIMIT_DRIVER");
    }

    suprnova::queue::bootstrap_default().await;
    suprnova::rate_limit::bootstrap_default().await;

    // Queue must accept a push without further config.
    Queue::push(NoopJob).await.unwrap();

    // Rate limiter must resolve through the container.
    let limiter: Arc<dyn RateLimiter> = App::resolve_make::<dyn RateLimiter>().unwrap();
    assert!(
        limiter
            .try_acquire(
                "k",
                &SlidingWindowConfig {
                    max_requests: 1,
                    window: Duration::from_secs(60),
                }
            )
            .await
            .unwrap()
    );
}
