//! `Resource` facade and `JsonApiResponse` pending response type.

use super::builder::{JsonApiBuilder, render_resource_object};
use super::fieldset::current_fieldset;
use super::include_tree::IncludeTree;
use super::trait_def::{IncludeResolutionError, IntoJsonResource};
use crate::data::current_include_set;
use crate::http::HttpResponse;
use bytes::Bytes;

/// Pending JSON:API response. Calling `.render()` resolves the
/// envelope against the current request's include-set + fieldset.
pub struct JsonApiResponse {
    result: Result<JsonApiBuilder, IncludeResolutionError>,
}

impl JsonApiResponse {
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
        Ok(HttpResponse::bytes_body(
            Bytes::from(bytes),
            "application/vnd.api+json",
        ))
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
        let mut included = Vec::new();
        let result = dto
            .resource_included(&include_tree, &mut included)
            .map(|()| {
                let mut builder = JsonApiBuilder::single(data);
                for inc in included {
                    builder.push_included(inc);
                }
                builder
            });
        JsonApiResponse { result }
    }

    pub fn collection<T: IntoJsonResource>(dtos: Vec<T>) -> JsonApiResponse {
        let fieldset = current_fieldset();
        let include_set = current_include_set();
        let include_tree = IncludeTree::from_include_set(&include_set);
        let data: Vec<serde_json::Value> = dtos
            .iter()
            .map(|d| render_resource_object(d, &fieldset))
            .collect();
        let mut included = Vec::new();
        let mut first_err: Option<IncludeResolutionError> = None;
        for d in &dtos {
            if let Err(e) = d.resource_included(&include_tree, &mut included) {
                first_err = Some(e);
                break;
            }
        }
        let result = match first_err {
            Some(e) => Err(e),
            None => {
                let mut builder = JsonApiBuilder::collection(data);
                for inc in included {
                    builder.push_included(inc);
                }
                Ok(builder)
            }
        };
        JsonApiResponse { result }
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
        let mut included = Vec::new();
        let mut first_err: Option<IncludeResolutionError> = None;
        for d in items {
            if let Err(e) = d.resource_included(&include_tree, &mut included) {
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
                for inc in included {
                    builder.push_included(inc);
                }
                Ok(builder)
            }
        };
        JsonApiResponse { result }
    }
}
