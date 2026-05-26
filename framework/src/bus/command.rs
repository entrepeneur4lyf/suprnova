//! Command + Handler traits for the Bus.

use crate::error::FrameworkError;
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

#[async_trait]
pub trait Command: Serialize + DeserializeOwned + Send + Sync + 'static {
    type Output: Send + 'static;
    fn command_name() -> &'static str
    where
        Self: Sized;
}

#[async_trait]
pub trait Handler<C: Command>: Send + Sync + 'static {
    async fn handle(&self, cmd: C) -> Result<C::Output, FrameworkError>;
}
