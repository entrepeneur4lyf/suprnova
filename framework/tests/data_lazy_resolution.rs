use std::sync::Arc;
use suprnova::data::{registry, RequestIncludeSet, REQUEST_INCLUDE_SET};
use suprnova::inertia::Prop;

#[tokio::test]
async fn lazy_resolves_when_in_include_set() {
    registry::register("_test_AlbumDto_t5", &["songs"]);
    let set = Arc::new(RequestIncludeSet {
        include: vec!["songs".into()],
        ..Default::default()
    });

    let prop = Prop::lazy(|| async { serde_json::json!(["a", "b"]) });
    let resolved = REQUEST_INCLUDE_SET
        .scope(set, prop.resolve_with_owner("_test_AlbumDto_t5", "songs"))
        .await
        .unwrap();
    assert_eq!(resolved, Some(serde_json::json!(["a", "b"])));
}

#[tokio::test]
async fn lazy_skipped_when_not_in_include_set() {
    registry::register("_test_AlbumDto_t5b", &["songs"]);
    let set = Arc::new(RequestIncludeSet::default());

    let prop = Prop::lazy(|| async { serde_json::json!(["a", "b"]) });
    let resolved = REQUEST_INCLUDE_SET
        .scope(set, prop.resolve_with_owner("_test_AlbumDto_t5b", "songs"))
        .await
        .unwrap();
    assert_eq!(resolved, None);
}

#[tokio::test]
async fn lazy_disallowed_field_errors() {
    registry::register("_test_AlbumDto_t5c", &["songs"]);
    let set = Arc::new(RequestIncludeSet {
        include: vec!["lyrics".into()],
        ..Default::default()
    });

    let prop = Prop::lazy(|| async { serde_json::json!(["a", "b"]) });
    let err = REQUEST_INCLUDE_SET
        .scope(set, prop.resolve_with_owner("_test_AlbumDto_t5c", "lyrics"))
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 400);
}
