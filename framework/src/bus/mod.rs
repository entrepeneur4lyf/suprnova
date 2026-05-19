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
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Result of a `Bus::dispatch` call.
///
/// In normal mode, dispatch returns `Executed(output)`. Under
/// `Bus::fake()` it returns `Captured` — the command is recorded for
/// `assert_dispatched` but the handler is not run.
#[derive(Debug)]
pub enum Dispatched<T> {
    Executed(T),
    Captured,
}

impl<T> Dispatched<T> {
    /// Unwrap `Executed`, panicking on `Captured` (typical real-mode use).
    pub fn unwrap_executed(self) -> T {
        match self {
            Dispatched::Executed(v) => v,
            Dispatched::Captured => panic!(
                "expected Dispatched::Executed but got Captured (fake mode active?)"
            ),
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

type ErasedDispatcher = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, FrameworkError>>
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
    /// for the same type.
    pub fn register<C, H>(handler: H)
    where
        C: Command,
        H: Handler<C>,
        C::Output: serde::Serialize + serde::de::DeserializeOwned,
    {
        let h = Arc::new(handler);
        let dispatcher: ErasedDispatcher = Arc::new(move |payload: serde_json::Value| {
            let h = h.clone();
            Box::pin(async move {
                let cmd: C = serde_json::from_value(payload).map_err(|e| {
                    FrameworkError::internal(format!("Bus decode {}: {e}", C::command_name()))
                })?;
                let out = h.handle(cmd).await?;
                let json = serde_json::to_value(&out).map_err(|e| {
                    FrameworkError::internal(format!("Bus encode {}: {e}", C::command_name()))
                })?;
                Ok(json)
            })
        });
        let mut g = lock::write(&REGISTRY).expect("bus registry poisoned");
        g.get_or_insert_with(HashMap::new)
            .insert(TypeId::of::<C>(), dispatcher);
    }

    /// Dispatch a command. Runs the registered handler in-process and returns
    /// its typed output wrapped in [`Dispatched`].
    ///
    /// Under [`testing::install_fake`], the command is captured and
    /// `Ok(Dispatched::Captured)` is returned — the handler is **not** invoked.
    /// Real failures (no handler registered, encode/decode errors, handler error)
    /// still produce `Err(_)`.
    pub async fn dispatch<C: Command>(cmd: C) -> Result<Dispatched<C::Output>, FrameworkError>
    where
        C::Output: serde::Serialize + serde::de::DeserializeOwned,
    {
        if testing::is_active() {
            testing::record::<C>(&cmd)?;
            return Ok(Dispatched::Captured);
        }
        let dispatcher = {
            let g = lock::read(&REGISTRY).expect("bus registry poisoned");
            let map = g
                .as_ref()
                .ok_or_else(|| FrameworkError::internal("Bus: no handlers registered"))?;
            map.get(&TypeId::of::<C>())
                .cloned()
                .ok_or_else(|| FrameworkError::internal(format!("Bus: no handler for {}", C::command_name())))?
        };
        let payload = serde_json::to_value(&cmd)
            .map_err(|e| FrameworkError::internal(format!("Bus encode: {e}")))?;
        let result = dispatcher(payload).await?;
        let out: C::Output = serde_json::from_value(result)
            .map_err(|e| FrameworkError::internal(format!("Bus decode result: {e}")))?;
        Ok(Dispatched::Executed(out))
    }

    /// Run commands sequentially, stopping on (and including) the first error.
    pub async fn chain<C: Command + Clone>(cmds: Vec<C>) -> Vec<Result<Dispatched<C::Output>, FrameworkError>>
    where
        C::Output: serde::Serialize + serde::de::DeserializeOwned,
    {
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
    pub async fn batch<C: Command + Clone>(cmds: Vec<C>) -> Vec<Result<Dispatched<C::Output>, FrameworkError>>
    where
        C::Output: serde::Serialize + serde::de::DeserializeOwned,
    {
        let futs = cmds.into_iter().map(|c| Self::dispatch(c));
        futures::future::join_all(futs).await
    }
}
