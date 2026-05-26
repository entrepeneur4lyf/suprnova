//! Multi-level `?include=` parse tree for JSON:API compound documents.

use crate::data::RequestIncludeSet;
use std::collections::HashMap;

/// Parsed tree representation of a multi-level `?include=` query.
///
/// `?include=author.posts.tags,comments` parses to:
/// ```text
/// {
///   author: { posts: { tags: {} } },
///   comments: {}
/// }
/// ```
#[derive(Debug, Default, Clone)]
pub struct IncludeTree {
    pub children: HashMap<String, IncludeTree>,
}

impl IncludeTree {
    /// Build from `RequestIncludeSet`. Each include name is split on
    /// `.` and the segments accumulate into a nested tree.
    pub fn from_include_set(set: &RequestIncludeSet) -> Self {
        let mut root = Self::default();
        for path in &set.include {
            let mut node = &mut root;
            for segment in path.split('.') {
                node = node.children.entry(segment.to_string()).or_default();
            }
        }
        root
    }

    /// Empty tree — no relationships requested.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Lookup a child subtree by name. Returns `None` when the name
    /// is not present in this branch.
    pub fn subtree(&self, name: &str) -> Option<&IncludeTree> {
        self.children.get(name)
    }

    /// Iterate over (name, subtree) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &IncludeTree)> {
        self.children.iter().map(|(k, v)| (k.as_str(), v))
    }
}
