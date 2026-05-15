//! Integration tests for the composition of `?include=` (DTO lazy resolution)
//! and `X-Inertia-Partial-Data` (Inertia partial-reload filtering).
//!
//! Rule: `X-Inertia-Partial-Data` is a *pre-resolution* gate applied by
//! `PartialFilter::should_include_eager` before the lazy closure is called.
//! `?include=` / `resolve_with_owner` is a *second* gate applied inside the
//! closure itself (via `REQUEST_INCLUDE_SET`). A prop must pass BOTH gates to
//! appear in the response.

use std::collections::HashMap;
use std::sync::Arc;

use suprnova::data::{registry, RequestIncludeSet, REQUEST_INCLUDE_SET};
use suprnova::inertia::Prop;
use suprnova::{InertiaRequestExt, InertiaResponse};

// ---------------------------------------------------------------------------
// Test request fixture — mirrors the MockReq used in framework/tests/inertia.rs
// ---------------------------------------------------------------------------

struct MockReq {
    path: String,
    headers: HashMap<String, String>,
}

impl MockReq {
    fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            headers: HashMap::new(),
        }
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name.to_string(), value.to_string());
        self
    }

    fn inertia(self) -> Self {
        self.header("X-Inertia", "true")
    }
}

impl InertiaRequestExt for MockReq {
    fn path(&self) -> &str {
        &self.path
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Body reader helper — equivalent to the one in tests/inertia.rs
// ---------------------------------------------------------------------------

async fn body_to_string(
    body: http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>,
) -> String {
    use http_body_util::BodyExt;
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ---------------------------------------------------------------------------
// Test A: partial-data filters AFTER ?include= resolves the lazy field.
//
// ?include=albums (via task-local) → resolver would run.
// X-Inertia-Partial-Data: name → only "name" passes the partial-data gate.
// Result: "name" present, "albums" absent (partial-data pre-gates it out
// before the include-set check even runs).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partial_data_filters_after_include_resolves() {
    registry::register("_test_ArtistDto_t6a", &["albums"]);

    let set = Arc::new(RequestIncludeSet {
        include: vec!["albums".into()],
        ..Default::default()
    });

    let req = MockReq::new("/artist/1")
        .inertia()
        .header("X-Inertia-Partial-Component", "Artist/Show")
        .header("X-Inertia-Partial-Data", "name");

    let resp = REQUEST_INCLUDE_SET
        .scope(
            set,
            InertiaResponse::new("Artist/Show")
                .with("name", "Beethoven")
                .prop_lazy_with_owner(
                    "_test_ArtistDto_t6a",
                    "albums",
                    Prop::lazy(|| async { serde_json::json!(["Symphony 9"]) }),
                )
                .resolve(&req),
        )
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body()).await;
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // "name" is in X-Inertia-Partial-Data → included.
    assert!(
        page["props"]["name"].is_string(),
        "expected 'name' prop to be present, got: {:?}",
        page["props"]
    );
    assert_eq!(page["props"]["name"], "Beethoven");

    // "albums" is NOT in X-Inertia-Partial-Data → partial-data gate excludes it
    // before the include-set resolver even runs.
    assert!(
        page["props"]["albums"].is_null(),
        "expected 'albums' prop to be absent (partial-data filtered), got: {:?}",
        page["props"]
    );
}

// ---------------------------------------------------------------------------
// Test B: no partial-data header → full include-resolved set returned.
//
// ?include=albums (via task-local), no X-Inertia-Partial-Data.
// Both "name" (eager) and "albums" (lazy-owned, in include set) appear.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_partial_data_returns_full_include_resolved_set() {
    registry::register("_test_ArtistDto_t6b", &["albums"]);

    let set = Arc::new(RequestIncludeSet {
        include: vec!["albums".into()],
        ..Default::default()
    });

    // No X-Inertia-Partial-Data header — full set returned.
    let req = MockReq::new("/artist/1").inertia();

    let resp = REQUEST_INCLUDE_SET
        .scope(
            set,
            InertiaResponse::new("Artist/Show")
                .with("name", "Beethoven")
                .prop_lazy_with_owner(
                    "_test_ArtistDto_t6b",
                    "albums",
                    Prop::lazy(|| async { serde_json::json!(["Symphony 9"]) }),
                )
                .resolve(&req),
        )
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body()).await;
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Both props present — no partial-data filter active.
    assert_eq!(
        page["props"]["name"], "Beethoven",
        "expected 'name' prop, got: {:?}",
        page["props"]
    );
    assert_eq!(
        page["props"]["albums"],
        serde_json::json!(["Symphony 9"]),
        "expected 'albums' prop resolved from include set, got: {:?}",
        page["props"]
    );
}

// ---------------------------------------------------------------------------
// Test C: both ?include=albums AND X-Inertia-Partial-Data: albums.
//
// The two filters agree on "albums" → it is present.
// "name" is an eager prop but excluded by partial-data.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partial_data_and_include_both_request_same_key() {
    registry::register("_test_ArtistDto_t6c", &["albums"]);

    let set = Arc::new(RequestIncludeSet {
        include: vec!["albums".into()],
        ..Default::default()
    });

    let req = MockReq::new("/artist/1")
        .inertia()
        .header("X-Inertia-Partial-Component", "Artist/Show")
        .header("X-Inertia-Partial-Data", "albums");

    let resp = REQUEST_INCLUDE_SET
        .scope(
            set,
            InertiaResponse::new("Artist/Show")
                .with("name", "Beethoven")
                .prop_lazy_with_owner(
                    "_test_ArtistDto_t6c",
                    "albums",
                    Prop::lazy(|| async { serde_json::json!(["Symphony 9"]) }),
                )
                .resolve(&req),
        )
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body()).await;
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // "albums" passes both gates — present.
    assert_eq!(
        page["props"]["albums"],
        serde_json::json!(["Symphony 9"]),
        "expected 'albums' prop when both partial-data and include agree, got: {:?}",
        page["props"]
    );

    // "name" is NOT in X-Inertia-Partial-Data → excluded.
    assert!(
        page["props"]["name"].is_null(),
        "expected 'name' prop to be absent (partial-data restricts to 'albums'), got: {:?}",
        page["props"]
    );
}
