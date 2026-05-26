//! Address and Attachment types for outgoing mail.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    pub email: String,
    pub name: Option<String>,
}

impl Address {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            name: None,
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub content: Vec<u8>,
    pub content_type: String,
}

impl Attachment {
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
