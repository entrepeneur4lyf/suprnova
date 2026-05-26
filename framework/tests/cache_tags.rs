use std::sync::Arc;
use suprnova::cache::InMemoryCache;
use suprnova::cache::store::CacheStore;

async fn fresh() -> Arc<dyn CacheStore> {
    Arc::new(InMemoryCache::with_prefix("test:"))
}

#[tokio::test]
async fn tagged_writes_can_be_flushed_by_tag() {
    let store = fresh().await;
    store
        .tagged_put_raw(&["users"], "u:1", "{\"id\":1}", None)
        .await
        .unwrap();
    store
        .tagged_put_raw(&["users", "active"], "u:2", "{\"id\":2}", None)
        .await
        .unwrap();
    store
        .tagged_put_raw(&["posts"], "p:1", "{\"id\":1}", None)
        .await
        .unwrap();

    assert!(store.has("u:1").await.unwrap());
    assert!(store.has("u:2").await.unwrap());
    assert!(store.has("p:1").await.unwrap());

    store.flush_tags(&["users"]).await.unwrap();

    assert!(!store.has("u:1").await.unwrap(), "u:1 removed by tag flush");
    assert!(
        !store.has("u:2").await.unwrap(),
        "u:2 removed by tag flush (it was tagged 'users')"
    );
    assert!(
        store.has("p:1").await.unwrap(),
        "p:1 untouched (different tag)"
    );
}

#[tokio::test]
async fn flushing_an_unknown_tag_is_a_noop() {
    let store = fresh().await;
    store.flush_tags(&["does-not-exist"]).await.unwrap();
}

#[tokio::test]
async fn tagged_keys_survive_normal_writes_to_the_same_key() {
    let store = fresh().await;
    // Untagged write to a key:
    store.put_raw("u:1", "{\"v\":1}", None).await.unwrap();
    // Tagged overwrite of the same key:
    store
        .tagged_put_raw(&["users"], "u:1", "{\"v\":2}", None)
        .await
        .unwrap();
    // The tag index now points at this key. Flushing the tag removes it.
    store.flush_tags(&["users"]).await.unwrap();
    assert!(!store.has("u:1").await.unwrap());
}
