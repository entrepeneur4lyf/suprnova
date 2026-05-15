//! JSON:API top-level document builder + resource-object renderer.

use serde_json::{Map, Value};
use super::trait_def::{IntoJsonResource, RelationshipValue};
use super::fieldset::RequestFieldsetSet;

/// Builder for a JSON:API top-level document. Consumed by
/// `Resource::single` / `Resource::collection` / `Resource::paginated`
/// to produce the final response body.
pub struct JsonApiBuilder {
    primary: PrimaryData,
    pub(crate) included: Vec<Value>,
    links: Map<String, Value>,
    meta: Map<String, Value>,
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
            seen_included: Default::default(),
        }
    }

    pub(crate) fn collection(data: Vec<Value>) -> Self {
        Self {
            primary: PrimaryData::Collection(data),
            included: Vec::new(),
            links: Map::new(),
            meta: Map::new(),
            seen_included: Default::default(),
        }
    }

    pub fn with_meta_kv(mut self, key: impl Into<String>, value: Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    pub fn with_link(mut self, rel: impl Into<String>, href: impl Into<String>) -> Self {
        self.links.insert(rel.into(), Value::String(href.into()));
        self
    }

    /// Push a related resource into `included`, deduplicated by
    /// (type, id) per JSON:API spec section 8.
    pub(crate) fn push_included(&mut self, resource: Value) {
        let key = (
            resource.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            resource.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
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
            PrimaryData::Single(v) => { doc.insert("data".into(), v); }
            PrimaryData::Collection(arr) => { doc.insert("data".into(), Value::Array(arr)); }
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
        Value::Object(doc)
    }
}

/// Render a single resource object — used by `Resource::single` and
/// recursively by `PushIncluded` impls for nested includes.
pub fn render_resource_object<T: IntoJsonResource>(
    resource: &T,
    fieldset: &RequestFieldsetSet,
) -> Value {
    let rtype = T::resource_type();
    let id = resource.resource_id();
    let attrs_filter = fieldset.fields_for(rtype);
    let attrs_filter_ref: Option<&[&str]> = attrs_filter.as_deref();
    let attrs = resource.resource_attributes(attrs_filter_ref);

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

    Value::Object(data)
}
