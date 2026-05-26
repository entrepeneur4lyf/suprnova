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
