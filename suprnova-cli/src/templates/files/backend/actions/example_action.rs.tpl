//! Example action.
//!
//! Demonstrates the single-responsibility command pattern. Resolve via
//! `App::resolve::<ExampleAction>()` from a controller, or inject it
//! directly with `#[inject] action: ExampleAction`. Delete this file
//! when you no longer need the example; use `suprnova make:action <Name>`
//! to scaffold real actions for your domain.

use suprnova::{injectable, FrameworkError};

/// Echoes its input back to the caller, with a structured-log entry
/// recording every invocation. Real applications replace this with the
/// domain command (e.g. `RegisterUser`, `PublishPost`, `ChargeInvoice`)
/// and inject the dependencies the command needs as fields.
///
/// `#[injectable]` registers the struct in the container so consumers
/// can resolve it via `App::resolve::<ExampleAction>()`.
#[injectable]
pub struct ExampleAction {
    // Inject dependencies as fields here, e.g.
    // db: suprnova::DbConnection,
}

impl ExampleAction {
    /// Run the action against `message` and return the echoed value.
    ///
    /// Emits a `tracing::info!` event so every invocation shows up in
    /// the request/log pipeline without the caller having to add their
    /// own instrumentation.
    pub async fn execute(&self, message: &str) -> Result<String, FrameworkError> {
        tracing::info!(action = "ExampleAction", message, "executed");
        Ok(format!("echo: {message}"))
    }
}
