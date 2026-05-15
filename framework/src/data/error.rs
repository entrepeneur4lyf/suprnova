//! Errors raised by the data subsystem (include-set parse / lookup).

use crate::FrameworkError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncludeError {
    /// The request asked to include a field that the receiving DTO
    /// does not list in its `allow_include` allowlist.
    UnknownInclude { field: String, allowed: Vec<String> },
}

impl IncludeError {
    pub fn into_framework_error(self) -> FrameworkError {
        match self {
            IncludeError::UnknownInclude { field, allowed } => FrameworkError::bad_request(
                format!(
                    "Unknown include `{}`. Allowed includes: [{}]",
                    field,
                    allowed.join(", ")
                ),
            ),
        }
    }
}
