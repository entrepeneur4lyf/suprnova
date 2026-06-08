//! Sparse fieldset support for JSON:API `?fields[type]=field1,field2`.

use std::collections::HashMap;

/// Parsed `?fields[type]=field1,field2` query string state per
/// JSON:API spec. Maps resource type → allowed attribute names.
#[derive(Debug, Default, Clone)]
pub struct RequestFieldsetSet {
    by_type: HashMap<String, Vec<String>>,
}

impl RequestFieldsetSet {
    /// Build from a raw query string. Parses `fields[type]=f1,f2`
    /// pairs; ignores keys outside the `fields[type]` shape.
    ///
    /// The parser URL-decodes pairs via `url::form_urlencoded::parse`,
    /// so encoded bracket forms (`fields%5Bposts%5D=...`), encoded
    /// commas inside values (`title%2Cbody`), and encoded type names
    /// all decode correctly before the `fields[<type>]` shape is
    /// matched. Repeated keys accumulate their field lists.
    pub fn from_query(query: &str) -> Self {
        let mut by_type: HashMap<String, Vec<String>> = HashMap::new();
        for (key_cow, val_cow) in url::form_urlencoded::parse(query.as_bytes()) {
            let key = key_cow.trim();
            // key must look like `fields[some_type]` after decoding.
            let Some(rest) = key.strip_prefix("fields[") else {
                continue;
            };
            let Some(rtype) = rest.strip_suffix(']') else {
                continue;
            };
            if rtype.is_empty() {
                continue;
            }
            let fields: Vec<String> = val_cow
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            by_type.entry(rtype.to_string()).or_default().extend(fields);
        }
        Self { by_type }
    }

    /// Returns the requested fields for `type`, or `None` if the
    /// request did not constrain this type (meaning: send all).
    pub fn fields_for<'a>(&'a self, resource_type: &str) -> Option<Vec<&'a str>> {
        self.by_type
            .get(resource_type)
            .map(|v| v.iter().map(String::as_str).collect())
    }

    /// Returns `true` when the request did not request a sparse fieldset
    /// for any type — meaning every type renders its full attribute set.
    pub fn is_empty(&self) -> bool {
        self.by_type.is_empty()
    }
}

tokio::task_local! {
    /// Per-request task-local holding the parsed sparse-fieldset set.
    /// Installed by `IncludeMiddleware`; queried via [`current_fieldset`].
    pub static REQUEST_FIELDSET: RequestFieldsetSet;
}

/// Access the current request's `RequestFieldsetSet`. Returns an
/// empty set when called outside an HTTP request scope.
pub fn current_fieldset() -> RequestFieldsetSet {
    REQUEST_FIELDSET.try_with(|s| s.clone()).unwrap_or_default()
}

/// Scope a `RequestFieldsetSet` around a future. Used in tests to
/// simulate what `IncludeMiddleware` does for a real HTTP request.
pub async fn scope_fieldset<F, R>(set: RequestFieldsetSet, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    REQUEST_FIELDSET.scope(set, f).await
}
