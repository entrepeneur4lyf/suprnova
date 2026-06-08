//! Command + Handler traits for the Bus.

use crate::error::FrameworkError;
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

/// A serializable command dispatched through the [`Bus`](crate::bus::Bus).
///
/// Commands are owned data — the bus serializes them across thread or
/// process boundaries (queued execution, RPC fan-out), so a command must
/// be `Serialize + DeserializeOwned`. Each command type names its own
/// [`Handler`] implementation by its [`command_name`](Command::command_name).
#[async_trait]
pub trait Command: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Return value the handler produces on success.
    type Output: Send + 'static;
    /// Stable identifier the bus uses to look up the registered handler.
    /// Conventionally the snake-case form of the type name.
    fn command_name() -> &'static str
    where
        Self: Sized;
}

/// Handler for a specific [`Command`] type. One handler per command type;
/// the bus enforces uniqueness at registration.
#[async_trait]
pub trait Handler<C: Command>: Send + Sync + 'static {
    /// Execute `cmd` and return its [`Command::Output`], or a
    /// [`FrameworkError`] on failure.
    async fn handle(&self, cmd: C) -> Result<C::Output, FrameworkError>;
}
