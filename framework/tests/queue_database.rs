use chrono::Utc;
use sea_orm::{ConnectionTrait, Database};
use std::time::Duration;
use suprnova::queue::database::DatabaseQueueDriver;
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::{BackoffSchedule, CURRENT_SCHEMA_VERSION, Envelope};
use uuid::Uuid;

async fn fresh_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared(
        r"
        CREATE TABLE jobs (
            id TEXT PRIMARY KEY,
            job_name TEXT NOT NULL,
            envelope_json TEXT NOT NULL,
            available_at INTEGER NOT NULL,
            reserved_until INTEGER NULL,
            reserved_token TEXT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        )
    ",
    )
    .await
    .unwrap();
    db.execute_unprepared("CREATE INDEX idx_jobs_available_at ON jobs(available_at)")
        .await
        .unwrap();
    db
}

fn env(name: &str) -> Envelope {
    let now = Utc::now();
    Envelope {
        schema_version: CURRENT_SCHEMA_VERSION,
        id: Uuid::new_v4(),
        job_name: name.into(),
        payload: serde_json::json!({}),
        dispatched_at: now,
        available_at: now,
        attempts: 0,
        max_tries: 3,
        backoff: BackoffSchedule::default(),
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
    }
}

#[tokio::test]
async fn database_driver_push_and_ack() {
    let db = fresh_db().await;
    let d = DatabaseQueueDriver::new(db, "jobs".to_string()).unwrap();

    d.push(env("A")).await.unwrap();
    d.push(env("B")).await.unwrap();

    let r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    let r2 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(r1.envelope.job_name, "A");
    assert_eq!(r2.envelope.job_name, "B");

    d.ack(&r1.token).await.unwrap();
    d.ack(&r2.token).await.unwrap();

    let none = d.pop(Duration::from_millis(10)).await.unwrap();
    assert!(none.is_none(), "queue drained");
}

#[tokio::test]
async fn database_driver_reserved_rows_invisible_to_other_pops() {
    let db = fresh_db().await;
    let d = DatabaseQueueDriver::new(db, "jobs".to_string()).unwrap();

    d.push(env("A")).await.unwrap();

    let _r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    let r2 = d.pop(Duration::from_millis(10)).await.unwrap();
    assert!(r2.is_none(), "row reserved by r1 must not be popped again");
}

#[tokio::test]
async fn database_driver_nack_bumps_attempts() {
    let db = fresh_db().await;
    let d = DatabaseQueueDriver::new(db, "jobs".to_string()).unwrap();

    d.push(env("A")).await.unwrap();

    let r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(r1.envelope.attempts, 0);

    d.nack(&r1.token, Duration::from_millis(0)).await.unwrap();

    let r2 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(
        r2.envelope.attempts, 1,
        "nack must bump attempts (per trait contract)"
    );
}

#[tokio::test]
async fn database_driver_rejects_invalid_table_identifier() {
    let db = fresh_db().await;
    for bad in [
        "",
        "jobs; DROP TABLE users",
        "jobs--",
        "jobs'",
        "jobs/*",
        "1jobs",
        "jobs jobs",
    ] {
        let err = match DatabaseQueueDriver::new(db.clone(), bad.into()) {
            Err(e) => e,
            Ok(_) => panic!("expected validation error for {bad:?}, got Ok"),
        };
        assert!(
            err.to_string().to_lowercase().contains("identifier"),
            "expected an identifier-validation error for {bad:?}, got: {err}"
        );
    }
}
