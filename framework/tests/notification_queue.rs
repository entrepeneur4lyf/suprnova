use async_trait::async_trait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use suprnova::notifications::channels::database::DatabaseChannel;
use suprnova::notifications::notify_job::SendNotificationJob;
use suprnova::notifications::{
    Channel, DynNotification, Notifiable, Notification, NotificationDispatcher,
};
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::queue::worker::{register_job, run_worker, WorkerConfig};
use suprnova::queue::Queue;
use suprnova::{FrameworkError, Notify};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OrderShipped {
    tracking: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str {
        "OrderShipped"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["database"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}

struct User {
    id: i64,
}

impl Notifiable for User {
    fn route_for(&self, channel: &str) -> Option<String> {
        if channel == "database" {
            Some(self.id.to_string())
        } else {
            None
        }
    }
}

async fn fresh_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared(
        r"
        CREATE TABLE notifications (
            id CHAR(36) PRIMARY KEY,
            type VARCHAR(255) NOT NULL,
            notifiable_type VARCHAR(255) NOT NULL,
            notifiable_id VARCHAR(64) NOT NULL,
            data TEXT NOT NULL,
            read_at TIMESTAMP NULL,
            created_at TIMESTAMP NOT NULL,
            updated_at TIMESTAMP NOT NULL
        )
        ",
    )
    .await
    .unwrap();
    db
}

#[tokio::test]
#[serial]
async fn notification_queue_dispatches_through_queue_and_lands_in_db() {
    let db = fresh_db().await;
    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(DatabaseChannel::new(db.clone(), "users")));
    suprnova::notifications::set_dispatcher(Arc::new(dispatcher));

    suprnova::notifications::register_notification_factory::<OrderShipped>(|payload| {
        let n: OrderShipped = serde_json::from_value(payload).map_err(|e| {
            suprnova::FrameworkError::internal(format!("decode OrderShipped: {e}"))
        })?;
        Ok(Box::new(n))
    });
    register_job::<SendNotificationJob>();

    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    Notify::queue(
        &User { id: 7 },
        OrderShipped {
            tracking: "1Z".into(),
        },
    )
    .await
    .unwrap();

    let handle = tokio::spawn(run_worker(
        driver.clone(),
        WorkerConfig {
            visibility_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(5),
        },
    ));

    for _ in 0..200 {
        let row = db
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT COUNT(*) FROM notifications".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let n: i64 = row.try_get_by_index(0).unwrap();
        if n > 0 {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.abort();

    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT type, notifiable_type, notifiable_id, data FROM notifications".to_string(),
        ))
        .await
        .unwrap()
        .expect("row present");
    assert_eq!(
        row.try_get_by_index::<String>(0).unwrap(),
        "OrderShipped"
    );
    assert_eq!(row.try_get_by_index::<String>(1).unwrap(), "users");
    assert_eq!(row.try_get_by_index::<String>(2).unwrap(), "7");
    let data_json: String = row.try_get_by_index(3).unwrap();
    let data: serde_json::Value = serde_json::from_str(&data_json).unwrap();
    assert_eq!(data["tracking"], "1Z");
}

#[tokio::test]
#[serial]
async fn notification_queue_unregistered_notification_surfaces_unknown_error_from_job() {
    // If a Notification's factory isn't registered, SendNotificationJob's
    // handle path returns `unknown notification: {name}` from the
    // registry lookup. This protects against silent retry loops on a
    // typo'd notification_name. We invoke handle directly rather than
    // round-tripping through the worker so the assertion is targeted at
    // the registry lookup, not at end-to-end retry/dead-letter behavior.
    use std::collections::HashMap;
    use suprnova::queue::Job;

    // The dispatcher binding is required by handle() before the factory
    // lookup runs — bind a minimal one so the assertion targets the
    // factory error, not the missing-dispatcher error.
    suprnova::notifications::set_dispatcher(Arc::new(NotificationDispatcher::new()));

    let job = SendNotificationJob {
        notifiable_route_per_channel: HashMap::new(),
        notification_name: "TotallyUnregisteredNotification".to_string(),
        notification_payload: serde_json::json!({}),
        channels: vec![],
    };
    let err = job.handle().await.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("unknown notification"),
        "error names the missing registry entry: {msg}"
    );
    assert!(
        msg.contains("TotallyUnregisteredNotification"),
        "error names the missing notification: {msg}"
    );
}

static SEND_HITS: AtomicU32 = AtomicU32::new(0);

struct CountingChannel;

#[async_trait]
impl Channel for CountingChannel {
    fn name(&self) -> &'static str {
        "database"
    }
    async fn deliver(
        &self,
        _route: &str,
        _notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        SEND_HITS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn notify_send_delivers_synchronously_through_bound_dispatcher() {
    // Notify::send is the sync sibling of Notify::queue — it must forward
    // to the bound dispatcher in-process with no queue round-trip.
    SEND_HITS.store(0, Ordering::SeqCst);

    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(CountingChannel));
    suprnova::notifications::set_dispatcher(Arc::new(dispatcher));

    Notify::send(
        &User { id: 42 },
        &OrderShipped {
            tracking: "SYNC-1".into(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        SEND_HITS.load(Ordering::SeqCst),
        1,
        "Notify::send must invoke the bound dispatcher exactly once"
    );
}
