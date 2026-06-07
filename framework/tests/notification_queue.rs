use async_trait::async_trait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use suprnova::notifications::channels::database::DatabaseChannel;
use suprnova::notifications::notify_job::SendNotificationJob;
use suprnova::notifications::{
    Channel, DynNotification, Notifiable, Notification, NotificationDispatcher,
};
use suprnova::queue::Queue;
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::queue::worker::{WorkerConfig, register_job, run_worker};
use suprnova::{FrameworkError, Notify};
use tokio_util::sync::CancellationToken;

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
    let _ = suprnova::notifications::set_dispatcher(Arc::new(dispatcher));

    let _ = suprnova::notifications::register_notification_factory::<OrderShipped>();
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
            max_jobs: None,
        },
        CancellationToken::new(),
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
    assert_eq!(row.try_get_by_index::<String>(0).unwrap(), "OrderShipped");
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
    let _ = suprnova::notifications::set_dispatcher(Arc::new(NotificationDispatcher::new()));

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

    let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(CountingChannel));
    let _ = suprnova::notifications::set_dispatcher(Arc::new(dispatcher));

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

// Multi-channel notification with two routed channels.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct DualChannelAlert {
    body: String,
}

impl Notification for DualChannelAlert {
    fn notification_name() -> &'static str {
        "DualChannelAlert"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["database", "mail"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "body": self.body })
    }
}

struct DualUser {
    id: i64,
    email: String,
}

impl Notifiable for DualUser {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "database" => Some(self.id.to_string()),
            "mail" => Some(self.email.clone()),
            _ => None,
        }
    }
}

// Regression: Notify::queue must push ONE SendNotificationJob per declared,
// routed channel. Before the fix, a single envelope carried the full
// channel list — so any per-channel failure restarted ALL channels on
// retry, causing the database channel to insert twice and the recipient
// to receive the same email twice.
#[tokio::test]
#[serial]
async fn notify_queue_pushes_one_envelope_per_routed_channel() {
    use suprnova::queue::driver::Reservation;

    // Dispatcher binding is needed only to satisfy register_notification_factory.
    let _ = suprnova::notifications::set_dispatcher(Arc::new(NotificationDispatcher::new()));
    let _ = suprnova::notifications::register_notification_factory::<DualChannelAlert>();
    register_job::<SendNotificationJob>();

    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    Notify::queue(
        &DualUser {
            id: 99,
            email: "x@example.org".into(),
        },
        DualChannelAlert {
            body: "ping".into(),
        },
    )
    .await
    .unwrap();

    // Drain the driver and count envelopes + assert each carries exactly
    // one channel.
    let mut popped: Vec<Reservation> = Vec::new();
    while let Some(r) = driver.pop(Duration::from_secs(1)).await.unwrap() {
        popped.push(r);
    }
    assert_eq!(
        popped.len(),
        2,
        "queue must hold one envelope per routed channel (database + mail = 2)",
    );
    for r in &popped {
        let job: SendNotificationJob = serde_json::from_value(r.envelope.payload.clone())
            .expect("payload decodes to SendNotificationJob");
        assert_eq!(
            job.channels.len(),
            1,
            "each envelope must carry exactly one channel for retry isolation",
        );
        assert_eq!(
            job.notifiable_route_per_channel.len(),
            1,
            "each envelope must carry exactly one route for its own channel",
        );
    }
}

// Regression: a recipient whose `route_for` returns None for a declared
// channel must not produce an envelope for that channel — matches the
// pre-fix behaviour where the handle path skipped unrouted channels.
#[tokio::test]
#[serial]
async fn notify_queue_skips_channels_with_no_route() {
    // Dispatcher binding is needed only to satisfy register_notification_factory.
    let _ = suprnova::notifications::set_dispatcher(Arc::new(NotificationDispatcher::new()));
    let _ = suprnova::notifications::register_notification_factory::<DualChannelAlert>();
    register_job::<SendNotificationJob>();

    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    // User resolves only the database channel; mail returns None.
    Notify::queue(
        &User { id: 99 },
        DualChannelAlert {
            body: "ping".into(),
        },
    )
    .await
    .unwrap();

    let r = driver
        .pop(Duration::from_secs(1))
        .await
        .unwrap()
        .expect("the database channel must produce an envelope");
    let job: SendNotificationJob =
        serde_json::from_value(r.envelope.payload.clone()).expect("decode");
    assert_eq!(job.channels, vec!["database".to_string()]);
    assert!(
        driver.pop(Duration::from_secs(1)).await.unwrap().is_none(),
        "no envelope for the mail channel (route_for returned None)",
    );
}
