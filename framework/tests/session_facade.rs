//! `SessionData` facade completion tests — exercises the Laravel
//! `Store` parity surface shipped by the session module: pull, push,
//! increment, decrement, remember, missing, has_any, has_all, all,
//! only, except, replace, put_many, forget_many, now, reflash, keep,
//! flash_input + old-input getters, previous-url + previous-route,
//! password_confirmed, plus the module-level `is_valid_session_id` /
//! `regenerate_csrf_token` free fns.

use std::collections::HashMap;

use suprnova::session::{
    SessionData, is_valid_session_id, new_session_slot_for_test, regenerate_csrf_token,
    session_scope_for_test,
};

fn data() -> SessionData {
    SessionData::new(
        "abcdefghij0123456789abcdefghij0123456789".to_string(),
        "csrf-token-csrf-token-csrf-token-csrf-tk".to_string(),
    )
}

#[tokio::test]
async fn pull_returns_and_forgets() {
    let mut s = data();
    s.put("name", "alice");
    assert_eq!(s.pull::<String>("name").as_deref(), Some("alice"));
    assert!(s.missing("name"));
}

#[tokio::test]
async fn pull_returns_none_when_missing() {
    let mut s = data();
    assert!(s.pull::<String>("ghost").is_none());
}

#[tokio::test]
async fn push_appends_onto_array() {
    let mut s = data();
    s.push("teams", "alpha");
    s.push("teams", "beta");
    let teams: Vec<String> = s.get("teams").unwrap();
    assert_eq!(teams, vec!["alpha", "beta"]);
}

#[tokio::test]
async fn push_replaces_non_array_with_singleton() {
    let mut s = data();
    s.put("flag", 1i64);
    s.push("flag", "ignored");
    let arr: Vec<String> = s.get("flag").unwrap();
    assert_eq!(arr, vec!["ignored"]);
}

#[tokio::test]
async fn increment_starts_from_zero_and_persists() {
    let mut s = data();
    assert_eq!(s.increment("count", 1), 1);
    assert_eq!(s.increment("count", 4), 5);
    assert_eq!(s.get::<i64>("count"), Some(5));
}

#[tokio::test]
async fn decrement_is_increment_negated() {
    let mut s = data();
    s.put("count", 10i64);
    assert_eq!(s.decrement("count", 3), 7);
}

#[tokio::test]
async fn remember_runs_default_on_miss_and_skips_on_hit() {
    let mut s = data();
    let mut calls = 0;
    let v = s.remember::<String, _>("k", || {
        calls += 1;
        "v1".to_string()
    });
    assert_eq!(v, "v1");
    assert_eq!(calls, 1);
    let v = s.remember::<String, _>("k", || {
        calls += 1;
        "v2".to_string()
    });
    assert_eq!(v, "v1"); // hit
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn missing_is_inverse_of_has() {
    let mut s = data();
    assert!(s.missing("k"));
    s.put("k", 1i64);
    assert!(!s.missing("k"));
}

#[tokio::test]
async fn has_any_short_circuits() {
    let mut s = data();
    s.put("a", 1i64);
    assert!(s.has_any(&["a", "b"]));
    assert!(!s.has_any(&["b", "c"]));
}

#[tokio::test]
async fn has_all_requires_every_key() {
    let mut s = data();
    s.put("a", 1i64);
    s.put("b", 2i64);
    assert!(s.has_all(&["a", "b"]));
    assert!(!s.has_all(&["a", "c"]));
}

#[tokio::test]
async fn all_borrows_the_data_map() {
    let mut s = data();
    s.put("k", "v");
    let all = s.all();
    assert!(all.contains_key("k"));
}

#[tokio::test]
async fn only_clones_named_subset() {
    let mut s = data();
    s.put("a", 1i64);
    s.put("b", 2i64);
    s.put("c", 3i64);
    let sub = s.only(&["a", "c"]);
    assert!(sub.contains_key("a"));
    assert!(sub.contains_key("c"));
    assert!(!sub.contains_key("b"));
}

#[tokio::test]
async fn except_clones_complement() {
    let mut s = data();
    s.put("a", 1i64);
    s.put("b", 2i64);
    let rest = s.except(&["a"]);
    assert!(!rest.contains_key("a"));
    assert!(rest.contains_key("b"));
}

#[tokio::test]
async fn replace_flushes_and_repopulates() {
    let mut s = data();
    s.put("old", 1i64);
    s.replace(&[("a", 10i64), ("b", 20i64)]);
    assert!(s.missing("old"));
    assert_eq!(s.get::<i64>("a"), Some(10));
    assert_eq!(s.get::<i64>("b"), Some(20));
}

#[tokio::test]
async fn put_many_writes_each_key() {
    let mut s = data();
    s.put_many(&[("a", 1i64), ("b", 2i64)]);
    assert_eq!(s.get::<i64>("a"), Some(1));
    assert_eq!(s.get::<i64>("b"), Some(2));
}

#[tokio::test]
async fn forget_many_drops_each_key() {
    let mut s = data();
    s.put("a", 1i64);
    s.put("b", 2i64);
    s.put("c", 3i64);
    s.forget_many(&["a", "c"]);
    assert!(s.missing("a"));
    assert!(s.missing("c"));
    assert!(s.has("b"));
}

#[tokio::test]
async fn now_flash_is_readable_this_request_and_gone_next_request() {
    let mut s = data();
    s.now("flash-now", "hello");
    // Reading via get_flash matches the per-key-old slot layout.
    assert_eq!(s.get_flash::<String>("flash-now").as_deref(), Some("hello"));
    // get_flash consumes; second call is None.
    assert!(s.get_flash::<String>("flash-now").is_none());
}

#[tokio::test]
async fn reflash_moves_old_into_new() {
    let mut s = data();
    s.flash("status", "ok");
    s.age_flash_data(); // ok now in old slot
    assert_eq!(s.get_flash::<String>("status").as_deref(), Some("ok"));
    // After consume, reflash with no flashes is a no-op.
    s.flash("again", "yes");
    s.age_flash_data();
    s.reflash();
    s.age_flash_data();
    assert_eq!(s.get_flash::<String>("again").as_deref(), Some("yes"));
}

#[tokio::test]
async fn keep_promotes_named_keys_back_to_new() {
    let mut s = data();
    s.flash("a", "1");
    s.flash("b", "2");
    s.age_flash_data(); // both now in old
    s.keep(&["a"]); // a back to new, b still in old
    s.age_flash_data(); // a moved to old, b dropped
    assert_eq!(s.get_flash::<String>("a").as_deref(), Some("1"));
    assert!(s.get_flash::<String>("b").is_none());
}

#[tokio::test]
async fn flash_input_round_trips_through_age() {
    let mut s = data();
    let mut input = HashMap::new();
    input.insert("email".to_string(), serde_json::json!("a@b.com"));
    input.insert("age".to_string(), serde_json::json!(30));
    s.flash_input(input);
    s.age_flash_data();
    assert!(s.has_old_input(None));
    assert!(s.has_old_input(Some("email")));
    assert_eq!(
        s.get_old_input::<String>("email").as_deref(),
        Some("a@b.com")
    );
    assert_eq!(s.get_old_input::<i64>("age"), Some(30));
    assert!(!s.has_old_input(Some("missing")));
}

#[tokio::test]
async fn previous_url_round_trip() {
    let mut s = data();
    assert!(!s.has_previous_uri());
    assert_eq!(s.previous_url(), None);
    s.set_previous_url("/dashboard");
    assert!(s.has_previous_uri());
    assert_eq!(s.previous_url().as_deref(), Some("/dashboard"));
}

#[tokio::test]
async fn previous_route_round_trip() {
    let mut s = data();
    s.set_previous_route("dashboard.index");
    assert_eq!(s.previous_route().as_deref(), Some("dashboard.index"));
}

#[tokio::test]
async fn password_confirmed_writes_timestamp() {
    let mut s = data();
    let before = chrono::Utc::now().timestamp();
    s.password_confirmed();
    let stamp = s.password_confirmed_at().unwrap();
    assert!(stamp >= before);
}

#[test]
fn is_valid_session_id_accepts_minted_shape() {
    let id = "abcdefghij0123456789abcdefghij0123456789";
    assert_eq!(id.len(), 40);
    assert!(is_valid_session_id(id));
}

#[test]
fn is_valid_session_id_rejects_wrong_length_and_chars() {
    assert!(!is_valid_session_id("short"));
    assert!(!is_valid_session_id(&"a".repeat(41)));
    // Uppercase is rejected — generate_session_id only emits lowercase.
    assert!(!is_valid_session_id(
        "ABCDEFGHIJ0123456789abcdefghij0123456789"
    ));
    // Underscore is non-alphanumeric.
    assert!(!is_valid_session_id(
        "abcdefghij_123456789abcdefghij0123456789"
    ));
}

#[tokio::test]
async fn regenerate_csrf_token_rotates_token_inside_scope() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot.clone(), async move {
        let prev = suprnova::session::get_csrf_token().unwrap();
        let next = regenerate_csrf_token().unwrap();
        assert_ne!(prev, next);
        assert_eq!(suprnova::session::get_csrf_token().unwrap(), next);
        // New token is 40 chars (matches generate_csrf_token shape).
        assert_eq!(next.len(), 40);
    })
    .await;
}

#[tokio::test]
async fn regenerate_csrf_token_outside_scope_returns_none() {
    // Not inside SESSION_CONTEXT — returns None rather than panic.
    assert!(regenerate_csrf_token().is_none());
}
