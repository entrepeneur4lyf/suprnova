//! Integration tests for the JSON:API resource layer (P3T9).
//!
//! Tests cover: single-resource envelopes, sparse fieldsets, relationship
//! objects, compound document `included`, multi-level includes, unknown-
//! include 400 rejection, collections, pagination, and error envelopes.

use serde_json::Value;
use suprnova::{Data, Resource, Validate};

// ── Test resource types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Data, Validate)]
#[json_resource("users")]
pub struct UserResource {
    pub id: i64,
    pub email: String,
    #[data(input_only)]
    pub password: String,
}

#[derive(Debug, Clone, Data, Validate)]
#[json_resource("tags")]
pub struct TagResource {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Data, Validate)]
#[json_resource("authors")]
pub struct AuthorResource {
    pub id: i64,
    pub name: String,

    #[data(allow_include)]
    pub posts: Vec<PostResource>,
}

#[derive(Debug, Clone, Data, Validate)]
#[json_resource("posts")]
pub struct PostResource {
    pub id: i64,
    pub title: String,

    #[data(allow_include)]
    pub author: Option<AuthorResource>,

    #[data(allow_include)]
    pub tags: Vec<TagResource>,
}

fn make_post_with_author_and_tags() -> PostResource {
    PostResource {
        id: 5,
        title: "Hi".into(),
        author: Some(AuthorResource {
            id: 7,
            name: "Alice".into(),
            posts: vec![],
        }),
        tags: vec![
            TagResource {
                id: 100,
                name: "rust".into(),
            },
            TagResource {
                id: 101,
                name: "web".into(),
            },
        ],
    }
}

// ── Step 2/14: single resource envelope ───────────────────────────────────

#[tokio::test]
async fn single_resource_envelope() {
    let user = UserResource {
        id: 7,
        email: "alice@example.com".into(),
        password: "REDACTED".into(),
    };
    let response = Resource::single(user).render().await.unwrap();
    let envelope: Value = serde_json::from_slice(response.body()).unwrap();

    assert_eq!(envelope["data"]["type"], "users");
    assert_eq!(envelope["data"]["id"], "7");
    assert_eq!(envelope["data"]["attributes"]["email"], "alice@example.com");
    assert!(
        envelope["data"]["attributes"].get("password").is_none(),
        "input_only must not leak to API output"
    );
    assert!(
        envelope["data"]["attributes"].get("id").is_none(),
        "id lives at data.id, not in attributes (spec)"
    );
}

// ── Step 15/16: sparse fieldsets ─────────────────────────────────────────

#[tokio::test]
async fn sparse_fieldsets_filter_attributes_per_type() {
    use suprnova::resources::{RequestFieldsetSet, scope_fieldset};

    let user = UserResource {
        id: 1,
        email: "alice@example.com".into(),
        password: "x".into(),
    };
    // `?fields[users]=id` — "id" is the id field, so it lives at data.id,
    // not in attributes. Requesting only "id" should yield empty attributes.
    let fieldset = RequestFieldsetSet::from_query("fields[users]=id");
    let envelope = scope_fieldset(fieldset, async move {
        let resp = Resource::single(user).render().await.unwrap();
        let body: Value = serde_json::from_slice(resp.body()).unwrap();
        body
    })
    .await;

    assert!(
        envelope["data"]["attributes"]
            .as_object()
            .unwrap()
            .is_empty(),
        "fields[users]=id produces empty attributes (id lives at data.id)"
    );
}

#[tokio::test]
async fn sparse_fieldsets_allow_named_attribute() {
    use suprnova::resources::{RequestFieldsetSet, scope_fieldset};

    let user = UserResource {
        id: 1,
        email: "alice@example.com".into(),
        password: "x".into(),
    };
    let fieldset = RequestFieldsetSet::from_query("fields[users]=email");
    let envelope = scope_fieldset(fieldset, async move {
        let resp = Resource::single(user).render().await.unwrap();
        let body: Value = serde_json::from_slice(resp.body()).unwrap();
        body
    })
    .await;

    assert_eq!(envelope["data"]["attributes"]["email"], "alice@example.com");
}

#[tokio::test]
async fn no_fieldset_param_returns_all_attributes() {
    let user = UserResource {
        id: 1,
        email: "alice@example.com".into(),
        password: "x".into(),
    };
    let resp = Resource::single(user).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["data"]["attributes"]["email"], "alice@example.com");
}

// ── Codex review finding 9 — fieldset URL decoding ────────────────────────

#[tokio::test]
async fn sparse_fieldsets_decode_encoded_bracket_keys() {
    // `%5B`/`%5D` are `[`/`]` — the type-key must be recognized after
    // the percent-decode pass.
    use suprnova::resources::{RequestFieldsetSet, scope_fieldset};

    let user = UserResource {
        id: 1,
        email: "alice@example.com".into(),
        password: "x".into(),
    };
    let fieldset = RequestFieldsetSet::from_query("fields%5Busers%5D=email");
    let envelope = scope_fieldset(fieldset, async move {
        let resp = Resource::single(user).render().await.unwrap();
        let body: Value = serde_json::from_slice(resp.body()).unwrap();
        body
    })
    .await;

    assert_eq!(envelope["data"]["attributes"]["email"], "alice@example.com");
}

#[tokio::test]
async fn sparse_fieldsets_decode_encoded_comma_in_value() {
    // Value `email%2Cfoo` decodes to `email,foo`; once split on `,`,
    // the unknown field `foo` is ignored by the renderer but `email`
    // must still come through.
    use suprnova::resources::{RequestFieldsetSet, scope_fieldset};

    let user = UserResource {
        id: 1,
        email: "alice@example.com".into(),
        password: "x".into(),
    };
    let fieldset = RequestFieldsetSet::from_query("fields[users]=email%2Cfoo");
    let envelope = scope_fieldset(fieldset, async move {
        let resp = Resource::single(user).render().await.unwrap();
        let body: Value = serde_json::from_slice(resp.body()).unwrap();
        body
    })
    .await;

    assert_eq!(envelope["data"]["attributes"]["email"], "alice@example.com");
}

#[test]
fn fieldset_repeated_keys_merge_across_encoding() {
    use suprnova::resources::RequestFieldsetSet;

    let fs = RequestFieldsetSet::from_query("fields[users]=email&fields%5Busers%5D=id");
    let merged = fs.fields_for("users").expect("users present");
    // Order is preserved; we just need both to be present.
    assert!(merged.contains(&"email"));
    assert!(merged.contains(&"id"));
}

// ── Step 17/18: relationships + included + multi-level ────────────────────

#[tokio::test]
async fn relationship_emitted_with_resource_identifier() {
    let post = make_post_with_author_and_tags();
    let resp = Resource::single(post).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();

    assert_eq!(
        body["data"]["relationships"]["author"]["data"]["type"],
        "authors"
    );
    assert_eq!(body["data"]["relationships"]["author"]["data"]["id"], "7");
    let tags_arr = body["data"]["relationships"]["tags"]["data"]
        .as_array()
        .unwrap();
    assert_eq!(tags_arr.len(), 2);
    assert_eq!(tags_arr[0]["type"], "tags");
    assert_eq!(tags_arr[0]["id"], "100");
}

#[tokio::test]
async fn included_compound_doc_when_single_level_include() {
    use suprnova::{RequestIncludeSet, scope_include_set};

    let post = make_post_with_author_and_tags();
    let include_set = RequestIncludeSet::from_query("include=author,tags");
    let body = scope_include_set(include_set, async move {
        let resp = Resource::single(post).render().await.unwrap();
        let v: Value = serde_json::from_slice(resp.body()).unwrap();
        v
    })
    .await;

    let included = body["included"].as_array().unwrap();
    // 1 author + 2 tags = 3.
    assert_eq!(included.len(), 3);
}

#[tokio::test]
async fn multi_level_include_walks_dot_notation_chain() {
    use suprnova::{RequestIncludeSet, scope_include_set};

    // `?include=author.posts` walks: author (one level) → posts under author
    let post = PostResource {
        id: 5,
        title: "Hi".into(),
        author: Some(AuthorResource {
            id: 7,
            name: "Alice".into(),
            posts: vec![PostResource {
                id: 6,
                title: "Sibling post".into(),
                author: None,
                tags: vec![],
            }],
        }),
        tags: vec![],
    };
    let include_set = RequestIncludeSet::from_query("include=author.posts");
    let body = scope_include_set(include_set, async move {
        let resp = Resource::single(post).render().await.unwrap();
        let v: Value = serde_json::from_slice(resp.body()).unwrap();
        v
    })
    .await;

    let included = body["included"].as_array().unwrap();
    // author (1) + author's posts (1) = 2.
    assert_eq!(included.len(), 2);
    let types: Vec<&str> = included
        .iter()
        .map(|v| v["type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"authors"));
    assert!(types.contains(&"posts"));
}

#[tokio::test]
async fn unknown_include_returns_400_errors_envelope() {
    use suprnova::{RequestIncludeSet, scope_include_set};

    let post = make_post_with_author_and_tags();
    let include_set = RequestIncludeSet::from_query("include=forbidden_field");
    let body = scope_include_set(include_set, async move {
        let resp = Resource::single(post).render().await.unwrap();
        // 400 bad request
        assert_eq!(resp.status_code(), 400);
        let v: Value = serde_json::from_slice(resp.body()).unwrap();
        v
    })
    .await;

    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors[0]["status"], "400");
    assert!(
        errors[0]["detail"]
            .as_str()
            .unwrap()
            .contains("forbidden_field")
    );
}

#[tokio::test]
async fn included_omitted_when_no_request_include() {
    let post = make_post_with_author_and_tags();
    let resp = Resource::single(post).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert!(
        body.get("included").is_none(),
        "included absent when ?include= not requested"
    );
}

// ── Step 19/21: collection + pagination ──────────────────────────────────

#[tokio::test]
async fn collection_envelope() {
    let users = vec![
        UserResource {
            id: 1,
            email: "a@e.com".into(),
            password: "x".into(),
        },
        UserResource {
            id: 2,
            email: "b@e.com".into(),
            password: "y".into(),
        },
    ];
    let resp = Resource::collection(users).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    let arr = body["data"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["type"], "users");
    assert_eq!(arr[0]["id"], "1");
}

#[tokio::test]
async fn paginated_collection_emits_links_and_meta() {
    use suprnova::LengthAwarePaginator;

    let items = vec![
        UserResource {
            id: 1,
            email: "a@e.com".into(),
            password: "x".into(),
        },
        UserResource {
            id: 2,
            email: "b@e.com".into(),
            password: "y".into(),
        },
    ];
    let paginator = LengthAwarePaginator::new(items, 47, 10, 2).with_base_url("/api/users");
    let resp = Resource::paginated(paginator).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();

    assert_eq!(body["meta"]["pagination"]["total"], 47);
    assert_eq!(body["meta"]["pagination"]["per_page"], 10);
    assert_eq!(body["meta"]["pagination"]["current_page"], 2);
    assert!(body["links"]["first"].is_string());
    assert!(body["links"]["last"].is_string());
    assert!(body["links"]["prev"].is_string());
    assert!(body["links"]["next"].is_string());
}

// ── Step 22/24: error envelopes ───────────────────────────────────────────

#[tokio::test]
async fn validation_error_becomes_jsonapi_errors_envelope() {
    use suprnova::FrameworkError;

    let err = FrameworkError::validation("email", "email is invalid");
    let response = err.into_json_api_response();
    let body: Value = serde_json::from_slice(response.body()).unwrap();

    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["status"], "422");
    assert_eq!(errors[0]["title"], "Validation failed");
    assert_eq!(errors[0]["detail"], "email is invalid");
    assert_eq!(errors[0]["source"]["pointer"], "/data/attributes/email");
}

#[tokio::test]
async fn not_found_becomes_jsonapi_404_errors_envelope() {
    use suprnova::FrameworkError;

    let err = FrameworkError::not_found("User not found");
    let response = err.into_json_api_response();
    let body: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(body["errors"][0]["status"], "404");
    assert_eq!(body["errors"][0]["title"], "Not found");
    assert_eq!(body["errors"][0]["detail"], "User not found");
}

#[tokio::test]
async fn bad_request_error_becomes_jsonapi_400_errors_envelope() {
    use suprnova::FrameworkError;

    let err = FrameworkError::bad_request("invalid request parameter");
    let response = err.into_json_api_response();
    let body: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(body["errors"][0]["status"], "400");
    assert_eq!(body["errors"][0]["title"], "Bad request");
    assert!(
        body["errors"][0].get("source").is_none(),
        "non-validation errors have no source pointer"
    );
}

// ── Laravel-13 parity surfaces ────────────────────────────────────────────

#[tokio::test]
async fn additional_attaches_top_level_keys() {
    use serde_json::Map;

    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let mut extra = Map::new();
    extra.insert("api_version".into(), Value::String("2.0".into()));
    extra.insert("trace_id".into(), Value::String("req-7".into()));
    let resp = Resource::single(user)
        .additional(extra)
        .render()
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["api_version"], "2.0");
    assert_eq!(body["trace_id"], "req-7");
}

#[tokio::test]
async fn additional_does_not_overwrite_canonical_members() {
    use serde_json::Map;
    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let mut extra = Map::new();
    extra.insert("data".into(), Value::String("hijack".into()));
    let resp = Resource::single(user)
        .additional(extra)
        .render()
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert!(
        body["data"].is_object(),
        "data must remain the primary resource object, not the additional hijack"
    );
    assert_eq!(body["data"]["type"], "users");
}

#[tokio::test]
async fn with_meta_adds_top_level_meta_key() {
    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let resp = Resource::single(user)
        .with_meta("copyright", Value::String("(c) 2026".into()))
        .render()
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["meta"]["copyright"], "(c) 2026");
}

#[tokio::test]
async fn with_link_adds_top_level_link() {
    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let resp = Resource::single(user)
        .with_link("self", "/api/users/1")
        .render()
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["links"]["self"], "/api/users/1");
}

#[tokio::test]
async fn with_jsonapi_emits_top_level_jsonapi_member() {
    use suprnova::JsonApiInfo;
    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let info = JsonApiInfo::new()
        .with_version("1.1")
        .with_ext("https://jsonapi.org/ext/atomic");
    let resp = Resource::single(user)
        .with_jsonapi(info)
        .render()
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["jsonapi"]["version"], "1.1");
    assert_eq!(body["jsonapi"]["ext"][0], "https://jsonapi.org/ext/atomic");
}

#[tokio::test]
async fn status_overrides_http_status_code() {
    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let resp = Resource::single(user).status(201).render().await.unwrap();
    assert_eq!(resp.status_code(), 201);
}

#[tokio::test]
async fn created_is_shorthand_for_201() {
    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let resp = Resource::single(user).created().render().await.unwrap();
    assert_eq!(resp.status_code(), 201);
}

#[tokio::test]
async fn json_api_alias_yields_same_envelope_as_resource() {
    use suprnova::JsonApi;
    let user = UserResource {
        id: 9,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let resp = JsonApi::single(user).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["data"]["id"], "9");
    assert_eq!(body["data"]["type"], "users");
}

#[tokio::test]
async fn json_api_alias_collection() {
    use suprnova::JsonApi;
    let users = vec![
        UserResource {
            id: 1,
            email: "a@e.com".into(),
            password: "x".into(),
        },
        UserResource {
            id: 2,
            email: "b@e.com".into(),
            password: "y".into(),
        },
    ];
    let resp = JsonApi::collection(users).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
}

// ── Maybe / MissingValue conditional attributes ──────────────────────────

#[tokio::test]
async fn maybe_missing_drops_attribute_via_strip_pass() {
    use suprnova::resources::{Maybe, strip_missing_values};

    // Direct integration test of the renderer pass: any field that
    // serializes to a Maybe::Missing sentinel is dropped.
    let mut value = serde_json::json!({
        "kept": Maybe::present(1),
        "dropped": Maybe::<i32>::missing(),
    });
    strip_missing_values(&mut value);
    assert_eq!(value["kept"], 1);
    assert!(value.get("dropped").is_none());
}

#[tokio::test]
async fn maybe_present_serializes_to_inner_value() {
    use suprnova::Maybe;
    let m = Maybe::present("hello");
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v, Value::String("hello".into()));
}

#[tokio::test]
async fn missing_value_alias_works() {
    use suprnova::MissingValue;
    let m: MissingValue<i32> = MissingValue::missing();
    assert!(m.is_missing());
}

#[tokio::test]
async fn insert_maybe_skips_missing_in_handcrafted_attributes() {
    use suprnova::resources::{Maybe, insert_maybe};
    let mut map = serde_json::Map::new();
    insert_maybe(&mut map, "email", Maybe::present("a@e.com"));
    insert_maybe(&mut map, "phone", Maybe::<&str>::missing());
    let v = Value::Object(map);
    assert_eq!(v["email"], "a@e.com");
    assert!(v.get("phone").is_none());
}

// ── Per-resource links + meta via trait override ──────────────────────────

#[derive(Debug, Clone)]
struct ManualPost {
    id: i64,
    title: String,
}

impl suprnova::resources::IntoJsonResource for ManualPost {
    fn resource_type() -> &'static str {
        "posts"
    }

    fn resource_id(&self) -> String {
        self.id.to_string()
    }

    fn resource_attributes(&self, _fieldset: Option<&[&str]>) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("title".into(), Value::String(self.title.clone()));
        Value::Object(m)
    }

    fn resource_relationships(&self) -> Vec<(String, suprnova::resources::RelationshipValue)> {
        Vec::new()
    }

    fn resource_included(
        &self,
        _t: &suprnova::resources::IncludeTree,
        _o: &mut Vec<Value>,
    ) -> Result<(), suprnova::resources::IncludeResolutionError> {
        Ok(())
    }

    fn resource_links(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert(
            "self".into(),
            Value::String(format!("/api/posts/{}", self.id)),
        );
        m
    }

    fn resource_meta(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("kind".into(), Value::String("blog".into()));
        m
    }
}

#[tokio::test]
async fn resource_links_emitted_per_resource() {
    let p = ManualPost {
        id: 42,
        title: "Hello".into(),
    };
    let resp = Resource::single(p).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["data"]["links"]["self"], "/api/posts/42");
}

#[tokio::test]
async fn resource_meta_emitted_per_resource() {
    let p = ManualPost {
        id: 42,
        title: "Hello".into(),
    };
    let resp = Resource::single(p).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["data"]["meta"]["kind"], "blog");
}

#[tokio::test]
async fn empty_resource_links_member_omitted() {
    // Default impl returns empty — no `links` key on the resource
    // object means the spec's "may" stays "absent".
    let user = UserResource {
        id: 1,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let resp = Resource::single(user).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert!(
        body["data"].get("links").is_none(),
        "links absent when resource_links is empty (the default)"
    );
    assert!(
        body["data"].get("meta").is_none(),
        "meta absent when resource_meta is empty (the default)"
    );
}

// ── resource_top_level_meta wiring ────────────────────────────────────────

#[derive(Debug, Clone)]
struct PostWithTopMeta {
    id: i64,
}

impl suprnova::resources::IntoJsonResource for PostWithTopMeta {
    fn resource_type() -> &'static str {
        "posts"
    }
    fn resource_id(&self) -> String {
        self.id.to_string()
    }
    fn resource_attributes(&self, _: Option<&[&str]>) -> Value {
        Value::Object(serde_json::Map::new())
    }
    fn resource_relationships(&self) -> Vec<(String, suprnova::resources::RelationshipValue)> {
        Vec::new()
    }
    fn resource_included(
        &self,
        _t: &suprnova::resources::IncludeTree,
        _o: &mut Vec<Value>,
    ) -> Result<(), suprnova::resources::IncludeResolutionError> {
        Ok(())
    }
    fn resource_top_level_meta(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("generated_at".into(), Value::String("2026-05-29".into()));
        m
    }
}

#[tokio::test]
async fn resource_top_level_meta_lifts_to_envelope_meta() {
    let p = PostWithTopMeta { id: 1 };
    let resp = Resource::single(p).render().await.unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["meta"]["generated_at"], "2026-05-29");
}

#[tokio::test]
async fn chainable_meta_link_status_combine() {
    let user = UserResource {
        id: 5,
        email: "a@e.com".into(),
        password: "x".into(),
    };
    let resp = Resource::single(user)
        .with_meta("v", Value::from(1))
        .with_link("self", "/api/users/5")
        .status(201)
        .render()
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(resp.status_code(), 201);
    assert_eq!(body["meta"]["v"], 1);
    assert_eq!(body["links"]["self"], "/api/users/5");
}
