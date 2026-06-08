//! Address and Attachment types for outgoing mail.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A mail address — email plus an optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    /// The raw email address (e.g. `"alice@example.org"`).
    pub email: String,
    /// Optional display name shown in `"Name <email>"` form.
    pub name: Option<String>,
}

impl Address {
    /// Build an address from an email-only string.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            name: None,
        }
    }
    /// Attach a display name to the address (rendered as `"Name <email>"`).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

impl From<&str> for Address {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for Address {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<(String, String)> for Address {
    /// `(name, email)`
    fn from(t: (String, String)) -> Self {
        Self {
            name: Some(t.0),
            email: t.1,
        }
    }
}

impl From<(&str, &str)> for Address {
    /// `(name, email)`
    fn from(t: (&str, &str)) -> Self {
        Self {
            name: Some(t.0.into()),
            email: t.1.into(),
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(n) => write!(f, "{n} <{}>", self.email),
            None => write!(f, "{}", self.email),
        }
    }
}

/// A binary attachment included on an outgoing mail message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Filename surfaced to the recipient.
    pub filename: String,
    /// Raw attachment bytes.
    pub content: Vec<u8>,
    /// MIME content type (e.g. `"application/pdf"`).
    pub content_type: String,
}

impl Attachment {
    /// Build an [`Attachment`] from raw bytes plus a filename and
    /// content-type label.
    pub fn new(
        filename: impl Into<String>,
        content: Vec<u8>,
        content_type: impl Into<String>,
    ) -> Self {
        Self {
            filename: filename.into(),
            content,
            content_type: content_type.into(),
        }
    }
}
