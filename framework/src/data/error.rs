//! Errors raised by the data subsystem (include-set parse / lookup).

use crate::FrameworkError;

/// Errors raised when resolving a request's include set against a
/// `#[derive(Data)]` allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncludeError {
    /// The request asked to include a field that the receiving DTO
    /// does not list in its `allow_include` allowlist.
    UnknownInclude {
        /// Field path the request asked to include.
        field: String,
        /// Allowlist the DTO declared, suitable for echoing back to the client.
        allowed: Vec<String>,
    },
}

impl IncludeError {
    /// Convert into a [`FrameworkError`] (400 with a remediation message
    /// listing the allowlist).
    pub fn into_framework_error(self) -> FrameworkError {
        match self {
            IncludeError::UnknownInclude { field, allowed } => {
                FrameworkError::bad_request(format!(
                    "Unknown include `{}`. Allowed includes: [{}]",
                    field,
                    allowed.join(", ")
                ))
            }
        }
    }
}
