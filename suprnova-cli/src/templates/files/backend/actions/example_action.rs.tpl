//! Example action.
//!
//! Single-responsibility command resolved from the container. Replace
//! the body with your own logic when you implement the real feature.

use suprnova::injectable;

/// Example action.
///
/// `#[injectable]` registers this struct in the container so controllers
/// can resolve it via `App::resolve::<ExampleAction>()`. Inject any
/// dependencies it needs as fields.
#[injectable]
pub struct ExampleAction {
    // Inject dependencies as fields, e.g.
    // db: suprnova::DbConnection,
}

impl ExampleAction {
    /// Run the action and return a status string.
    pub fn execute(&self) -> String {
        "Hello from ExampleAction!".to_string()
    }
}
