//! Synchronous command bus.
//!
//! Handlers register one-per-command-type and dispatch is in-process.
//! For async background dispatch, use the `Queue` facade instead.

pub mod command;
pub mod testing;

use crate::error::FrameworkError;
use crate::lock;
use command::{Command, Handler};
use futures::future::BoxFuture;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Result of a `Bus::dispatch` call.
///
/// In normal mode, dispatch returns `Executed(output)`. Under
/// `Bus::fake()` it returns `Captured` — the command is recorded for
/// `assert_dispatched` but the handler is not run.
#[derive(Debug)]
pub enum Dispatched<T> {
    /// Real-mode result: the handler ran and produced `T`.
    Executed(T),
    /// Fake-mode result: the dispatch was recorded for assertions but no handler ran.
    Captured,
}

impl<T> Dispatched<T> {
    /// Unwrap `Executed`, panicking on `Captured` (typical real-mode use).
    pub fn unwrap_executed(self) -> T {
        match self {
            Dispatched::Executed(v) => v,
            Dispatched::Captured => {
                panic!("expected Dispatched::Executed but got Captured (fake mode active?)")
            }
        }
    }

    /// `true` if the command was actually run.
    pub fn is_executed(&self) -> bool {
        matches!(self, Dispatched::Executed(_))
    }

    /// `true` if the command was captured (fake mode).
    pub fn is_captured(&self) -> bool {
        matches!(self, Dispatched::Captured)
    }

    /// Convert to `Option<T>` — `Some(T)` if executed, `None` if captured.
    pub fn executed(self) -> Option<T> {
        match self {
            Dispatched::Executed(v) => Some(v),
            Dispatched::Captured => None,
        }
    }
}

/// Erased dispatcher: takes a boxed-`Any` command (we down-cast back to `C`
/// inside the closure) and returns a boxed-`Any` output (the caller down-casts
/// back to `C::Output`).
///
/// The bus is purely in-process: type-erasure goes through `Box<dyn Any>`
/// rather than JSON. Both the command-in and the output-out down-casts are
/// infallible by TypeId construction at the registration and dispatch sites
/// below; we panic on mismatch because that can only happen if the registry
/// has been corrupted.
type ErasedDispatcher = Arc<
    dyn Fn(Box<dyn Any + Send>) -> BoxFuture<'static, Result<Box<dyn Any + Send>, FrameworkError>>
        + Send
        + Sync,
>;

static REGISTRY: RwLock<Option<HashMap<TypeId, ErasedDispatcher>>> = RwLock::new(None);

/// Synchronous command bus facade.
///
/// Register a handler once at boot (or at the start of each test) with
/// [`Bus::register`], then dispatch commands with [`Bus::dispatch`].
/// For test isolation, install [`testing::install_fake`] to capture dispatches
/// without running handlers.
pub struct Bus;

impl Bus {
    /// Register a handler for command type `C`. Overwrites any previous handler
    /// for the same type and logs a warning when that happens — tests routinely
    /// re-register, but a duplicate binding from boot usually indicates two
    /// `register` calls from different service providers.
    ///
    /// The dispatcher path keeps commands and outputs as native Rust values —
    /// no serde round-trip. Only the test-fake path serializes commands (so it
    /// can store them by predicate-friendly value). That means a `C::Output`
    /// only has to be `Send + 'static`; non-serde types like `Bytes`, opaque
    /// handles, `Arc<Mutex<…>>`, etc. all work as outputs on the real path.
    pub fn register<C, H>(handler: H)
    where
        C: Command,
        H: Handler<C>,
    {
        let h = Arc::new(handler);
        let dispatcher: ErasedDispatcher = Arc::new(move |any_cmd: Box<dyn Any + Send>| {
            let h = h.clone();
            Box::pin(async move {
                let cmd: C = *any_cmd.downcast::<C>().unwrap_or_else(|_| {
                    panic!(
                        "Bus: internal type invariant — dispatcher for {} got a value of the \
                         wrong concrete type. This is a framework bug.",
                        C::command_name()
                    )
                });
                let out = h.handle(cmd).await?;
                let boxed: Box<dyn Any + Send> = Box::new(out);
                Ok(boxed)
            })
        });
        match lock::write(&REGISTRY, "bus handler registry") {
            Ok(mut g) => {
                let map = g.get_or_insert_with(HashMap::new);
                if map.insert(TypeId::of::<C>(), dispatcher).is_some() {
                    tracing::warn!(
                        command = C::command_name(),
                        "Bus::register replaced an existing handler for this command type. \
                         If this fires outside of test setup, two registrations are competing \
                         and the later one wins."
                    );
                }
            }
            Err(_) => {
                tracing::error!(
                    command = C::command_name(),
                    "Bus registry lock poisoned; skipping handler registration. \
                     Bus::dispatch calls for this command will return \
                     'no handler for ...' errors."
                );
            }
        }
    }

    /// Dispatch a command. Runs the registered handler in-process and returns
    /// its typed output wrapped in [`Dispatched`].
    ///
    /// Under [`testing::install_fake`], the command is captured and
    /// `Ok(Dispatched::Captured)` is returned — the handler is **not** invoked.
    /// Real failures (no handler registered or handler error) still produce
    /// `Err(_)`.
    pub async fn dispatch<C: Command>(cmd: C) -> Result<Dispatched<C::Output>, FrameworkError> {
        if testing::is_active() {
            testing::record::<C>(&cmd)?;
            return Ok(Dispatched::Captured);
        }
        let dispatcher = {
            let g = lock::read(&REGISTRY, "bus handler registry")?;
            let map = g
                .as_ref()
                .ok_or_else(|| FrameworkError::internal("Bus: no handlers registered"))?;
            map.get(&TypeId::of::<C>()).cloned().ok_or_else(|| {
                FrameworkError::internal(format!("Bus: no handler for {}", C::command_name()))
            })?
        };
        let result = dispatcher(Box::new(cmd)).await?;
        let out: C::Output = *result.downcast::<C::Output>().unwrap_or_else(|_| {
            panic!(
                "Bus: internal type invariant — handler for {} returned a value of the wrong \
                 concrete type. This is a framework bug.",
                C::command_name()
            )
        });
        Ok(Dispatched::Executed(out))
    }

    /// Run commands sequentially, stopping on (and including) the first error.
    ///
    /// This is the in-process synchronous helper and is intentionally
    /// homogeneous over a single command type `C` — the dispatcher returns
    /// `Dispatched<C::Output>`, which only makes sense when every input
    /// shares one `Output`. For Laravel-style heterogeneous chains of
    /// different job types, use [`Queue::chain`](crate::queue::Queue::chain),
    /// which boxes each job into a queue envelope.
    pub async fn chain<C: Command + Clone>(
        cmds: Vec<C>,
    ) -> Vec<Result<Dispatched<C::Output>, FrameworkError>> {
        let mut out = Vec::with_capacity(cmds.len());
        for c in cmds {
            let r = Self::dispatch(c).await;
            let was_err = r.is_err();
            out.push(r);
            if was_err {
                break;
            }
        }
        out
    }

    /// Run commands concurrently and collect results in input order.
    ///
    /// Like [`chain`](Self::chain), this is the in-process synchronous helper
    /// and is homogeneous over a single command type `C`. For Laravel-style
    /// heterogeneous batches of mixed job types, use
    /// [`Queue::batch`](crate::queue::Queue::batch).
    pub async fn batch<C: Command + Clone>(
        cmds: Vec<C>,
    ) -> Vec<Result<Dispatched<C::Output>, FrameworkError>> {
        let futs = cmds.into_iter().map(|c| Self::dispatch(c));
        futures::future::join_all(futs).await
    }
}
