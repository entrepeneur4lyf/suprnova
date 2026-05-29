//! JSON:API top-level document builder + resource-object renderer.

use super::fieldset::RequestFieldsetSet;
use super::maybe::strip_missing_values;
use super::trait_def::{IntoJsonResource, RelationshipValue};
use serde_json::{Map, Value};

/// Builder for a JSON:API top-level document. Consumed by
/// `Resource::single` / `Resource::collection` / `Resource::paginated`
/// to produce the final response body.
pub struct JsonApiBuilder {
    primary: PrimaryData,
    pub(crate) included: Vec<Value>,
    links: Map<String, Value>,
    meta: Map<String, Value>,
    /// `additional()` top-level keys — merged into the envelope root
    /// alongside (not under) `data`/`included`/`links`/`meta`.
    additional: Map<String, Value>,
    /// Optional `jsonapi` member (spec §5.1.1).
    jsonapi: Option<Value>,
    seen_included: std::collections::HashSet<(String, String)>,
}

enum PrimaryData {
    Single(Value),
    Collection(Vec<Value>),
}

impl JsonApiBuilder {
    pub(crate) fn single(data: Value) -> Self {
        Self {
            primary: PrimaryData::Single(data),
            included: Vec::new(),
            links: Map::new(),
            meta: Map::new(),
            additional: Map::new(),
            jsonapi: None,
            seen_included: Default::default(),
        }
    }

    pub(crate) fn collection(data: Vec<Value>) -> Self {
        Self {
            primary: PrimaryData::Collection(data),
            included: Vec::new(),
            links: Map::new(),
            meta: Map::new(),
            additional: Map::new(),
            jsonapi: None,
            seen_included: Default::default(),
        }
    }

    /// Add a top-level `meta` key/value. Laravel-shape name.
    pub fn with_meta(mut self, key: impl Into<String>, value: Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// Suprnova-name alias for [`Self::with_meta`].
    pub fn with_meta_kv(self, key: impl Into<String>, value: Value) -> Self {
        self.with_meta(key, value)
    }

    /// Merge a whole map into top-level `meta`.
    pub fn with_meta_map(mut self, map: Map<String, Value>) -> Self {
        for (k, v) in map {
            self.meta.insert(k, v);
        }
        self
    }

    pub fn with_link(mut self, rel: impl Into<String>, href: impl Into<String>) -> Self {
        self.links.insert(rel.into(), Value::String(href.into()));
        self
    }

    /// Set a top-level link from an arbitrary `Value` (allowing the
    /// JSON:API link-object form `{href, meta}`, not just a bare URL).
    pub fn with_link_value(mut self, rel: impl Into<String>, value: Value) -> Self {
        self.links.insert(rel.into(), value);
        self
    }

    /// Add a key to the envelope root that lives alongside `data`. The
    /// spec calls these "extension members"; Laravel exposes them via
    /// `JsonResource::additional`.
    pub fn with_additional(mut self, key: impl Into<String>, value: Value) -> Self {
        self.additional.insert(key.into(), value);
        self
    }

    /// Merge a whole map of additional keys.
    pub fn with_additional_map(mut self, map: Map<String, Value>) -> Self {
        for (k, v) in map {
            self.additional.insert(k, v);
        }
        self
    }

    /// Set the top-level `jsonapi` member.
    pub fn with_jsonapi(mut self, value: Value) -> Self {
        self.jsonapi = Some(value);
        self
    }

    /// Push a related resource into `included`, deduplicated by
    /// (type, id) per JSON:API spec section 8.
    pub(crate) fn push_included(&mut self, resource: Value) {
        let key = (
            resource
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            resource
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        );
        if key.0.is_empty() || key.1.is_empty() {
            return;
        }
        if self.seen_included.insert(key) {
            self.included.push(resource);
        }
    }

    pub fn build(self) -> Value {
        let mut doc = Map::new();
        match self.primary {
            PrimaryData::Single(v) => {
                doc.insert("data".into(), v);
            }
            PrimaryData::Collection(arr) => {
                doc.insert("data".into(), Value::Array(arr));
            }
        }
        if !self.included.is_empty() {
            doc.insert("included".into(), Value::Array(self.included));
        }
        if !self.links.is_empty() {
            doc.insert("links".into(), Value::Object(self.links));
        }
        if !self.meta.is_empty() {
            doc.insert("meta".into(), Value::Object(self.meta));
        }
        if let Some(jsonapi) = self.jsonapi {
            doc.insert("jsonapi".into(), jsonapi);
        }
        // `additional` keys land at the envelope root alongside `data`.
        // They never override the canonical members above.
        for (k, v) in self.additional {
            doc.entry(k).or_insert(v);
        }
        Value::Object(doc)
    }
}

/// Render a single resource object — used by `Resource::single` and
/// recursively by `PushIncluded` impls for nested includes.
///
/// Emits `{type, id, attributes, relationships?, links?, meta?}` per
/// the JSON:API spec §5.2. Strips any `Maybe::Missing` sentinels from
/// the attributes pass.
pub fn render_resource_object<T: IntoJsonResource>(
    resource: &T,
    fieldset: &RequestFieldsetSet,
) -> Value {
    let rtype = T::resource_type();
    let id = resource.resource_id();
    let attrs_filter = fieldset.fields_for(rtype);
    let attrs_filter_ref: Option<&[&str]> = attrs_filter.as_deref();
    let mut attrs = resource.resource_attributes(attrs_filter_ref);
    // Drop any Maybe::Missing sentinel objects emitted by conditional
    // attributes (see resources::maybe).
    strip_missing_values(&mut attrs);

    let mut data = Map::new();
    data.insert("type".into(), Value::String(rtype.to_string()));
    data.insert("id".into(), Value::String(id));
    data.insert("attributes".into(), attrs);

    let rels = resource.resource_relationships();
    if !rels.is_empty() {
        let mut rels_map = Map::new();
        for (name, value) in rels {
            let v = match value {
                RelationshipValue::Single(rid) => serde_json::json!({ "data": rid.to_value() }),
                RelationshipValue::Many(rids) => {
                    let arr: Vec<Value> = rids.iter().map(|r| r.to_value()).collect();
                    serde_json::json!({ "data": arr })
                }
                RelationshipValue::Null => serde_json::json!({ "data": null }),
            };
            rels_map.insert(name, v);
        }
        data.insert("relationships".into(), Value::Object(rels_map));
    }

    // Per-resource links — emit only when non-empty (spec §5.2.7).
    let links = resource.resource_links();
    if !links.is_empty() {
        data.insert("links".into(), Value::Object(links));
    }

    // Per-resource meta — emit only when non-empty (spec §5.2.7).
    let meta = resource.resource_meta();
    if !meta.is_empty() {
        data.insert("meta".into(), Value::Object(meta));
    }

    Value::Object(data)
}
