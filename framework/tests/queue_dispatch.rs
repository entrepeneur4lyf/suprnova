use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, Ordering};
use suprnova::queue::worker::{dispatch_by_name, register_job};
use suprnova::{FrameworkError, Job, async_trait};

static TOTAL: AtomicI64 = AtomicI64::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Add {
    a: i64,
    b: i64,
}

#[async_trait]
impl Job for Add {
    fn job_name() -> &'static str {
        "Add"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        TOTAL.fetch_add(self.a + self.b, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn registered_jobs_can_be_dispatched_by_name() {
    TOTAL.store(0, Ordering::SeqCst);
    register_job::<Add>();
    dispatch_by_name("Add", serde_json::json!({ "a": 5, "b": 7 }))
        .await
        .unwrap();
    assert_eq!(TOTAL.load(Ordering::SeqCst), 12);
}

#[tokio::test]
async fn dispatch_unknown_job_returns_error() {
    let err = dispatch_by_name("DoesNotExist", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown job"));
}

#[tokio::test]
async fn dispatch_malformed_payload_returns_error() {
    register_job::<Add>();
    let err = dispatch_by_name("Add", serde_json::json!({ "a": "not a number" }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("decode job"));
}
