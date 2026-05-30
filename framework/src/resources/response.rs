//! `Resource` facade and `JsonApiResponse` pending response type.

use super::builder::{IncludedSink, JsonApiBuilder, render_resource_object};
use super::fieldset::current_fieldset;
use super::include_tree::IncludeTree;
use super::jsonapi_info::JsonApiInfo;
use super::trait_def::{IncludeResolutionError, IntoJsonResource};
use crate::data::current_include_set;
use crate::http::HttpResponse;
use bytes::Bytes;
use serde_json::{Map, Value};

/// Pending JSON:API response. Calling `.render()` resolves the
/// envelope against the current request's include-set + fieldset.
///
/// Chainable mutators mirror Laravel's `JsonResource`:
/// - `.additional(map)` — adds top-level keys alongside `data`
/// - `.with_meta(key, value)` — adds to top-level `meta`
/// - `.with_link(rel, href)` — adds to top-level `links`
/// - `.with_jsonapi(info)` — sets the top-level `jsonapi` member
/// - `.status(code)` — overrides the response HTTP status
pub struct JsonApiResponse {
    result: Result<JsonApiBuilder, IncludeResolutionError>,
    status: u16,
}

impl JsonApiResponse {
    pub(crate) fn from_result(result: Result<JsonApiBuilder, IncludeResolutionError>) -> Self {
        Self {
            result,
            status: 200,
        }
    }

    /// Override the HTTP status code (e.g. `201` after a successful
    /// `POST`). Default is `200`. Laravel parity: `wasRecentlyCreated`
    /// auto-201; we expose it as an explicit mutator instead because
    /// resource DTOs are decoupled from Eloquent model lifecycle.
    pub fn status(mut self, code: u16) -> Self {
        self.status = code;
        self
    }

    /// Set the response HTTP status to `201 Created`. Convenience
    /// shorthand for `.status(201)`. Mirrors Laravel's
    /// `ResourceResponse::calculateStatus` auto-201 behaviour.
    pub fn created(self) -> Self {
        self.status(201)
    }

    /// Add a top-level `meta` key/value (spec §5.1.2). Laravel-shape
    /// name. Multiple calls accumulate. Calling without a prior render
    /// failure is the only safe path; on a poisoned response this
    /// becomes a no-op.
    pub fn with_meta(mut self, key: impl Into<String>, value: Value) -> Self {
        if let Ok(b) = self.result {
            self.result = Ok(b.with_meta(key, value));
        }
        self
    }

    /// Alias for [`Self::with_meta`] — Laravel often uses bare `meta()`.
    pub fn meta(self, key: impl Into<String>, value: Value) -> Self {
        self.with_meta(key, value)
    }

    /// Merge a whole `meta` map at once. Equivalent to repeated
    /// `with_meta(k, v)` calls.
    pub fn with_meta_map(mut self, map: Map<String, Value>) -> Self {
        if let Ok(b) = self.result {
            self.result = Ok(b.with_meta_map(map));
        }
        self
    }

    /// Add a top-level `links` rel/href (spec §5.1.3). Laravel parity:
    /// the per-document `links` member set by `with($request)`.
    pub fn with_link(mut self, rel: impl Into<String>, href: impl Into<String>) -> Self {
        if let Ok(b) = self.result {
            self.result = Ok(b.with_link(rel, href));
        }
        self
    }

    /// Alias for [`Self::with_link`].
    pub fn link(self, rel: impl Into<String>, href: impl Into<String>) -> Self {
        self.with_link(rel, href)
    }

    /// Add a top-level `links` rel as an arbitrary `Value` — useful
    /// when the link is the JSON:API link-object form `{href, meta}`
    /// rather than a bare URL string.
    pub fn with_link_value(mut self, rel: impl Into<String>, value: Value) -> Self {
        if let Ok(b) = self.result {
            self.result = Ok(b.with_link_value(rel, value));
        }
        self
    }

    /// Attach an arbitrary key at the envelope root, alongside `data`,
    /// `included`, etc. Mirrors Laravel's `JsonResource::additional`.
    /// Use sparingly; canonical members (`data`, `included`, `links`,
    /// `meta`, `jsonapi`, `errors`) are not overwritten.
    pub fn additional(mut self, map: Map<String, Value>) -> Self {
        if let Ok(b) = self.result {
            self.result = Ok(b.with_additional_map(map));
        }
        self
    }

    /// Single-key form of [`Self::additional`].
    pub fn with_additional(mut self, key: impl Into<String>, value: Value) -> Self {
        if let Ok(b) = self.result {
            self.result = Ok(b.with_additional(key, value));
        }
        self
    }

    /// Set the top-level `jsonapi` member (spec §5.1.1). Laravel parity:
    /// `JsonApiResource::configure(version, ext, profile, meta)`.
    pub fn with_jsonapi(mut self, info: JsonApiInfo) -> Self {
        if let Ok(b) = self.result {
            self.result = Ok(b.with_jsonapi(info.to_value()));
        }
        self
    }

    /// Materialise into an HTTP response. Reads `RequestIncludeSet`
    /// and `RequestFieldsetSet` from task-locals. If any requested
    /// `?include=` path was unresolvable, returns a JSON:API 400
    /// errors envelope.
    pub async fn render(self) -> Result<HttpResponse, crate::FrameworkError> {
        let builder = match self.result {
            Ok(b) => b,
            Err(e) => {
                return Ok(crate::FrameworkError::bad_request(format!(
                    "include path '{}' is not allowed on type '{}'",
                    e.path, e.on_type
                ))
                .into_json_api_response());
            }
        };
        let body = builder.build();
        let bytes = serde_json::to_vec(&body)
            .map_err(|e| crate::FrameworkError::internal(format!("JSON:API encode: {e}")))?;
        Ok(
            HttpResponse::bytes_body(Bytes::from(bytes), "application/vnd.api+json")
                .status(self.status),
        )
    }
}

/// Public facade. Construction:
/// - `Resource::single(dto)` — one resource
/// - `Resource::collection(dtos)` — array of resources
/// - `Resource::paginated(paginator)` — paginated collection with auto links/meta
pub struct Resource;

impl Resource {
    pub fn single<T: IntoJsonResource>(dto: T) -> JsonApiResponse {
        let fieldset = current_fieldset();
        let include_set = current_include_set();
        let include_tree = IncludeTree::from_include_set(&include_set);
        let data = render_resource_object(&dto, &fieldset);
        let mut sink = IncludedSink::new();
        let top_level_meta = dto.resource_top_level_meta();
        let result = dto.resource_included(&include_tree, &mut sink).map(|()| {
            let mut builder = JsonApiBuilder::single(data);
            builder.absorb_included_sink(sink);
            if !top_level_meta.is_empty() {
                builder = builder.with_meta_map(top_level_meta);
            }
            builder
        });
        JsonApiResponse::from_result(result)
    }

    pub fn collection<T: IntoJsonResource>(dtos: Vec<T>) -> JsonApiResponse {
        let fieldset = current_fieldset();
        let include_set = current_include_set();
        let include_tree = IncludeTree::from_include_set(&include_set);
        let data: Vec<serde_json::Value> = dtos
            .iter()
            .map(|d| render_resource_object(d, &fieldset))
            .collect();
        let mut sink = IncludedSink::new();
        let mut first_err: Option<IncludeResolutionError> = None;
        for d in &dtos {
            if let Err(e) = d.resource_included(&include_tree, &mut sink) {
                first_err = Some(e);
                break;
            }
        }
        let result = match first_err {
            Some(e) => Err(e),
            None => {
                let mut builder = JsonApiBuilder::collection(data);
                builder.absorb_included_sink(sink);
                // Collections take their top-level meta from the first
                // item, mirroring Laravel's `with($request)` semantics
                // — every item in an `AnonymousResourceCollection` is
                // the same class, so calling `resource_top_level_meta`
                // on item 0 is a faithful single-source representative.
                if let Some(first) = dtos.first() {
                    let m = first.resource_top_level_meta();
                    if !m.is_empty() {
                        builder = builder.with_meta_map(m);
                    }
                }
                Ok(builder)
            }
        };
        JsonApiResponse::from_result(result)
    }

    /// Paginated collection — pulls items off the paginator, builds
    /// `data`, attaches `links.{self,first,prev,next,last}` and
    /// `meta.pagination` per JSON:API recommendation.
    pub fn paginated<T: IntoJsonResource, P: crate::pagination::Paginated<T>>(
        paginator: P,
    ) -> JsonApiResponse {
        let fieldset = current_fieldset();
        let include_set = current_include_set();
        let include_tree = IncludeTree::from_include_set(&include_set);
        let items = paginator.items();
        let data: Vec<serde_json::Value> = items
            .iter()
            .map(|d| render_resource_object(d, &fieldset))
            .collect();
        let mut sink = IncludedSink::new();
        let mut first_err: Option<IncludeResolutionError> = None;
        for d in items {
            if let Err(e) = d.resource_included(&include_tree, &mut sink) {
                first_err = Some(e);
                break;
            }
        }
        let result = match first_err {
            Some(e) => Err(e),
            None => {
                let mut builder = JsonApiBuilder::collection(data)
                    .with_meta_kv("pagination", paginator.meta_value());
                for (rel, href) in paginator.links_iter() {
                    builder = builder.with_link(rel, href);
                }
                builder.absorb_included_sink(sink);
                if let Some(first) = items.first() {
                    let m = first.resource_top_level_meta();
                    if !m.is_empty() {
                        builder = builder.with_meta_map(m);
                    }
                }
                Ok(builder)
            }
        };
        JsonApiResponse::from_result(result)
    }
}

/// Laravel-shape facade alias for [`Resource`]. Both names construct
/// the same `JsonApiResponse`.
///
/// ```ignore
/// // These two calls are identical:
/// let r = JsonApi::single(user);
/// let r = Resource::single(user);
/// ```
pub struct JsonApi;

impl JsonApi {
    #[inline]
    pub fn single<T: IntoJsonResource>(dto: T) -> JsonApiResponse {
        Resource::single(dto)
    }

    #[inline]
    pub fn collection<T: IntoJsonResource>(dtos: Vec<T>) -> JsonApiResponse {
        Resource::collection(dtos)
    }

    #[inline]
    pub fn paginated<T: IntoJsonResource, P: crate::pagination::Paginated<T>>(
        paginator: P,
    ) -> JsonApiResponse {
        Resource::paginated(paginator)
    }
}
