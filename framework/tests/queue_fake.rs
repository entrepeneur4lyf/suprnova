use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use suprnova::queue::testing::{
    assert_pushed, assert_pushed_later, install_fake, pushed_with_available_at,
};
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

#[tokio::test]
async fn queue_fake_records_available_at_for_delayed_pushes() {
    let _guard = install_fake();
    let now = Utc::now();
    let in_an_hour = now + ChronoDuration::hours(1);

    // Immediate push records ~now.
    Queue::push(Greet {
        name: "instant".into(),
    })
    .await
    .unwrap();

    // Delayed push must record the dispatched timestamp, not now.
    Queue::push_later(
        Greet {
            name: "scheduled".into(),
        },
        in_an_hour,
    )
    .await
    .unwrap();

    let entries = pushed_with_available_at::<Greet>();
    assert_eq!(entries.len(), 2, "two pushes captured");

    let scheduled = entries
        .iter()
        .find(|(g, _)| g.name == "scheduled")
        .expect("delayed push present");
    assert_eq!(
        scheduled.1, in_an_hour,
        "fake must record the dispatched available_at exactly"
    );

    let instant = entries
        .iter()
        .find(|(g, _)| g.name == "instant")
        .expect("instant push present");
    let drift = (instant.1 - now).num_milliseconds().abs();
    assert!(
        drift < 1_000,
        "Queue::push records ~now (drift {drift}ms must be small)"
    );

    // Predicate helper also exercises both fields.
    assert_pushed_later::<Greet>(|g, t| g.name == "scheduled" && t == in_an_hour);
}
