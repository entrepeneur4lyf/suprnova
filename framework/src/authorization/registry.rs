use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;

// A sync gate closure, type-erased.
type SyncGateFn = Box<dyn Fn(&dyn Any, &dyn Any) -> bool + Send + Sync>;

// An async gate closure, type-erased.
// The closure returns an owned, boxed future (no borrowed references in the
// output) — this sidesteps lifetime issues with `for<'a>` HRTBs on trait
// objects. Callers clone/copy the user and resource values into the closure.
type AsyncGateFn =
    Box<dyn Fn(&dyn Any, &dyn Any) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

enum GateEntry {
    Sync(SyncGateFn),
    Async(AsyncGateFn),
}

pub(crate) struct GateRegistry {
    gates: RwLock<HashMap<(String, TypeId, TypeId), GateEntry>>,
}

impl GateRegistry {
    pub(crate) fn new() -> Self {
        Self {
            gates: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn register<U: 'static, R: 'static>(
        &self,
        action: &str,
        f: impl Fn(&U, &R) -> bool + Send + Sync + 'static,
    ) {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        let erased: SyncGateFn = Box::new(move |u, r| {
            let u = u.downcast_ref::<U>().expect("gate user type");
            let r = r.downcast_ref::<R>().expect("gate resource type");
            f(u, r)
        });
        // Degrade gracefully on lock poison rather than panic the
        // boot path. Skipping a single gate registration is
        // recoverable; the gate's authorize calls return None →
        // safe-deny (`Err(FrameworkError::Unauthorized)`).
        match self.gates.write() {
            Ok(mut gates) => {
                gates.insert(key, GateEntry::Sync(erased));
            }
            Err(_) => {
                tracing::error!(
                    action = %action,
                    user_type = std::any::type_name::<U>(),
                    resource_type = std::any::type_name::<R>(),
                    "Gate registry lock poisoned; skipping sync gate registration. \
                     Calls to Gate::authorize for this action will safe-deny."
                );
            }
        }
    }

    /// Register an async gate.
    ///
    /// The closure must produce an *owned* future — it cannot borrow `user` or
    /// `resource` because the type-erased `&dyn Any` references cannot outlive
    /// the `GateRegistry`. Callers are expected to clone / copy any data they
    /// need inside the closure body before returning the future.
    pub(crate) fn register_async<U, R, F, Fut>(&self, action: &str, f: F)
    where
        U: 'static,
        R: 'static,
        F: Fn(&U, &R) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        let erased: AsyncGateFn = Box::new(move |u, r| {
            let u = u.downcast_ref::<U>().expect("gate user type");
            let r = r.downcast_ref::<R>().expect("gate resource type");
            let fut = f(u, r);
            Box::pin(fut)
        });
        // Same poison-recovery shape as `register` (sync) —
        // safe-deny on the next authorize attempt.
        match self.gates.write() {
            Ok(mut gates) => {
                gates.insert(key, GateEntry::Async(erased));
            }
            Err(_) => {
                tracing::error!(
                    action = %action,
                    user_type = std::any::type_name::<U>(),
                    resource_type = std::any::type_name::<R>(),
                    "Gate registry lock poisoned; skipping async gate registration. \
                     Calls to Gate::authorize_async for this action will safe-deny."
                );
            }
        }
    }

    /// Invoke a sync gate. Returns `None` if not registered, `Some(false)` if
    /// the gate is registered as async (caller must use `invoke_async`).
    ///
    /// Hitting the async-registered branch via the sync path is almost always
    /// a caller bug — silently denying would hide it, so we emit a
    /// `tracing::warn!` and the caller can spot it in logs.
    pub(crate) fn invoke<U: 'static, R: 'static>(
        &self,
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<bool> {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        // Every Gate::authorize call dispatches through here.
        // Returning None on poison means the caller's
        // `Gate::authorize` returns Err(Unauthorized) — safe-deny.
        let gates = match self.gates.read() {
            Ok(g) => g,
            Err(_) => {
                tracing::error!(
                    action = %action,
                    user_type = std::any::type_name::<U>(),
                    resource_type = std::any::type_name::<R>(),
                    "Gate registry lock poisoned during invoke; safe-denying \
                     (returning None so authorize maps to Err(Unauthorized))."
                );
                return None;
            }
        };
        match gates.get(&key) {
            Some(GateEntry::Sync(f)) => Some(f(user as &dyn Any, resource as &dyn Any)),
            Some(GateEntry::Async(_)) => {
                tracing::warn!(
                    action = %action,
                    user_type = std::any::type_name::<U>(),
                    resource_type = std::any::type_name::<R>(),
                    "Gate::allows/denies/authorize called on an async-registered gate — \
                     defaulting to deny. Use Gate::allows_async/denies_async/authorize_async instead.",
                );
                Some(false)
            }
            None => None,
        }
    }

    /// Invoke a gate asynchronously. Works for both sync- and async-registered
    /// gates. Returns `None` if the gate is not registered.
    pub(crate) async fn invoke_async<U: 'static, R: 'static>(
        &self,
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<bool> {
        type AsyncFut = Pin<Box<dyn Future<Output = bool> + Send>>;
        type EntryResult = Option<Result<bool, AsyncFut>>;

        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        // We hold the read lock only long enough to clone the result or start
        // the async dispatch — we must NOT hold it across an `.await`.
        //
        // Degrade to None on poison; caller's `Gate::authorize_async`
        // returns Err(Unauthorized) (safe-deny).
        let entry_result: EntryResult = match self.gates.read() {
            Ok(gates) => match gates.get(&key) {
                Some(GateEntry::Sync(f)) => Some(Ok(f(user as &dyn Any, resource as &dyn Any))),
                Some(GateEntry::Async(f)) => Some(Err(f(user as &dyn Any, resource as &dyn Any))),
                None => None,
            },
            Err(_) => {
                tracing::error!(
                    action = %action,
                    user_type = std::any::type_name::<U>(),
                    resource_type = std::any::type_name::<R>(),
                    "Gate registry lock poisoned during invoke_async; safe-denying."
                );
                None
            }
        };

        match entry_result {
            None => None,
            Some(Ok(result)) => Some(result),
            Some(Err(fut)) => Some(fut.await),
        }
    }

    /// Whether a gate (sync or async) is registered for
    /// `(action, U, R)`. Used by [`crate::Gate::has`] for the
    /// introspection path Laravel calls `Gate::has`.
    pub(crate) fn has<U: 'static, R: 'static>(&self, action: &str) -> bool {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        match self.gates.read() {
            Ok(gates) => gates.contains_key(&key),
            // Poisoned lock: same safe-deny posture as the invoke
            // paths — pretend the gate is absent rather than panic.
            Err(_) => false,
        }
    }

    /// Distinct action names across every registered (action, U, R)
    /// tuple. Used by [`crate::Gate::abilities`] — mirrors Laravel's
    /// `Gate::abilities()` (which also dedupes by action name).
    /// Returns an empty vec on lock poison (same safe-deny shape).
    pub(crate) fn abilities(&self) -> Vec<String> {
        let Ok(gates) = self.gates.read() else {
            tracing::error!(
                "Gate registry lock poisoned during abilities(); returning empty list."
            );
            return Vec::new();
        };
        let mut names: Vec<String> = gates.keys().map(|(a, _, _)| a.clone()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

pub(crate) fn global() -> &'static GateRegistry {
    static R: std::sync::OnceLock<GateRegistry> = std::sync::OnceLock::new();
    R.get_or_init(GateRegistry::new)
}
