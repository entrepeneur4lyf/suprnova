//! Multi-level `?include=` parse tree for JSON:API compound documents.

use crate::data::RequestIncludeSet;
use std::collections::BTreeMap;

/// Parsed tree representation of a multi-level `?include=` query.
///
/// `?include=author.posts.tags,comments` parses to:
/// ```text
/// {
///   author: { posts: { tags: {} } },
///   comments: {}
/// }
/// ```
///
/// Children are stored in a `BTreeMap` so iteration order is the
/// deterministic lexicographic order of include names. The order is
/// observable only when validating includes against a resource's
/// allowlist: if multiple invalid include paths are present in one
/// request, the first one rejected is now stable across runs (instead
/// of varying with `HashMap`'s randomised iteration). The JSON:API
/// response itself does not surface this order — `included` is a set,
/// and the spec assigns no semantic meaning to its member order.
#[derive(Debug, Default, Clone)]
pub struct IncludeTree {
    /// Sub-includes keyed by their dotted segment. The leaf set is
    /// the set of paths actually requested by the client.
    pub children: BTreeMap<String, IncludeTree>,
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
