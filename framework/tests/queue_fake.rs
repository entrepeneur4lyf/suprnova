use serde::{Deserialize, Serialize};
use suprnova::queue::testing::{assert_pushed, install_fake};
use suprnova::{FrameworkError, Job, Queue, async_trait};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Greet {
    name: String,
}

#[async_trait]
impl Job for Greet {
    fn job_name() -> &'static str {
        "Greet"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[tokio::test]
async fn queue_fake_captures_pushed_jobs_without_running_them() {
    let _guard = install_fake();
    Queue::push(Greet {
        name: "Lucas".into(),
    })
    .await
    .unwrap();
    Queue::push(Greet { name: "Ada".into() }).await.unwrap();
    assert_pushed::<Greet>(|g| g.name == "Lucas");
    assert_pushed::<Greet>(|g| g.name == "Ada");
}

#[tokio::test]
async fn queue_fake_isolates_per_test() {
    let _guard = install_fake();
    Queue::push(Greet {
        name: "Solo".into(),
    })
    .await
    .unwrap();
    assert_pushed::<Greet>(|g| g.name == "Solo");
}
