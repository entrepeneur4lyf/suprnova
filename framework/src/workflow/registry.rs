//! Workflow registry via inventory

use crate::error::FrameworkError;
use std::future::Future;
use std::pin::Pin;

/// Boxed workflow runner
pub type WorkflowRunner =
    fn(&str) -> Pin<Box<dyn Future<Output = Result<String, FrameworkError>> + Send>>;

/// Inventory entry for a workflow
pub struct WorkflowEntry {
    pub name: &'static str,
    pub run: WorkflowRunner,
}

inventory::collect!(WorkflowEntry);

/// Find a workflow entry by name
pub fn find(name: &str) -> Option<&'static WorkflowEntry> {
    inventory::iter::<WorkflowEntry>
        .into_iter()
        .find(|entry| entry.name == name)
}
