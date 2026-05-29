//! Typed `jsonapi` top-level member (spec §5.1.1, Laravel
//! `JsonApiResource::configure(...)`).

use serde_json::{Map, Value};

/// JSON:API document-level implementation information. Mirrors Laravel
/// 13's `JsonApiResource::$jsonApiInformation`. Renders to the optional
/// top-level `jsonapi` member of the envelope.
#[derive(Debug, Clone, Default)]
pub struct JsonApiInfo {
    /// Optional spec version (e.g. `"1.1"`).
    pub version: Option<String>,
    /// Optional `ext` URIs (spec §5.1.1.1).
    pub ext: Vec<String>,
    /// Optional `profile` URIs (spec §5.1.1.2).
    pub profile: Vec<String>,
    /// Optional `meta` object.
    pub meta: Map<String, Value>,
}

impl JsonApiInfo {
    /// Start an empty info block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the spec version.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Append an `ext` URI.
    pub fn with_ext(mut self, uri: impl Into<String>) -> Self {
        self.ext.push(uri.into());
        self
    }

    /// Append a `profile` URI.
    pub fn with_profile(mut self, uri: impl Into<String>) -> Self {
        self.profile.push(uri.into());
        self
    }

    /// Add a meta key/value.
    pub fn with_meta(mut self, key: impl Into<String>, value: Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// True when no fields are populated.
    pub fn is_empty(&self) -> bool {
        self.version.is_none()
            && self.ext.is_empty()
            && self.profile.is_empty()
            && self.meta.is_empty()
    }

    /// Render to a `Value` suitable for inclusion under the top-level
    /// `jsonapi` member. Returns `Value::Object` even when partially
    /// populated; only fully empty info is `Value::Object` with no keys.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        if let Some(v) = &self.version {
            m.insert("version".into(), Value::String(v.clone()));
        }
        if !self.ext.is_empty() {
            m.insert(
                "ext".into(),
                Value::Array(self.ext.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }
        if !self.profile.is_empty() {
            m.insert(
                "profile".into(),
                Value::Array(
                    self.profile
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if !self.meta.is_empty() {
            m.insert("meta".into(), Value::Object(self.meta.clone()));
        }
        Value::Object(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_info_renders_to_empty_object() {
        assert_eq!(JsonApiInfo::new().to_value(), Value::Object(Map::new()));
    }

    #[test]
    fn version_renders() {
        let v = JsonApiInfo::new().with_version("1.1").to_value();
        assert_eq!(v["version"], "1.1");
    }

    #[test]
    fn ext_and_profile_render_as_arrays() {
        let v = JsonApiInfo::new()
            .with_ext("https://jsonapi.org/ext/atomic")
            .with_profile("https://example.com/profile")
            .to_value();
        assert!(v["ext"].is_array());
        assert!(v["profile"].is_array());
    }

    #[test]
    fn meta_renders() {
        let v = JsonApiInfo::new()
            .with_meta("copyright", Value::String("2026".into()))
            .to_value();
        assert_eq!(v["meta"]["copyright"], "2026");
    }

    #[test]
    fn is_empty_tracks_state() {
        assert!(JsonApiInfo::new().is_empty());
        assert!(!JsonApiInfo::new().with_version("1.0").is_empty());
    }
}
