use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use super::Response;

// A sync gate closure, type-erased. Returns a rich `Response` — bool gates are
// wrapped into a bare allow/deny at registration time.
type SyncGateFn = Box<dyn Fn(&dyn Any, &dyn Any) -> Response + Send + Sync>;

// An async gate closure, type-erased.
// The closure returns an owned, boxed future (no borrowed references in the
// output) — this sidesteps lifetime issues with `for<'a>` HRTBs on trait
// objects. Callers clone/copy the user and resource values into the closure.
type AsyncGateFn =
    Box<dyn Fn(&dyn Any, &dyn Any) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

// A before-hook: receives the (type-erased) user + the action name, returns
// `Some(decision)` to short-circuit the gate or `None` to continue. Keyed by
// the user's `TypeId` (a hook is about the *user's* global privileges, so it is
// resource-agnostic — put resource-specific logic in the gate itself).
type BeforeFn = dyn Fn(&dyn Any, &str) -> Option<bool> + Send + Sync;

// An after-hook: receives the user, the action name, and the running decision
// (`None` while still undecided). Mirrors Laravel's `??=` semantic — an after
// hook can only *fill in* an undecided result, never override an existing one.
// All after hooks still run (so they can log) regardless of the result.
type AfterFn = dyn Fn(&dyn Any, &str, Option<bool>) -> Option<bool> + Send + Sync;

enum GateEntry {
    Sync(SyncGateFn),
    Async(AsyncGateFn),
}

pub(crate) struct GateRegistry {
    gates: RwLock<HashMap<(String, TypeId, TypeId), GateEntry>>,
    // before/after hooks are stored behind `Arc` so the evaluation path can
    // clone the hook list out under a short read lock and invoke the user
    // closures *outside* the lock — a before hook that itself calls
    // `Gate::allows` re-enters this registry, and holding the read lock across
    // that nested call could deadlock a non-reentrant `RwLock`.
    before: RwLock<HashMap<TypeId, Vec<Arc<BeforeFn>>>>,
    after: RwLock<HashMap<TypeId, Vec<Arc<AfterFn>>>>,
}

impl GateRegistry {
    pub(crate) fn new() -> Self {
        Self {
            gates: RwLock::new(HashMap::new()),
            before: RwLock::new(HashMap::new()),
            after: RwLock::new(HashMap::new()),
        }
    }

    // ── Gate registration ────────────────────────────────────────────────────

    pub(crate) fn register<U: 'static, R: 'static>(
        &self,
        action: &str,
        f: impl Fn(&U, &R) -> bool + Send + Sync + 'static,
    ) {
        let erased: SyncGateFn = Box::new(move |u, r| {
            let u = u.downcast_ref::<U>().expect("gate user type");
            let r = r.downcast_ref::<R>().expect("gate resource type");
            Response::from(f(u, r))
        });
        self.insert_gate::<U, R>(action, GateEntry::Sync(erased), "sync");
    }

    /// Register a sync gate whose closure returns a rich [`Response`] directly
    /// (so denials can carry a message / status). See [`register`](Self::register)
    /// for the bool form.
    pub(crate) fn register_with<U: 'static, R: 'static>(
        &self,
        action: &str,
        f: impl Fn(&U, &R) -> Response + Send + Sync + 'static,
    ) {
        let erased: SyncGateFn = Box::new(move |u, r| {
            let u = u.downcast_ref::<U>().expect("gate user type");
            let r = r.downcast_ref::<R>().expect("gate resource type");
            f(u, r)
        });
        self.insert_gate::<U, R>(action, GateEntry::Sync(erased), "sync(response)");
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
        let erased: AsyncGateFn = Box::new(move |u, r| {
            let u = u.downcast_ref::<U>().expect("gate user type");
            let r = r.downcast_ref::<R>().expect("gate resource type");
            let fut = f(u, r);
            Box::pin(async move { Response::from(fut.await) })
        });
        self.insert_gate::<U, R>(action, GateEntry::Async(erased), "async");
    }

    /// Register an async gate whose future resolves to a rich [`Response`].
    pub(crate) fn register_async_with<U, R, F, Fut>(&self, action: &str, f: F)
    where
        U: 'static,
        R: 'static,
        F: Fn(&U, &R) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let erased: AsyncGateFn = Box::new(move |u, r| {
            let u = u.downcast_ref::<U>().expect("gate user type");
            let r = r.downcast_ref::<R>().expect("gate resource type");
            let fut = f(u, r);
            Box::pin(fut)
        });
        self.insert_gate::<U, R>(action, GateEntry::Async(erased), "async(response)");
    }

    // Insert a gate entry, degrading gracefully on lock poison rather than
    // panicking the boot path. Skipping a single registration is recoverable:
    // the gate's authorize calls return None → safe-deny.
    fn insert_gate<U: 'static, R: 'static>(&self, action: &str, entry: GateEntry, kind: &str) {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        match self.gates.write() {
            Ok(mut gates) => {
                gates.insert(key, entry);
            }
            Err(_) => {
                tracing::error!(
                    action = %action,
                    kind = %kind,
                    user_type = std::any::type_name::<U>(),
                    resource_type = std::any::type_name::<R>(),
                    "Gate registry lock poisoned; skipping gate registration. \
                     Calls to Gate::authorize for this action will safe-deny."
                );
            }
        }
    }

    // ── before / after hooks ───────────────────────────────────────────────

    /// Register a before-hook keyed by the user type `U`. Runs before any gate
    /// for an `allows`/`inspect` on that user type; first `Some` short-circuits.
    pub(crate) fn register_before<U: 'static>(
        &self,
        f: impl Fn(&U, &str) -> Option<bool> + Send + Sync + 'static,
    ) {
        let erased: Arc<BeforeFn> = Arc::new(move |u: &dyn Any, action: &str| {
            let u = u.downcast_ref::<U>().expect("before-hook user type");
            f(u, action)
        });
        match self.before.write() {
            Ok(mut map) => map.entry(TypeId::of::<U>()).or_default().push(erased),
            Err(_) => tracing::error!(
                user_type = std::any::type_name::<U>(),
                "before-hook registry poisoned; skipping registration."
            ),
        }
    }

    /// Register an after-hook keyed by the user type `U`. Runs after the gate
    /// on an `allows`/`inspect`; can only fill an undecided result.
    pub(crate) fn register_after<U: 'static>(
        &self,
        f: impl Fn(&U, &str, Option<bool>) -> Option<bool> + Send + Sync + 'static,
    ) {
        let erased: Arc<AfterFn> =
            Arc::new(move |u: &dyn Any, action: &str, current: Option<bool>| {
                let u = u.downcast_ref::<U>().expect("after-hook user type");
                f(u, action, current)
            });
        match self.after.write() {
            Ok(mut map) => map.entry(TypeId::of::<U>()).or_default().push(erased),
            Err(_) => tracing::error!(
                user_type = std::any::type_name::<U>(),
                "after-hook registry poisoned; skipping registration."
            ),
        }
    }

    // Clone the hook list for a user type out from under a short read lock so
    // the closures can be invoked without holding the lock (see `before`/`after`
    // field docs for the re-entrancy reasoning). Poison → empty (safe-skip).
    fn before_hooks(&self, tid: TypeId) -> Vec<Arc<BeforeFn>> {
        match self.before.read() {
            Ok(map) => map.get(&tid).cloned().unwrap_or_default(),
            Err(_) => {
                tracing::error!("before-hook registry poisoned during read; skipping hooks.");
                Vec::new()
            }
        }
    }

    fn after_hooks(&self, tid: TypeId) -> Vec<Arc<AfterFn>> {
        match self.after.read() {
            Ok(map) => map.get(&tid).cloned().unwrap_or_default(),
            Err(_) => {
                tracing::error!("after-hook registry poisoned during read; skipping hooks.");
                Vec::new()
            }
        }
    }

    // ── Gate invocation ──────────────────────────────────────────────────────

    /// Invoke a sync gate. Returns `None` if no gate is registered for the
    /// `(action, U, R)` tuple, `Some(deny)` if the gate is registered as async
    /// (caller must use `invoke_async`).
    ///
    /// Hitting the async-registered branch via the sync path is almost always
    /// a caller bug — silently denying would hide it, so we emit a
    /// `tracing::warn!` and the caller can spot it in logs.
    pub(crate) fn invoke<U: 'static, R: 'static>(
        &self,
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<Response> {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
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
                Some(Response::deny())
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
    ) -> Option<Response> {
        type AsyncFut = Pin<Box<dyn Future<Output = Response> + Send>>;
        type EntryResult = Option<Result<Response, AsyncFut>>;

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

    // ── Full evaluation: before → gate → after ──────────────────────────────

    /// Evaluate `(action, U, R)` through the full Laravel pipeline: before
    /// hooks (first `Some` wins), then the gate, then after hooks (fill-only).
    /// Returns `None` when nothing decided (no before hook, no gate, no after
    /// hook) — callers normalize that to a default deny.
    pub(crate) fn raw<U: 'static, R: 'static>(
        &self,
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<Response> {
        let tid = TypeId::of::<U>();
        let mut result = self.run_before(tid, user as &dyn Any, action);
        if result.is_none() {
            result = self.invoke::<U, R>(action, user, resource);
        }
        self.run_after(tid, user as &dyn Any, action, result)
    }

    /// Async sibling of [`raw`](Self::raw). before/after hooks are synchronous;
    /// only the gate dispatch awaits.
    pub(crate) async fn raw_async<U: 'static, R: 'static>(
        &self,
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<Response> {
        let tid = TypeId::of::<U>();
        let mut result = self.run_before(tid, user as &dyn Any, action);
        if result.is_none() {
            result = self.invoke_async::<U, R>(action, user, resource).await;
        }
        self.run_after(tid, user as &dyn Any, action, result)
    }

    // Run before hooks; first `Some` short-circuits.
    fn run_before(&self, tid: TypeId, user: &dyn Any, action: &str) -> Option<Response> {
        for hook in self.before_hooks(tid) {
            if let Some(decision) = hook(user, action) {
                return Some(Response::from(decision));
            }
        }
        None
    }

    // Run all after hooks (so they can log), filling the result only while it
    // is still undecided — Laravel's `$result ??= $afterResult`.
    fn run_after(
        &self,
        tid: TypeId,
        user: &dyn Any,
        action: &str,
        mut result: Option<Response>,
    ) -> Option<Response> {
        for hook in self.after_hooks(tid) {
            let current = result.as_ref().map(Response::allowed);
            let filled = hook(user, action, current);
            // Fill-only: an after hook can decide an undecided result, never
            // override one already produced by a before hook or gate.
            if result.is_none() {
                result = filled.map(Response::from);
            }
        }
        result
    }

    // ── Introspection ────────────────────────────────────────────────────────

    /// Whether a gate (sync or async) is registered for `(action, U, R)`. Used
    /// by [`crate::Gate::has`] for the introspection path Laravel calls
    /// `Gate::has`.
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
