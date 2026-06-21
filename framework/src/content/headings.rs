//! Heading metadata and slug generation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A rendered Markdown heading with its generated anchor ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Heading {
    /// Heading depth, from 1 through 6.
    pub level: u8,
    /// Unique HTML anchor ID for this heading.
    pub id: String,
    /// Visible heading text with inline Markdown formatting removed.
    pub title: String,
}

/// Convert visible heading text into a stable URL-safe anchor slug.
pub fn slugify_heading(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                slug.push(lower);
            }
            last_was_separator = false;
        } else if !last_was_separator && !slug.is_empty() {
            slug.push('-');
            last_was_separator = true;
        }
    }

    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "section".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Generates unique heading IDs for a single rendered document.
pub(crate) struct HeadingIdGenerator {
    seen: HashMap<String, usize>,
}

impl HeadingIdGenerator {
    /// Create an empty per-document heading ID generator.
    pub(crate) fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    /// Return the next unique heading ID for visible heading text.
    pub(crate) fn next_id(&mut self, title: &str) -> String {
        let base = slugify_heading(title);
        let count = self.seen.entry(base.clone()).or_insert(0);
        *count += 1;

        if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        }
    }
}

/// Prefix a generated heading ID with the configured anchor namespace.
pub(crate) fn prefixed_heading_id(
    prefix: &str,
    generator: &mut HeadingIdGenerator,
    title: &str,
) -> String {
    format!("{}{}", prefix, generator.next_id(title))
}
