//! Per-request include/exclude/only/except set parsed from the query
//! string by `IncludeMiddleware` and bound to a `tokio::task_local!`
//! so handlers and the lazy-prop resolver can consult it.

use std::sync::Arc;

/// Parsed `?include=`/`?exclude=`/`?only=`/`?except=` query parameters.
///
/// Semantics:
/// - `include` — lazy fields to resolve (default: none resolved).
/// - `exclude` — fields to drop from the response.
/// - `only` — when set, the response includes ONLY these fields.
/// - `except` — fields to drop (same effect as `exclude`; both names
///   exist for Laravel-Data API parity).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestIncludeSet {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub only: Option<Vec<String>>,
    pub except: Vec<String>,
}

impl RequestIncludeSet {
    /// Parse `?include=`/`?exclude=`/`?only=`/`?except=` query parameters
    /// from a raw query string.
    ///
    /// # Input contract
    ///
    /// - `raw` must NOT include the leading `?` — caller strips it.
    /// - Values are expected to be percent-decoded already; this parser
    ///   does not URL-decode (`include=foo%2Cbar` stays as one literal
    ///   value `foo%2Cbar`).
    /// - Repeated keys accumulate (`include=a&include=b` → `include: [a, b]`),
    ///   matching Laravel's array semantics.
    /// - The Laravel array form `include[]=a&include[]=b` is also accepted
    ///   and accumulates the same way.
    /// - Whitespace around values is trimmed; empty values are dropped
    ///   (`include= a , , b` → `[a, b]`).
    /// - Unknown keys are silently ignored — only the four canonical names
    ///   are recognized.
    pub fn from_query(raw: &str) -> Self {
        let mut s = Self::default();
        for pair in raw.split('&') {
            let mut iter = pair.splitn(2, '=');
            let key = iter.next().unwrap_or("").trim();
            let val = iter.next().unwrap_or("");
            let stripped_key = key.strip_suffix("[]").unwrap_or(key);
            let values: Vec<String> = val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            match stripped_key {
                "include" => s.include.extend(values),
                "exclude" => s.exclude.extend(values),
                "only" => {
                    let bucket = s.only.get_or_insert_with(Vec::new);
                    bucket.extend(values);
                }
                "except" => s.except.extend(values),
                _ => {}
            }
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty()
            && self.exclude.is_empty()
            && self.only.is_none()
            && self.except.is_empty()
    }

    pub fn includes(&self, field: &str) -> bool {
        self.include.iter().any(|s| s == field)
    }
}

tokio::task_local! {
    /// Per-request include set. Bound by `IncludeMiddleware`; consulted
    /// by `Prop::Lazy` resolution and any handler that wants to honor
    /// `?include=` / `?only=` semantics.
    pub static REQUEST_INCLUDE_SET: Arc<RequestIncludeSet>;
}

/// Helper: get the current request's include set, or empty if none bound.
pub fn current_include_set() -> Arc<RequestIncludeSet> {
    REQUEST_INCLUDE_SET
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::new(RequestIncludeSet::default()))
}
