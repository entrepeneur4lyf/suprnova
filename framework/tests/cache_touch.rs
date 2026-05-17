use std::sync::Arc;
use std::time::Duration;
use suprnova::cache::store::CacheStore;
use suprnova::cache::InMemoryCache;

async fn fresh() -> Arc<dyn CacheStore> {
    Arc::new(InMemoryCache::with_prefix("t:"))
}

#[tokio::test]
async fn touch_extends_ttl_without_changing_value() {
    let s = fresh().await;
    s.put_raw("k", "{\"v\":1}", Some(Duration::from_millis(80)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;

    let touched = s.touch("k", Duration::from_millis(200)).await.unwrap();
    assert!(touched, "touch should report success for an existing key");

    tokio::time::sleep(Duration::from_millis(120)).await; // total 160ms; original ttl was 80ms
    assert!(s.has("k").await.unwrap(), "key kept alive by touch");
    let val: Option<String> = s.get_raw("k").await.unwrap();
    assert_eq!(val.as_deref(), Some("{\"v\":1}"), "value unchanged by touch");
}

#[tokio::test]
async fn touch_on_missing_key_returns_false() {
    let s = fresh().await;
    let touched = s.touch("nope", Duration::from_secs(10)).await.unwrap();
    assert!(!touched);
}

#[tokio::test]
async fn touch_on_expired_key_returns_false() {
    let s = fresh().await;
    s.put_raw("k", "v", Some(Duration::from_millis(20)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let touched = s.touch("k", Duration::from_secs(10)).await.unwrap();
    assert!(!touched, "touching an expired key must report false");
}
