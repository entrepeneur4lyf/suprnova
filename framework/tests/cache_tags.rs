use std::sync::Arc;
use std::time::Duration;
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

/// Per the documented contract: a tagged write followed by an untagged
/// overwrite clears the entry's tags. A later `flush_tags` of the OLD
/// tag must NOT delete the live untagged value.
#[tokio::test]
async fn untagged_overwrite_after_tagged_write_survives_flush_of_old_tag() {
    let store = fresh().await;

    store
        .tagged_put_raw(&["users"], "u:1", "{\"v\":1}", None)
        .await
        .unwrap();
    // The forward index now lists u:1 under "users". Overwrite untagged.
    store.put_raw("u:1", "{\"v\":2}", None).await.unwrap();

    store.flush_tags(&["users"]).await.unwrap();

    assert!(
        store.has("u:1").await.unwrap(),
        "untagged value must survive a flush of the tag it once carried"
    );
    let v: Option<String> = store.get_raw("u:1").await.unwrap();
    assert_eq!(v.as_deref(), Some("{\"v\":2}"), "value preserved unchanged");
}

/// Re-tagging a key from `["a"]` to `["b"]` must drop the membership in
/// tag `a`. A later `flush_tags(["a"])` must leave the value alone.
#[tokio::test]
async fn retagging_drops_old_tag_membership() {
    let store = fresh().await;

    store.tagged_put_raw(&["a"], "k", "v1", None).await.unwrap();
    store.tagged_put_raw(&["b"], "k", "v2", None).await.unwrap();

    store.flush_tags(&["a"]).await.unwrap();
    assert!(
        store.has("k").await.unwrap(),
        "k re-tagged to b — flushing a must not touch it"
    );

    store.flush_tags(&["b"]).await.unwrap();
    assert!(
        !store.has("k").await.unwrap(),
        "flushing the current tag removes the value"
    );
}

/// `flush()` must clear the tag index as well as the value store —
/// otherwise a later `flush_tags` walks a stale forward index against a
/// fresh, unrelated keyspace.
#[tokio::test]
async fn flush_clears_tag_index_so_subsequent_flush_tags_is_clean() {
    let store = fresh().await;

    store
        .tagged_put_raw(&["users"], "u:1", "v1", None)
        .await
        .unwrap();
    store.flush().await.unwrap();

    // Recreate u:1 untagged — flush_tags("users") must not touch it.
    store.put_raw("u:1", "v2", None).await.unwrap();
    store.flush_tags(&["users"]).await.unwrap();

    assert!(
        store.has("u:1").await.unwrap(),
        "tag_index must be cleared by flush() so the stale 'users' candidate is gone"
    );
}

/// Expired tagged keys must not be silently revived by `flush_tags`
/// (the value is already gone — the validation gate sees no entry and
/// nothing is deleted) and the forward index entry must drop with the
/// flush so it can't haunt a future write.
#[tokio::test]
async fn flush_tags_skips_expired_keys() {
    let store = fresh().await;
    store
        .tagged_put_raw(&["t"], "k", "v", Some(Duration::from_millis(20)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;

    // No-op as the entry has already expired.
    store.flush_tags(&["t"]).await.unwrap();

    // Re-create the key untagged; flush_tags(["t"]) must not delete it.
    store.put_raw("k", "v2", None).await.unwrap();
    store.flush_tags(&["t"]).await.unwrap();
    assert!(
        store.has("k").await.unwrap(),
        "stale forward index entry must not delete a fresh untagged value"
    );
}

/// `forget` must drop the value AND prune its forward index entries.
/// A later re-create + flush_tags of the old tag must not delete it.
#[tokio::test]
async fn forget_prunes_tag_index() {
    let store = fresh().await;
    store.tagged_put_raw(&["t"], "k", "v", None).await.unwrap();
    assert!(store.forget("k").await.unwrap(), "k existed");

    // Recreate untagged and flush — must survive.
    store.put_raw("k", "v2", None).await.unwrap();
    store.flush_tags(&["t"]).await.unwrap();
    assert!(
        store.has("k").await.unwrap(),
        "forget must drop forward index reference so a future untagged k is safe"
    );
}
