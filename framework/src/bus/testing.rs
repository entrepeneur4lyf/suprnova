//! `Bus::fake()` — installs a capture-only recorder.

use crate::bus::command::Command;
use crate::error::FrameworkError;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct FakeStore {
    dispatched: HashMap<TypeId, Vec<serde_json::Value>>,
}

static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);

pub(crate) fn is_active() -> bool {
    FAKE.lock().unwrap().is_some()
}

pub(crate) fn record<C: Command>(cmd: &C) -> Result<(), FrameworkError> {
    let payload = serde_json::to_value(cmd)
        .map_err(|e| FrameworkError::internal(format!("bus encode: {e}")))?;
    let mut g = FAKE.lock().unwrap();
    if let Some(s) = g.as_mut() {
        s.dispatched
            .entry(TypeId::of::<C>())
            .or_default()
            .push(payload);
    }
    Ok(())
}

/// Install a fake Bus that captures dispatched commands instead of running handlers.
///
/// Returns a guard that removes the fake when dropped. Use with `let _guard = install_fake();`
/// inside a `#[serial]` test to avoid races with other tests.
pub fn install_fake() -> BusFakeGuard {
    *FAKE.lock().unwrap() = Some(FakeStore::default());
    BusFakeGuard
}

/// RAII guard returned by [`install_fake`]. Resets the fake on drop.
pub struct BusFakeGuard;

impl Drop for BusFakeGuard {
    fn drop(&mut self) {
        // Tolerate poisoning: if a test assertion panicked while holding
        // the lock, this drop still needs to clear the fake so the next
        // test starts fresh. Reading through the poison avoids a
        // panic-in-destructor abort that would mask the original failure.
        let mut g = FAKE.lock().unwrap_or_else(|p| p.into_inner());
        *g = None;
    }
}

/// Count dispatched commands of type `C` matching `pred`. Shared core for the
/// assertion helpers below. Panics if the fake is not active.
fn count_matching<C: Command>(pred: &impl Fn(&C) -> bool) -> usize {
    let g = FAKE.lock().unwrap();
    let store = g.as_ref().expect("Bus::fake() must be active");
    store
        .dispatched
        .get(&TypeId::of::<C>())
        .map(|b| {
            b.iter()
                .filter_map(|p| serde_json::from_value::<C>(p.clone()).ok())
                .filter(|c| pred(c))
                .count()
        })
        .unwrap_or(0)
}

/// Assert that at least one command of type `C` was dispatched matching `pred`.
///
/// Panics if the fake is not active or no matching command was found.
pub fn assert_dispatched<C: Command>(pred: impl Fn(&C) -> bool) {
    let count = count_matching::<C>(&pred);
    assert!(
        count > 0,
        "expected at least one dispatched {}",
        C::command_name()
    );
}

/// Assert that NO command of type `C` matching `pred` was dispatched.
///
/// Panics if the fake is not active or a matching command was found.
pub fn assert_not_dispatched<C: Command>(pred: impl Fn(&C) -> bool) {
    let count = count_matching::<C>(&pred);
    assert_eq!(
        count,
        0,
        "expected no dispatched {} but found {count}",
        C::command_name()
    );
}

/// Assert that EXACTLY `expected` commands of type `C` matching `pred` were
/// dispatched.
///
/// Panics if the fake is not active or the count does not match.
pub fn assert_dispatched_times<C: Command>(pred: impl Fn(&C) -> bool, expected: usize) {
    let actual = count_matching::<C>(&pred);
    assert_eq!(
        actual,
        expected,
        "expected {expected} dispatched {} but found {actual}",
        C::command_name()
    );
}

/// Assert that NO commands of any type were dispatched under the active fake.
///
/// Panics if the fake is not active or any command was dispatched.
pub fn assert_nothing_dispatched() {
    let total: usize = {
        let g = FAKE.lock().unwrap();
        let store = g.as_ref().expect("Bus::fake() must be active");
        store.dispatched.values().map(Vec::len).sum()
    };
    assert_eq!(
        total, 0,
        "expected no dispatched commands but found {total}"
    );
}
