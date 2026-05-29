//! Workflow registry via inventory

use crate::error::FrameworkError;
use std::collections::{HashMap, HashSet};
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

impl std::fmt::Debug for WorkflowEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `run` field is a function pointer; print only the name so
        // diagnostics stay readable.
        f.debug_struct("WorkflowEntry")
            .field("name", &self.name)
            .finish()
    }
}

inventory::collect!(WorkflowEntry);

/// Find a workflow entry by name.
///
/// Returns the first inventory entry matching `name`. Duplicate
/// registrations under the same name are silently order-dependent
/// here; use [`find_strict`] for a duplicate-aware lookup or
/// [`assert_no_duplicates`] at boot to fail loudly instead.
pub fn find(name: &str) -> Option<&'static WorkflowEntry> {
    inventory::iter::<WorkflowEntry>
        .into_iter()
        .find(|entry| entry.name == name)
}

/// Find a workflow entry by name, returning an error if more than one
/// entry shares the same name.
///
/// Duplicate `#[workflow]` registrations under a single name are a
/// bug: which body runs depends on inventory link order, which depends
/// on the platform linker. Two `welcome_flow` workflows from different
/// modules can have wildly different behaviour at runtime — silent
/// shadowing is undebuggable. Boot-time callers (workers, `start_named`)
/// should prefer this over [`find`].
pub fn find_strict(name: &str) -> Result<Option<&'static WorkflowEntry>, FrameworkError> {
    let matches: Vec<&'static WorkflowEntry> = inventory::iter::<WorkflowEntry>
        .into_iter()
        .filter(|entry| entry.name == name)
        .collect();

    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        n => Err(FrameworkError::internal(format!(
            "Workflow '{name}' is registered {n} times. \
             Two `#[workflow]` definitions cannot share the same name \
             (`module_path::fn_name`). Rename one of the workflow \
             functions or move it to a uniquely-named module."
        ))),
    }
}

/// Boot-time check that every registered workflow name is unique.
///
/// Walks the inventory once and returns an aggregated error listing
/// every duplicated name. Call this from your application bootstrap
/// (before starting a worker or accepting requests) so a duplicate
/// `#[workflow]` collision aborts startup rather than degrading into
/// order-dependent behaviour at runtime.
pub fn assert_no_duplicates() -> Result<(), FrameworkError> {
    let mut seen: HashMap<&'static str, usize> = HashMap::new();
    for entry in inventory::iter::<WorkflowEntry> {
        *seen.entry(entry.name).or_insert(0) += 1;
    }
    let dupes: Vec<(&'static str, usize)> =
        seen.into_iter().filter(|(_, count)| *count > 1).collect();
    if dupes.is_empty() {
        return Ok(());
    }

    let mut lines: Vec<String> = dupes
        .iter()
        .map(|(name, count)| format!("  - '{name}' is registered {count} times"))
        .collect();
    lines.sort();
    Err(FrameworkError::internal(format!(
        "Duplicate `#[workflow]` registrations detected:\n{}",
        lines.join("\n")
    )))
}

/// List the names of every registered workflow. Used by tooling and
/// the duplicate-detection helpers.
#[doc(hidden)]
pub fn all_names() -> Vec<&'static str> {
    inventory::iter::<WorkflowEntry>
        .into_iter()
        .map(|e| e.name)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic entries inserted into the inventory via `inventory::submit!`
    // exercise the duplicate-detection path without depending on a real
    // `#[workflow]` macro expansion.
    fn dupe_runner_a(
        _input: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, FrameworkError>> + Send>> {
        Box::pin(async { Ok("a".to_string()) })
    }

    fn dupe_runner_b(
        _input: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, FrameworkError>> + Send>> {
        Box::pin(async { Ok("b".to_string()) })
    }

    inventory::submit! {
        WorkflowEntry {
            name: "suprnova::workflow::registry::tests::DUPE_NAME",
            run: dupe_runner_a,
        }
    }

    inventory::submit! {
        WorkflowEntry {
            name: "suprnova::workflow::registry::tests::DUPE_NAME",
            run: dupe_runner_b,
        }
    }

    #[test]
    fn find_strict_returns_err_on_duplicate() {
        let result = find_strict("suprnova::workflow::registry::tests::DUPE_NAME");
        let err = result.expect_err("duplicate name must error");
        let msg = err.to_string();
        assert!(
            msg.contains("registered 2 times")
                || msg.contains("registered 3 times")
                || msg.contains("registered ") && msg.contains(" times"),
            "error must mention duplication count, got: {msg}"
        );
        assert!(
            msg.contains("DUPE_NAME"),
            "error must mention offending name, got: {msg}"
        );
    }

    #[test]
    fn find_strict_returns_none_for_missing() {
        let result = find_strict("suprnova::workflow::registry::tests::DOES_NOT_EXIST")
            .expect("missing name is not an error");
        assert!(result.is_none(), "missing entry must be Ok(None)");
    }

    #[test]
    fn assert_no_duplicates_lists_offenders() {
        let err = assert_no_duplicates().expect_err("DUPE_NAME guarantees we have a duplicate");
        let msg = err.to_string();
        assert!(
            msg.contains("DUPE_NAME"),
            "error must list the offending workflow, got: {msg}"
        );
        assert!(
            msg.contains("Duplicate"),
            "error must label the failure clearly, got: {msg}"
        );
    }
}
