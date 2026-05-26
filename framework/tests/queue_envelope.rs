use chrono::{TimeZone, Utc};
use suprnova::queue::envelope::{Envelope, EnvelopeError};
use suprnova::queue::job::BackoffSchedule;
use uuid::Uuid;

#[test]
fn envelope_round_trips_through_json() {
    let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let when = Utc.with_ymd_and_hms(2026, 5, 16, 12, 34, 56).unwrap();

    let env = Envelope {
        schema_version: 1,
        id,
        job_name: "SendWelcomeEmail".into(),
        payload: serde_json::json!({ "user_id": 42 }),
        dispatched_at: when,
        available_at: when,
        attempts: 0,
        max_tries: 3,
        backoff: BackoffSchedule::Exponential {
            base_secs: 2,
            cap_secs: 300,
            jitter_ratio: 0.25,
        },
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
    };

    let json = serde_json::to_string(&env).unwrap();
    let parsed: Envelope = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.schema_version, 1);
    assert_eq!(parsed.id, id);
    assert_eq!(parsed.job_name, "SendWelcomeEmail");
    assert_eq!(parsed.payload["user_id"], 42);
    assert_eq!(parsed.dispatched_at, when);
    assert_eq!(parsed.attempts, 0);
    assert_eq!(parsed.max_tries, 3);
}

#[test]
fn envelope_rejects_unknown_schema_version() {
    let json = r#"{"schema_version":2,"id":"550e8400-e29b-41d4-a716-446655440000","job_name":"X","payload":{},"dispatched_at":"2026-05-16T12:34:56Z","available_at":"2026-05-16T12:34:56Z","attempts":0,"max_tries":3,"backoff":{"kind":"exponential","base_secs":2,"cap_secs":300,"jitter_ratio":0.25},"timeout_secs":null,"fail_on_timeout":false,"idempotency_key":null}"#;
    let err = Envelope::from_json(json).unwrap_err();
    assert!(matches!(err, EnvelopeError::UnsupportedSchemaVersion(2)));
}

#[test]
fn envelope_rejects_malformed_json() {
    let err = Envelope::from_json("not json").unwrap_err();
    assert!(matches!(err, EnvelopeError::Decode(_)));
}

#[test]
fn envelope_wire_format_is_frozen() {
    // FROZEN v1 layout — every field, every name, every type.
    // Any test failure here means we either need a schema_version bump
    // OR a revert. Never silently update the expected string.
    let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let when = Utc.with_ymd_and_hms(2026, 5, 16, 12, 34, 56).unwrap();
    let env = Envelope {
        schema_version: 1,
        id,
        job_name: "Frozen".into(),
        payload: serde_json::json!({ "k": "v" }),
        dispatched_at: when,
        available_at: when,
        attempts: 0,
        max_tries: 3,
        backoff: BackoffSchedule::Exponential {
            base_secs: 2,
            cap_secs: 300,
            jitter_ratio: 0.25,
        },
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
    };
    let json = serde_json::to_string(&env).unwrap();
    let expected = r#"{"schema_version":1,"id":"550e8400-e29b-41d4-a716-446655440000","job_name":"Frozen","payload":{"k":"v"},"dispatched_at":"2026-05-16T12:34:56Z","available_at":"2026-05-16T12:34:56Z","attempts":0,"max_tries":3,"backoff":{"kind":"exponential","base_secs":2,"cap_secs":300,"jitter_ratio":0.25},"timeout_secs":null,"fail_on_timeout":false,"idempotency_key":null}"#;
    assert_eq!(
        json, expected,
        "FROZEN v1 envelope wire format must not drift"
    );
}
