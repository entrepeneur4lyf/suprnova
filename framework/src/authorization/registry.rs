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
        self.gates
            .write()
            .unwrap()
            .insert(key, GateEntry::Sync(erased));
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
        self.gates
            .write()
            .unwrap()
            .insert(key, GateEntry::Async(erased));
    }

    /// Invoke a sync gate. Returns `None` if not registered, `None` if the gate
    /// is async (caller must use `invoke_async`).
    pub(crate) fn invoke<U: 'static, R: 'static>(
        &self,
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<bool> {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        let gates = self.gates.read().unwrap();
        match gates.get(&key) {
            Some(GateEntry::Sync(f)) => Some(f(user as &dyn Any, resource as &dyn Any)),
            // Async gate called via sync path → default deny (documented behaviour).
            Some(GateEntry::Async(_)) => Some(false),
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
        let entry_result: EntryResult = {
            let gates = self.gates.read().unwrap();
            match gates.get(&key) {
                Some(GateEntry::Sync(f)) => Some(Ok(f(user as &dyn Any, resource as &dyn Any))),
                Some(GateEntry::Async(f)) => {
                    Some(Err(f(user as &dyn Any, resource as &dyn Any)))
                }
                None => None,
            }
        };

        match entry_result {
            None => None,
            Some(Ok(result)) => Some(result),
            Some(Err(fut)) => Some(fut.await),
        }
    }
}

pub(crate) fn global() -> &'static GateRegistry {
    static R: std::sync::OnceLock<GateRegistry> = std::sync::OnceLock::new();
    R.get_or_init(GateRegistry::new)
}
