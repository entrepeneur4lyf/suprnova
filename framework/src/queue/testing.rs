//! `Queue::fake()` — installs an in-memory recorder that captures
//! dispatched jobs without running them.
//!
//! `install_fake()` acquires a process-wide serialization mutex for the
//! lifetime of the returned `QueueFakeGuard`. This prevents parallel tests
//! from clobbering each other's fake store.
//!
//! Recorded pushes carry their `available_at` so tests can assert delayed
//! dispatch timestamps through [`pushed_with_available_at`] /
//! [`assert_pushed_later`] without leaving the fake surface.

use crate::error::FrameworkError;
use crate::queue::Job;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// One captured push: the serialized job payload + the `available_at` the
/// facade dispatched with. `Queue::push` records `Utc::now()`; the `*_later`
/// variants record the explicit timestamp.
#[derive(Clone)]
struct FakePush {
    payload: serde_json::Value,
    available_at: DateTime<Utc>,
}

#[derive(Default)]
struct FakeStore {
    pushed: HashMap<TypeId, Vec<FakePush>>,
}

/// Process-wide serializer: only one test may hold the fake at a time.
static FAKE_SERIAL: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);

fn lock_fake() -> std::sync::MutexGuard<'static, Option<FakeStore>> {
    FAKE.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn is_active() -> bool {
    lock_fake().is_some()
}

pub(crate) fn record<J: Job>(job: &J, available_at: DateTime<Utc>) -> Result<(), FrameworkError> {
    let payload =
        serde_json::to_value(job).map_err(|e| FrameworkError::internal(format!("encode: {e}")))?;
    let mut g = lock_fake();
    if let Some(store) = g.as_mut() {
        store
            .pushed
            .entry(TypeId::of::<J>())
            .or_default()
            .push(FakePush {
                payload,
                available_at,
            });
    }
    Ok(())
}

/// Install the queue fake for the current test.
///
/// The returned `QueueFakeGuard` holds a process-wide serialization lock,
/// preventing parallel tests from running simultaneously and interfering
/// with each other's store. It also clears the store on drop.
pub fn install_fake() -> QueueFakeGuard {
    let serial = FAKE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *lock_fake() = Some(FakeStore::default());
    QueueFakeGuard { _serial: serial }
}

pub struct QueueFakeGuard {
    _serial: MutexGuard<'static, ()>,
}

impl Drop for QueueFakeGuard {
    fn drop(&mut self) {
        // Use unwrap_or_else so a poisoned mutex from a test failure never
        // causes a double-panic (which would abort the process).
        *lock_fake() = None;
    }
}

pub fn assert_pushed<J: Job>(pred: impl Fn(&J) -> bool) {
    let g = lock_fake();
    let store = g.as_ref().expect("Queue::fake() must be active");
    let bucket = store.pushed.get(&TypeId::of::<J>());
    let count = bucket
        .map(|b| {
            b.iter()
                .filter_map(|p| serde_json::from_value::<J>(p.payload.clone()).ok())
                .filter(|j| pred(j))
                .count()
        })
        .unwrap_or(0);
    assert!(count > 0, "expected at least one pushed {}", J::job_name());
}

/// All captured pushes of `J` with their `available_at`. Use this in tests
/// that need to assert delayed-dispatch timestamps (e.g. that
/// `Queue::push_later(job, t)` recorded `t`, not `now`).
pub fn pushed_with_available_at<J: Job>() -> Vec<(J, DateTime<Utc>)> {
    let g = lock_fake();
    let store = g.as_ref().expect("Queue::fake() must be active");
    store
        .pushed
        .get(&TypeId::of::<J>())
        .map(|b| {
            b.iter()
                .filter_map(|p| {
                    serde_json::from_value::<J>(p.payload.clone())
                        .ok()
                        .map(|j| (j, p.available_at))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Like [`assert_pushed`] but receives `(job, available_at)` so tests can
/// pin the scheduled timestamp.
pub fn assert_pushed_later<J: Job>(pred: impl Fn(&J, DateTime<Utc>) -> bool) {
    let g = lock_fake();
    let store = g.as_ref().expect("Queue::fake() must be active");
    let count = store
        .pushed
        .get(&TypeId::of::<J>())
        .map(|b| {
            b.iter()
                .filter_map(|p| {
                    serde_json::from_value::<J>(p.payload.clone())
                        .ok()
                        .map(|j| (j, p.available_at))
                })
                .filter(|(j, t)| pred(j, *t))
                .count()
        })
        .unwrap_or(0);
    assert!(
        count > 0,
        "expected at least one pushed {} matching (job, available_at)",
        J::job_name()
    );
}

pub fn pushed<J: Job>() -> Vec<J> {
    let g = lock_fake();
    let store = g.as_ref().expect("Queue::fake() must be active");
    store
        .pushed
        .get(&TypeId::of::<J>())
        .map(|b| {
            b.iter()
                .filter_map(|p| serde_json::from_value::<J>(p.payload.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}
