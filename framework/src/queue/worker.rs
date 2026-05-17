//! Worker registry + dispatch by job_name.
//!
//! Each `Job` impl registers a deserialize-and-run shim keyed by its
//! `job_name`. Drivers call `dispatch_by_name` to run an inbound payload.
//! Re-registering the same name is allowed (last writer wins) — useful
//! for tests; deterministic in production because each Job has exactly
//! one registration site.

use crate::error::FrameworkError;
use crate::queue::Job;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

type Dispatcher = Arc<dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<(), FrameworkError>> + Send + Sync>;

static REGISTRY: RwLock<Option<HashMap<String, Dispatcher>>> = RwLock::new(None);

pub fn register_job<J: Job>() {
    let f: Dispatcher = Arc::new(|payload: serde_json::Value| {
        Box::pin(async move {
            let job: J = serde_json::from_value(payload)
                .map_err(|e| FrameworkError::internal(format!("decode job: {e}")))?;
            job.handle().await
        })
    });
    let mut g = REGISTRY.write().expect("queue registry poisoned");
    g.get_or_insert_with(HashMap::new).insert(J::job_name().to_string(), f);
}

pub async fn dispatch_by_name(name: &str, payload: serde_json::Value) -> Result<(), FrameworkError> {
    let dispatcher = {
        let g = REGISTRY.read().expect("queue registry poisoned");
        let map = g.as_ref()
            .ok_or_else(|| FrameworkError::internal(format!("unknown job: {name}")))?;
        map.get(name)
            .cloned()
            .ok_or_else(|| FrameworkError::internal(format!("unknown job: {name}")))?
    };
    dispatcher(payload).await
}

/// Return all registered job names. Used by admin inspectors and
/// `cargo run --bin app -- jobs:list` (Phase 6B).
pub fn registered_job_names() -> Vec<String> {
    REGISTRY.read().expect("queue registry poisoned")
        .as_ref()
        .map(|m| { let mut v: Vec<_> = m.keys().cloned().collect(); v.sort(); v })
        .unwrap_or_default()
}
