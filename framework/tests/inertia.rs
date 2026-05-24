//! End-to-end tests for the Inertia response pipeline.
//!
//! Drives `InertiaResponse::resolve` directly through an in-test
//! `InertiaRequestExt` mock so the full filtering + page-object
//! materialization path is covered without booting a real server.
//! `hyper::body::Incoming` cannot be constructed outside hyper's
//! connection machinery, which is why these tests go through the
//! trait rather than `suprnova::Request` directly.
//!
//! Tier 1 shared-data tests use `TestContainer::fake()` for per-test
//! isolation — the container's Inertia registry is scoped to the
//! guard's lifetime, so tests run in parallel without seeing each
//! other's registrations.

use std::collections::HashMap;
use suprnova::{Frontend, InertiaConfig, InertiaRequestExt, InertiaResponse};

/// Minimal `InertiaRequestExt` impl for tests.
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

#[tokio::test]
async fn initial_html_visit_returns_shell_with_embedded_page_object() {
    let req = MockReq::new("/home"); // no X-Inertia header → HTML response
    let resp = InertiaResponse::new("Home")
        .with("title", "Welcome")
        .with("count", 42u32)
        .resolve(&req).await.unwrap();

    let hyper_resp = resp.into_hyper();
    assert_eq!(hyper_resp.status(), 200);

    let content_type = hyper_resp.headers().get("Content-Type").unwrap();
    assert!(content_type
        .to_str()
        .unwrap()
        .starts_with("text/html"));

    let vary = hyper_resp.headers().get("Vary").unwrap();
    assert_eq!(vary, "X-Inertia");

    let body = body_to_string(hyper_resp.into_body());
    assert!(body.contains("<!DOCTYPE html>"));
    assert!(body.contains("<title>Suprnova</title>"));
    assert!(body.contains(r#"<div id="app" data-page="#));
    // The embedded page object should reference the URL and component.
    assert!(body.contains("&quot;component&quot;:&quot;Home&quot;"));
    assert!(body.contains("&quot;url&quot;:&quot;/home&quot;"));
}

#[tokio::test]
async fn inertia_xhr_visit_returns_json_page_object() {
    let req = MockReq::new("/users").inertia();
    let resp = InertiaResponse::new("Users")
        .with("users", serde_json::json!([{"id": 1, "name": "Alice"}]))
        .resolve(&req).await.unwrap();

    let hyper_resp = resp.into_hyper();
    assert_eq!(hyper_resp.status(), 200);

    let content_type = hyper_resp.headers().get("Content-Type").unwrap();
    assert!(content_type
        .to_str()
        .unwrap()
        .starts_with("application/json"));

    assert_eq!(hyper_resp.headers().get("X-Inertia").unwrap(), "true");
    assert_eq!(hyper_resp.headers().get("Vary").unwrap(), "X-Inertia");

    let body = body_to_string(hyper_resp.into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(page["component"], "Users");
    assert_eq!(page["url"], "/users");
    assert_eq!(page["version"], "1.0");
    assert!(page["props"]["users"].is_array());
    assert!(page["props"]["errors"].is_object());
}

#[tokio::test]
async fn partial_reload_with_only_filters_props_correctly() {
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users")
        .header("X-Inertia-Partial-Data", "users");

    let resp = InertiaResponse::new("Users")
        .with("auth", serde_json::json!({"id": 1}))
        .with("users", serde_json::json!([]))
        .with("categories", serde_json::json!([]))
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let props = page["props"].as_object().unwrap();
    assert!(props.contains_key("users"));
    assert!(!props.contains_key("auth"));
    assert!(!props.contains_key("categories"));
    assert!(props.contains_key("errors")); // always present
}

#[tokio::test]
async fn partial_reload_with_except_excludes_listed_props() {
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users")
        .header("X-Inertia-Partial-Except", "auth");

    let resp = InertiaResponse::new("Users")
        .with("auth", serde_json::json!({"id": 1}))
        .with("users", serde_json::json!([]))
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let props = page["props"].as_object().unwrap();
    assert!(props.contains_key("users"));
    assert!(!props.contains_key("auth"));
}

#[tokio::test]
async fn partial_reload_except_takes_precedence_over_only() {
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users")
        .header("X-Inertia-Partial-Data", "users,auth")
        .header("X-Inertia-Partial-Except", "auth");

    let resp = InertiaResponse::new("Users")
        .with("auth", serde_json::json!({"id": 1}))
        .with("users", serde_json::json!([]))
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let props = page["props"].as_object().unwrap();
    assert!(props.contains_key("users"));
    assert!(!props.contains_key("auth"));
}

#[tokio::test]
async fn partial_reload_for_different_component_returns_all_props() {
    // Component mismatch: client says it's on "Posts", server is rendering "Users".
    // The filter is inactive — all props returned.
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Partial-Component", "Posts")
        .header("X-Inertia-Partial-Data", "users");

    let resp = InertiaResponse::new("Users")
        .with("auth", serde_json::json!({"id": 1}))
        .with("users", serde_json::json!([]))
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let props = page["props"].as_object().unwrap();
    assert!(props.contains_key("users"));
    assert!(props.contains_key("auth"));
}

#[tokio::test]
async fn always_props_bypass_partial_reload_filter() {
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users")
        .header("X-Inertia-Partial-Data", "users");

    let resp = InertiaResponse::new("Users")
        .with("users", serde_json::json!([]))
        .always("flash", serde_json::json!({"msg": "saved"}))
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let props = page["props"].as_object().unwrap();
    assert!(props.contains_key("users"));
    // `flash` is Always — appears despite not being in partial-data.
    assert!(props.contains_key("flash"));
}

#[tokio::test]
async fn html_shell_uses_per_response_title_override() {
    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .title("My Custom Page")
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    assert!(body.contains("<title>My Custom Page</title>"));
    assert!(!body.contains("<title>Suprnova</title>"));
}

#[tokio::test]
async fn html_shell_uses_config_default_title_when_no_override() {
    let cfg = InertiaConfig::new().default_title("Acme App");
    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    assert!(body.contains("<title>Acme App</title>"));
}

#[tokio::test]
async fn html_shell_for_react_includes_refresh_preamble() {
    let cfg = InertiaConfig::new().frontend(Frontend::React);
    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    assert!(body.contains("@react-refresh"));
    assert!(body.contains("__vite_plugin_react_preamble_installed__"));
    assert!(body.contains("src/main.tsx"));
}

#[tokio::test]
async fn html_shell_for_svelte_omits_react_preamble() {
    let cfg = InertiaConfig::new().frontend(Frontend::Svelte);
    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    assert!(!body.contains("@react-refresh"));
    assert!(body.contains("src/main.ts"));
}

#[tokio::test]
async fn html_shell_for_vue_omits_react_preamble() {
    let cfg = InertiaConfig::new().frontend(Frontend::Vue);
    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    assert!(!body.contains("@react-refresh"));
    assert!(body.contains("src/main.ts"));
}

#[tokio::test]
async fn production_html_shell_falls_back_to_legacy_paths_when_manifest_missing() {
    // When no manifest.json exists on disk, the framework falls back to the
    // pre-manifest hardcoded `/{assets_base_url}/main.{js,css}` shape so apps
    // produced before D20-B keep booting. A tracing::warn! fires once on
    // first read inside `InertiaConfig::vite_manifest` (not asserted here —
    // requires tracing capture).
    let cfg = InertiaConfig::new()
        .production()
        // Point at a path guaranteed not to exist.
        .manifest_path("/definitely/not/a/real/manifest.json");
    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    assert!(body.contains("/assets/main.js"));
    assert!(body.contains("/assets/main.css"));
    // Dev-only Vite scripts should NOT appear in production
    assert!(!body.contains("/@vite/client"));
    assert!(!body.contains("@react-refresh"));
}

#[tokio::test]
async fn production_html_shell_reads_vite_manifest_for_hashed_assets() {
    // D20-B regression: with a real manifest pointing entry `src/main.ts`
    // at hashed output, the prod shell emits the hashed filenames + CSS +
    // modulepreload chunks instead of the legacy `/assets/main.js` path.
    let dir = std::env::temp_dir();
    let manifest_path = dir.join(format!(
        "test-inertia-manifest-{}.json",
        uuid::Uuid::new_v4()
    ));
    let manifest = r#"{
        "src/main.ts": {
            "file": "main-Q9zSqcUL.js",
            "name": "main",
            "src": "src/main.ts",
            "isEntry": true,
            "css": ["main-3R4lN-AT.css"],
            "imports": ["_runtime-DTQbz0Cz.js"]
        },
        "_runtime-DTQbz0Cz.js": {
            "file": "runtime-DTQbz0Cz.js"
        }
    }"#;
    std::fs::write(&manifest_path, manifest).unwrap();

    let cfg = InertiaConfig::new()
        .production()
        .manifest_path(&manifest_path);

    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    std::fs::remove_file(&manifest_path).ok();

    // Hashed entry file present
    assert!(body.contains("/assets/main-Q9zSqcUL.js"),
        "body should contain hashed entry; got: {body}");
    // Hashed CSS file present
    assert!(body.contains("/assets/main-3R4lN-AT.css"),
        "body should contain hashed CSS; got: {body}");
    // Module preload for the imported runtime chunk
    assert!(body.contains("modulepreload"),
        "body should contain modulepreload tag");
    assert!(body.contains("/assets/runtime-DTQbz0Cz.js"),
        "body should contain preloaded chunk; got: {body}");
    // Legacy hardcoded paths should NOT appear
    assert!(!body.contains("/assets/main.js"));
    assert!(!body.contains("/assets/main.css"));
}

#[tokio::test]
async fn production_html_shell_respects_custom_assets_base_url() {
    // assets_base_url defaults to /assets; users can override (e.g. when
    // serving from /build or a CDN).
    let dir = std::env::temp_dir();
    let manifest_path = dir.join(format!(
        "test-inertia-manifest-{}.json",
        uuid::Uuid::new_v4()
    ));
    let manifest = r#"{
        "src/main.ts": {
            "file": "main-AAA.js",
            "isEntry": true,
            "css": []
        }
    }"#;
    std::fs::write(&manifest_path, manifest).unwrap();

    let cfg = InertiaConfig::new()
        .production()
        .manifest_path(&manifest_path)
        .assets_base_url("/build");

    let req = MockReq::new("/home");
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    std::fs::remove_file(&manifest_path).ok();

    assert!(body.contains("/build/main-AAA.js"),
        "custom base URL should prefix asset path; got: {body}");
    assert!(!body.contains("/assets/main"));
}

#[tokio::test]
async fn version_in_page_object_matches_configured_version() {
    let cfg = InertiaConfig::new().version("abc123");
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["version"], "abc123");
}

#[tokio::test]
async fn errors_prop_is_always_an_empty_object_when_unset() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    let errors = &page["props"]["errors"];
    assert!(errors.is_object());
    assert!(errors.as_object().unwrap().is_empty());
}

#[tokio::test]
async fn props_serialize_in_insertion_order_via_indexmap() {
    // serde_json's preserve_order feature + IndexMap should produce stable,
    // insertion-ordered output. The "errors" key is inserted first by the
    // resolver, then each user-added prop in order.
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Test")
        .with("zebra", 1)
        .with("apple", 2)
        .with("mango", 3)
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());

    let zebra_pos = body.find("zebra").unwrap();
    let apple_pos = body.find("apple").unwrap();
    let mango_pos = body.find("mango").unwrap();
    assert!(zebra_pos < apple_pos);
    assert!(apple_pos < mango_pos);
}

#[tokio::test]
async fn version_conflict_response_carries_x_inertia_location() {
    let resp = InertiaResponse::version_conflict("/new-location");
    let hyper_resp = resp.into_hyper();
    assert_eq!(hyper_resp.status(), 409);
    assert_eq!(
        hyper_resp.headers().get("X-Inertia-Location").unwrap(),
        "/new-location"
    );
}

#[tokio::test]
async fn page_object_url_reflects_request_path() {
    let req = MockReq::new("/users/42/edit").inertia();
    let resp = InertiaResponse::new("Users/Edit").resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["url"], "/users/42/edit");
}

#[tokio::test]
async fn xhr_response_omits_html_shell_entirely() {
    let req = MockReq::new("/home").inertia();
    let resp = InertiaResponse::new("Home")
        .with("data", "value")
        .resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    // JSON output should NOT contain any of the HTML shell markers.
    assert!(!body.contains("<!DOCTYPE html>"));
    assert!(!body.contains("<html"));
    assert!(!body.contains("data-page="));
}

#[tokio::test]
async fn resolvers_run_concurrently_not_serially() {
    // Three Lazy resolvers each sleep 80ms. Serial would be ~240ms, parallel
    // should be ~80ms. Allow generous headroom on the upper bound to avoid
    // flakiness on a loaded CI runner while still catching serialization.
    let req = MockReq::new("/").inertia();
    let start = std::time::Instant::now();
    let _ = InertiaResponse::new("Home")
        .lazy("a", || async {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            Ok::<_, suprnova::FrameworkError>(serde_json::json!("a"))
        })
        .lazy("b", || async {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            Ok::<_, suprnova::FrameworkError>(serde_json::json!("b"))
        })
        .lazy("c", || async {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            Ok::<_, suprnova::FrameworkError>(serde_json::json!("c"))
        })
        .resolve(&req)
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "parallel resolution should complete in ~80ms, took {:?}",
        elapsed
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(80),
        "should still take at least one resolver's duration, took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn html_shell_csrf_meta_tag_present_even_when_session_unset() {
    // No session => csrf_token() returns None => empty content. The
    // tag still needs to render so the frontend can read it.
    let req = MockReq::new("/");
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    assert!(body.contains(r#"<meta name="csrf-token" content="""#));
}

// ---- Tier 1: shared data, Lazy/Optional, version middleware ----

#[tokio::test]
async fn static_share_appears_in_every_inertia_response() {
    let _guard = suprnova::testing::TestContainer::fake();

    suprnova::App::inertia_share("appName", "Suprnova");

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(page["props"]["appName"], "Suprnova");

}

#[tokio::test]
async fn user_props_override_static_shared_data() {
    let _guard = suprnova::testing::TestContainer::fake();

    suprnova::App::inertia_share("title", "Shared Title");

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with("title", "Page Title")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Per the precedence chain (static → trait → user), user wins on dups.
    assert_eq!(page["props"]["title"], "Page Title");

}

#[tokio::test]
async fn shared_props_field_lists_registry_keys() {
    let _guard = suprnova::testing::TestContainer::fake();

    suprnova::App::inertia_share("appName", "Suprnova");
    suprnova::App::inertia_share("apiHost", "api.example.com");

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with("page", "home")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let shared = page["sharedProps"]
        .as_array()
        .expect("sharedProps should be an array");
    let names: Vec<&str> = shared.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"appName"));
    assert!(names.contains(&"apiHost"));
    // User-only `page` key must NOT be advertised as shared.
    assert!(!names.contains(&"page"));
}

#[tokio::test]
async fn shared_props_field_omitted_when_registry_empty() {
    let _guard = suprnova::testing::TestContainer::fake();

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with("page", "home")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        !page.as_object().unwrap().contains_key("sharedProps"),
        "sharedProps must be omitted when no shared registry entries exist"
    );
}

#[tokio::test]
async fn shared_props_includes_key_even_when_user_overrides() {
    // Per the Inertia v3 client contract, sharedProps is just a key
    // list — the client reads values from `props`. Overriding a
    // shared key with `.with()` doesn't remove the key from
    // sharedProps; the override wins in `props` and that's what the
    // client sees. Verifies the override-still-in-sharedProps contract.
    let _guard = suprnova::testing::TestContainer::fake();

    suprnova::App::inertia_share("title", "Shared Title");

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with("title", "Page Title")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(page["props"]["title"], "Page Title");
    let shared = page["sharedProps"].as_array().unwrap();
    let names: Vec<&str> = shared.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        names.contains(&"title"),
        "shared key should remain in sharedProps even when overridden"
    );
}

#[tokio::test]
async fn lazy_shared_resolves_only_when_partial_includes_key() {
    let _guard = suprnova::testing::TestContainer::fake();

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    // Unique key so we don't collide with other concurrent tests that
    // might have left state in the static registry despite SHARED_LOCK
    // (e.g. across cargo's parallel test binaries — unlikely but cheap
    // to guard against).
    let key = "expensive_lazy_test";

    suprnova::App::inertia_share_lazy(key, move || {
        let c = counter.clone();
        async move {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, suprnova::FrameworkError>(serde_json::json!({"computed": true}))
        }
    });

    // Standard visit — resolver should run (Lazy is included on standard visits).
    let req = MockReq::new("/").inertia();
    let _ = InertiaResponse::new("Home")
        .resolve(&req)
        .await
        .unwrap();
    let after_step_1 = call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(after_step_1, 1, "standard visit should resolve lazy once");

    // Partial reload excluding the key — resolver should NOT run.
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Partial-Component", "Home")
        .header("X-Inertia-Partial-Data", "other_key");
    let _ = InertiaResponse::new("Home")
        .resolve(&req)
        .await
        .unwrap();
    let after_step_2 = call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        after_step_2, 1,
        "partial reload excluding the key must not invoke the resolver"
    );

    // Partial reload that explicitly requests the key — resolver runs.
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Partial-Component", "Home")
        .header("X-Inertia-Partial-Data", key);
    let _ = InertiaResponse::new("Home")
        .resolve(&req)
        .await
        .unwrap();
    let after_step_3 = call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        after_step_3, 2,
        "explicit partial-data request must invoke the resolver"
    );

}

#[tokio::test]
async fn trait_provider_runs_with_request_context() {
    let _guard = suprnova::testing::TestContainer::fake();

    struct AuthProvider;
    #[async_trait::async_trait]
    impl suprnova::inertia::InertiaSharedData for AuthProvider {
        async fn share(
            &self,
            req: &dyn suprnova::InertiaRequestExt,
        ) -> Result<indexmap::IndexMap<String, suprnova::Prop>, suprnova::FrameworkError>
        {
            let mut m = indexmap::IndexMap::new();
            // Per-request data: read a header to derive the prop.
            let auth_header = req.header("X-Auth-User").unwrap_or("anonymous");
            m.insert(
                "auth".to_string(),
                suprnova::Prop::Eager(serde_json::json!({ "user": auth_header })),
            );
            Ok(m)
        }
    }

    suprnova::App::register_inertia_shared(std::sync::Arc::new(AuthProvider));

    let req = MockReq::new("/").inertia().header("X-Auth-User", "alice");
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["props"]["auth"]["user"], "alice");

    // Different request → different per-request data.
    let req2 = MockReq::new("/").inertia().header("X-Auth-User", "bob");
    let resp2 = InertiaResponse::new("Home").resolve(&req2).await.unwrap();
    let body2 = body_to_string(resp2.into_hyper().into_body());
    let page2: serde_json::Value = serde_json::from_str(&body2).unwrap();
    assert_eq!(page2["props"]["auth"]["user"], "bob");

}

#[tokio::test]
async fn trait_share_overrides_static_share_but_user_overrides_both() {
    let _guard = suprnova::testing::TestContainer::fake();

    suprnova::App::inertia_share("layer", "static");

    struct Trait;
    #[async_trait::async_trait]
    impl suprnova::inertia::InertiaSharedData for Trait {
        async fn share(
            &self,
            _req: &dyn suprnova::InertiaRequestExt,
        ) -> Result<indexmap::IndexMap<String, suprnova::Prop>, suprnova::FrameworkError>
        {
            let mut m = indexmap::IndexMap::new();
            m.insert(
                "layer".to_string(),
                suprnova::Prop::Eager(serde_json::Value::String("trait".into())),
            );
            Ok(m)
        }
    }
    suprnova::App::register_inertia_shared(std::sync::Arc::new(Trait));

    // No user override — trait wins over static.
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["props"]["layer"], "trait");

    // User override — user wins.
    let resp = InertiaResponse::new("Home")
        .with("layer", "user")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["props"]["layer"], "user");

}

#[tokio::test]
async fn lazy_user_prop_resolves_only_when_requested_in_partial_reload() {
    // Acquire SHARED_LOCK even though this test doesn't touch the static
    // registry: it calls `resolve()`, which reads the global registry,
    // so if another test in this binary has shared data registered we
    // don't want it to leak into ours.
    let _guard = suprnova::testing::TestContainer::fake();
    let req = MockReq::new("/posts")
        .inertia()
        .header("X-Inertia-Partial-Component", "Posts")
        .header("X-Inertia-Partial-Data", "users");

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let resp = InertiaResponse::new("Posts")
        .with("users", serde_json::json!([]))
        .lazy("posts", move || {
            let c = counter.clone();
            async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, suprnova::FrameworkError>(serde_json::json!([{"id": 1}]))
            }
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // `posts` not in partial-data → resolver not invoked, key absent.
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!page["props"].as_object().unwrap().contains_key("posts"));
    assert!(page["props"].as_object().unwrap().contains_key("users"));
}

#[tokio::test]
async fn optional_prop_excluded_on_standard_visit() {
    let _guard = suprnova::testing::TestContainer::fake();
    let req = MockReq::new("/").inertia();

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let resp = InertiaResponse::new("Home")
        .optional("permissions", move || {
            let c = counter.clone();
            async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, suprnova::FrameworkError>(serde_json::json!(["read", "write"]))
            }
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Standard visit → optional NOT included AND NOT resolved.
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!page["props"]
        .as_object()
        .unwrap()
        .contains_key("permissions"));
}

#[tokio::test]
async fn optional_prop_included_when_explicitly_requested() {
    let _guard = suprnova::testing::TestContainer::fake();
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Partial-Component", "Home")
        .header("X-Inertia-Partial-Data", "permissions");

    let resp = InertiaResponse::new("Home")
        .optional("permissions", || async {
            Ok::<_, suprnova::FrameworkError>(serde_json::json!(["read"]))
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["props"]["permissions"], serde_json::json!(["read"]));
}

#[tokio::test]
async fn lazy_resolver_error_propagates_as_framework_error() {
    let _guard = suprnova::testing::TestContainer::fake();
    let req = MockReq::new("/").inertia();

    let result = InertiaResponse::new("Home")
        .lazy("boom", || async {
            Err::<serde_json::Value, _>(suprnova::FrameworkError::internal("kaboom"))
        })
        .resolve(&req)
        .await;

    match result {
        Err(e) => assert!(e.to_string().contains("kaboom")),
        Ok(_) => panic!("expected resolver error to propagate"),
    }
}


// ---- Tier 2: flash, deferred, merge, once ----

#[tokio::test]
async fn flash_via_response_builder_emits_top_level_field() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .flash("toast", serde_json::json!({"msg": "saved"}))
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["flash"]["toast"]["msg"], "saved");
    // Not under props.
    assert!(!page["props"]
        .as_object()
        .unwrap()
        .contains_key("flash"));
}

#[tokio::test]
async fn flash_field_absent_when_no_data() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("flash"));
}

#[tokio::test]
async fn defer_on_initial_visit_is_in_deferred_props_not_props() {
    let req = MockReq::new("/").inertia();

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let resp = InertiaResponse::new("Users")
        .defer("permissions", move || {
            let c = counter.clone();
            async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, suprnova::FrameworkError>(serde_json::json!(["read", "write"]))
            }
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Resolver not called on initial visit.
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    // Not in props.
    assert!(!page["props"]
        .as_object()
        .unwrap()
        .contains_key("permissions"));
    // In deferredProps under "default" group.
    let deferred = page["deferredProps"].as_object().unwrap();
    let default_group = deferred["default"].as_array().unwrap();
    assert_eq!(default_group, &vec![serde_json::json!("permissions")]);
}

#[tokio::test]
async fn defer_partial_reload_invokes_resolver_and_lands_in_props() {
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users")
        .header("X-Inertia-Partial-Data", "permissions");

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let resp = InertiaResponse::new("Users")
        .defer("permissions", move || {
            let c = counter.clone();
            async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, suprnova::FrameworkError>(serde_json::json!(["read"]))
            }
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(page["props"]["permissions"], serde_json::json!(["read"]));
    // No deferredProps emitted for the resolved key.
    assert!(!page.as_object().unwrap().contains_key("deferredProps"));
}

#[tokio::test]
async fn defer_grouping_buckets_keys() {
    let req = MockReq::new("/").inertia();

    let resp = InertiaResponse::new("Posts")
        .defer_with(
            "teams",
            suprnova::DeferOptions::new().group("attributes"),
            || async { Ok::<_, suprnova::FrameworkError>(serde_json::json!([])) },
        )
        .defer_with(
            "projects",
            suprnova::DeferOptions::new().group("attributes"),
            || async { Ok::<_, suprnova::FrameworkError>(serde_json::json!([])) },
        )
        .defer("permissions", || async {
            Ok::<_, suprnova::FrameworkError>(serde_json::json!([]))
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let deferred = page["deferredProps"].as_object().unwrap();
    assert_eq!(
        deferred["attributes"].as_array().unwrap(),
        &vec![serde_json::json!("teams"), serde_json::json!("projects")]
    );
    assert_eq!(
        deferred["default"].as_array().unwrap(),
        &vec![serde_json::json!("permissions")]
    );
}

#[tokio::test]
async fn defer_rescue_catches_resolver_error() {
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users")
        .header("X-Inertia-Partial-Data", "permissions");

    let resp = InertiaResponse::new("Users")
        .defer_with(
            "permissions",
            suprnova::DeferOptions::new().rescue(),
            || async {
                Err::<serde_json::Value, _>(suprnova::FrameworkError::internal("boom"))
            },
        )
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Prop omitted from props
    assert!(!page["props"]
        .as_object()
        .unwrap()
        .contains_key("permissions"));
    // But listed in rescuedProps
    assert_eq!(page["rescuedProps"], serde_json::json!(["permissions"]));
}

#[tokio::test]
async fn defer_without_rescue_propagates_error() {
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users")
        .header("X-Inertia-Partial-Data", "permissions");

    let result = InertiaResponse::new("Users")
        .defer("permissions", || async {
            Err::<serde_json::Value, _>(suprnova::FrameworkError::internal("kaboom"))
        })
        .resolve(&req)
        .await;

    match result {
        Err(e) => assert!(e.to_string().contains("kaboom")),
        Ok(_) => panic!("expected error to propagate"),
    }
}

#[tokio::test]
async fn merge_emits_merge_props_and_includes_value() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Tags")
        .merge("tags", serde_json::json!(["rust", "web"]))
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["props"]["tags"], serde_json::json!(["rust", "web"]));
    assert_eq!(page["mergeProps"], serde_json::json!(["tags"]));
}

#[tokio::test]
async fn merge_prepend_emits_prepend_props() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Feed")
        .merge_prepend("notifications", serde_json::json!([]))
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["prependProps"], serde_json::json!(["notifications"]));
    assert!(!page.as_object().unwrap().contains_key("mergeProps"));
}

#[tokio::test]
async fn deep_merge_emits_deep_merge_props() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Chat")
        .deep_merge("chat", serde_json::json!({"messages": []}))
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["deepMergeProps"], serde_json::json!(["chat"]));
}

#[tokio::test]
async fn merge_with_match_on_emits_dotted_path_in_match_props_on() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Posts")
        .merge_with(
            "posts",
            serde_json::json!([{"id": 1}]),
            suprnova::MergeStrategy::Append {
                match_on: Some("id".into()),
            },
        )
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["mergeProps"], serde_json::json!(["posts"]));
    assert_eq!(page["matchPropsOn"], serde_json::json!(["posts.id"]));
}

#[tokio::test]
async fn once_first_visit_resolves_and_emits_metadata() {
    let req = MockReq::new("/").inertia();
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let resp = InertiaResponse::new("Billing")
        .once("plans", move || {
            let c = counter.clone();
            async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, suprnova::FrameworkError>(serde_json::json!([{"id": 1, "name": "Basic"}]))
            }
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(page["props"]["plans"].is_array());
    let once = page["onceProps"].as_object().unwrap();
    let entry = once["plans"].as_object().unwrap();
    assert_eq!(entry["prop"], "plans");
    assert!(entry["expiresAt"].is_null());
}

#[tokio::test]
async fn once_second_visit_skips_resolver_via_except_header() {
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Except-Once-Props", "plans");

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let resp = InertiaResponse::new("Billing")
        .once("plans", move || {
            let c = counter.clone();
            async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, suprnova::FrameworkError>(serde_json::json!([]))
            }
        })
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Resolver skipped — client claims to have it cached.
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    // Value NOT in props.
    assert!(!page["props"].as_object().unwrap().contains_key("plans"));
    // But metadata still emitted.
    assert!(page["onceProps"]["plans"].is_object());
}

#[tokio::test]
async fn once_with_fresh_ignores_except_header() {
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Except-Once-Props", "plans");

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let resp = InertiaResponse::new("Billing")
        .once_with(
            "plans",
            suprnova::OnceOptions::new().fresh(),
            move || {
                let c = counter.clone();
                async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<_, suprnova::FrameworkError>(serde_json::json!([{"id": 99}]))
                }
            },
        )
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // fresh() forces resolver to run despite the except-header.
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(page["props"]["plans"], serde_json::json!([{"id": 99}]));
}

#[tokio::test]
async fn once_with_as_key_uses_custom_cache_key() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Team")
        .once_with(
            "memberRoles",
            suprnova::OnceOptions::new().as_key("roles"),
            || async {
                Ok::<_, suprnova::FrameworkError>(serde_json::json!(["admin", "member"]))
            },
        )
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Prop name is "memberRoles", cache key is "roles".
    assert!(page["props"]["memberRoles"].is_array());
    let once = page["onceProps"].as_object().unwrap();
    assert!(once.contains_key("roles"));
    let entry = once["roles"].as_object().unwrap();
    assert_eq!(entry["prop"], "memberRoles");
}

#[tokio::test]
async fn once_with_until_emits_expires_at() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Dashboard")
        .once_with(
            "rates",
            suprnova::OnceOptions::new().until(1_700_000_000_000),
            || async { Ok::<_, suprnova::FrameworkError>(serde_json::json!({})) },
        )
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let entry = page["onceProps"]["rates"].as_object().unwrap();
    assert_eq!(entry["expiresAt"], serde_json::json!(1_700_000_000_000_i64));
}

#[tokio::test]
async fn app_flash_persists_to_response_via_task_local() {
    let _guard = suprnova::testing::TestContainer::fake();
    let req = MockReq::new("/").inertia();

    // Set up a fresh flash scope using the same pattern the server uses.
    let bag = suprnova::inertia::flash_new_bag_for_test();
    suprnova::inertia::flash_scope_for_test(bag, async move {
        suprnova::App::flash("toast", serde_json::json!({"msg": "via App::flash"}));

        let resp = InertiaResponse::new("Home")
            .resolve(&req)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["flash"]["toast"]["msg"], "via App::flash");
    })
    .await;
}

#[tokio::test]
async fn share_once_via_app_registers_once_prop() {
    let _guard = suprnova::testing::TestContainer::fake();

    suprnova::App::inertia_share_once("countries", || async {
        Ok::<_, suprnova::FrameworkError>(serde_json::json!(["US", "CA"]))
    });

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(page["props"]["countries"], serde_json::json!(["US", "CA"]));
    assert!(page["onceProps"]["countries"].is_object());
}

// ---- Tier 3: history encryption, location, 303 middleware ----

#[tokio::test]
async fn encrypt_history_per_response_emits_flag() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .encrypt_history(true)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["encryptHistory"], true);
}

#[tokio::test]
async fn encrypt_history_omitted_when_false_or_unset() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("encryptHistory"));

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .encrypt_history(false)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("encryptHistory"));
}

#[tokio::test]
async fn clear_history_emits_flag_only_when_set() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .clear_history()
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["clearHistory"], true);

    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("clearHistory"));
}

#[tokio::test]
async fn encrypt_history_per_response_overrides_config_default() {
    let cfg = suprnova::InertiaConfig::new().encrypt_history(true);
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .encrypt_history(false)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Per-response false beats config-default true.
    assert!(!page.as_object().unwrap().contains_key("encryptHistory"));
}

#[tokio::test]
async fn encrypt_history_config_default_applies_when_no_override() {
    let cfg = suprnova::InertiaConfig::new().encrypt_history(true);
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with_config(cfg)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["encryptHistory"], true);
}

#[tokio::test]
async fn inertia_location_returns_409_with_x_inertia_location() {
    let resp = InertiaResponse::location("https://example.com/external");
    let hyper_resp = resp.into_hyper();
    assert_eq!(hyper_resp.status(), 409);
    assert_eq!(
        hyper_resp.headers().get("X-Inertia-Location").unwrap(),
        "https://example.com/external"
    );
}

// ---- Tier 3.1: fragment preservation ----
//
// `preserveFragment` is a page-object flag set on the *destination*
// response of a redirect — the client (which knows its own URL hash)
// carries the fragment over to the new URL when this flag is true.
// `InertiaResponse::redirect(url)` is the X-Inertia-Redirect mechanism
// for soft Inertia redirects whose target URL may carry a `#fragment`.

#[tokio::test]
async fn preserve_fragment_true_emits_flag_in_page_object() {
    let req = MockReq::new("/article/new").inertia();
    let resp = InertiaResponse::new("Article/Show")
        .preserve_fragment(true)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["preserveFragment"], true);
}

#[tokio::test]
async fn preserve_fragment_default_does_not_emit_flag() {
    let req = MockReq::new("/article").inertia();
    let resp = InertiaResponse::new("Article/Show")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("preserveFragment"));
}

#[tokio::test]
async fn preserve_fragment_false_does_not_emit_flag() {
    let req = MockReq::new("/article").inertia();
    let resp = InertiaResponse::new("Article/Show")
        .preserve_fragment(false)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("preserveFragment"));
}

#[tokio::test]
async fn inertia_redirect_returns_409_with_x_inertia_redirect() {
    let resp = InertiaResponse::redirect("/article/new#section");
    let hyper_resp = resp.into_hyper();
    assert_eq!(hyper_resp.status(), 409);
    assert_eq!(
        hyper_resp.headers().get("X-Inertia-Redirect").unwrap(),
        "/article/new#section"
    );
    // X-Inertia-Redirect is distinct from X-Inertia-Location — only
    // one of the two should be present, per the protocol.
    assert!(hyper_resp.headers().get("X-Inertia-Location").is_none());
}

#[tokio::test]
async fn inertia_redirect_distinct_from_location() {
    // Sanity check: redirect() and location() produce different shapes.
    let redirect = InertiaResponse::redirect("/foo").into_hyper();
    let location = InertiaResponse::location("/foo").into_hyper();

    assert!(redirect.headers().get("X-Inertia-Redirect").is_some());
    assert!(redirect.headers().get("X-Inertia-Location").is_none());

    assert!(location.headers().get("X-Inertia-Redirect").is_none());
    assert!(location.headers().get("X-Inertia-Location").is_some());
}

#[tokio::test]
async fn preserve_fragment_flows_through_html_shell_data_page() {
    // Initial (non-XHR) visit returns the HTML shell with the page object
    // embedded in the `data-page` attribute on `<div id="app">`. Verify
    // `preserveFragment: true` survives that path the same as the XHR path.
    let req = MockReq::new("/article/new"); // no X-Inertia → HTML response
    let resp = InertiaResponse::new("Article/Show")
        .preserve_fragment(true)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());

    // The HTML shell HTML-escapes the JSON page object inside `data-page`.
    // `"preserveFragment":true` becomes `&quot;preserveFragment&quot;:true`.
    assert!(
        body.contains("&quot;preserveFragment&quot;:true"),
        "expected escaped preserveFragment:true in data-page; body was:\n{}",
        body
    );
}

#[tokio::test]
async fn preserve_fragment_survives_partial_reload_filter() {
    // `preserveFragment` is a top-level page-object flag, not a prop, so
    // partial-reload filtering (which only filters `props`) must not
    // affect it. Drive a partial reload with `X-Inertia-Partial-Component`
    // + `X-Inertia-Partial-Data` and verify the flag still emits.
    let req = MockReq::new("/article")
        .inertia()
        .header("X-Inertia-Partial-Component", "Article/Show")
        .header("X-Inertia-Partial-Data", "title");
    let resp = InertiaResponse::new("Article/Show")
        .preserve_fragment(true)
        .with("title", "Welcome")
        .with("body", "long content")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Partial filter limited props to `title` only.
    assert_eq!(page["props"]["title"], "Welcome");
    assert!(page["props"].as_object().unwrap().get("body").is_none());
    // …but the top-level flag is unaffected.
    assert_eq!(page["preserveFragment"], true);
}

// ---- Tier 4: SSR ----
//
// These tests spawn a tiny localhost HTTP server that mimics the
// `@inertiajs/{...}/server` SSR worker — accepts `POST /render` with
// the page object, returns `{head, body}`. We then resolve an Inertia
// response with SSR enabled pointed at this worker and inspect the
// generated HTML shell.

mod ssr_tests {
    use super::*;
    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use suprnova::{InertiaConfig, InertiaResponse};

    /// Spawn a one-shot SSR worker bound to 127.0.0.1:0. The worker
    /// always returns `head: [<title>SSR</title>]` and `body: <pre-rendered/>`.
    /// Returns the socket address it's listening on.
    async fn spawn_mock_ssr() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc =
                        service_fn(|_req: hyper::Request<hyper::body::Incoming>| async move {
                            let body = serde_json::json!({
                                "head": ["<title>SSR Title</title>", "<meta name=\"ssr\" content=\"yes\">"],
                                "body": "<main id=\"ssr\">SSR rendered content</main>",
                            });
                            let payload = serde_json::to_vec(&body).unwrap();
                            Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(payload)))
                                    .unwrap(),
                            )
                        });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn ssr_disabled_by_default_produces_empty_mount() {
        let req = MockReq::new("/"); // non-XHR initial visit
        let resp = InertiaResponse::new("Home")
            .with("title", "Hi")
            .resolve(&req)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        assert!(!body.contains("data-server-rendered"));
        assert!(body.contains("<div id=\"app\""));
    }

    #[tokio::test]
    async fn ssr_enabled_injects_head_and_body_with_data_attr() {
        let addr = spawn_mock_ssr().await;
        let cfg = InertiaConfig::new().ssr(format!("http://{}", addr));
        let req = MockReq::new("/");
        let resp = InertiaResponse::new("Home")
            .with_config(cfg)
            .with("title", "Hi")
            .resolve(&req)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());

        assert!(
            body.contains("data-server-rendered=\"true\""),
            "expected data-server-rendered on mount; body:\n{}",
            body
        );
        assert!(body.contains("<title>SSR Title</title>"));
        assert!(body.contains("<meta name=\"ssr\" content=\"yes\">"));
        assert!(body.contains("<main id=\"ssr\">SSR rendered content</main>"));
    }

    #[tokio::test]
    async fn ssr_worker_unreachable_falls_back_to_csr() {
        // Point at a port nothing is listening on. Default
        // throw_on_error=false → falls back silently.
        let cfg = InertiaConfig::new().ssr("http://127.0.0.1:1");
        let req = MockReq::new("/");
        let resp = InertiaResponse::new("Home")
            .with_config(cfg)
            .resolve(&req)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        assert!(!body.contains("data-server-rendered"));
    }

    #[tokio::test]
    async fn ssr_throw_on_error_propagates_error() {
        let cfg = InertiaConfig::new()
            .ssr("http://127.0.0.1:1")
            .ssr_throw_on_error(true);
        let req = MockReq::new("/");
        let result = InertiaResponse::new("Home")
            .with_config(cfg)
            .resolve(&req)
            .await;
        assert!(
            result.is_err(),
            "throw_on_error=true must propagate worker failure"
        );
    }

    #[tokio::test]
    async fn ssr_excluded_path_skips_worker() {
        // Even with a working worker, excluded paths render CSR.
        let addr = spawn_mock_ssr().await;
        let cfg = InertiaConfig::new()
            .ssr(format!("http://{}", addr))
            .ssr_exclude("/admin/**");
        let req = MockReq::new("/admin/users");
        let resp = InertiaResponse::new("Admin/Users")
            .with_config(cfg)
            .resolve(&req)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        assert!(!body.contains("data-server-rendered"));
    }

    #[tokio::test]
    async fn ssr_xhr_request_does_not_invoke_worker() {
        // For Inertia XHRs we return JSON, not HTML — SSR is irrelevant.
        // We bind a worker that would PANIC if called; if SSR is invoked
        // erroneously, the request stalls or errors.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handler_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called = handler_called.clone();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                called.store(true, std::sync::atomic::Ordering::SeqCst);
                drop(stream);
            }
        });
        let cfg = InertiaConfig::new().ssr(format!("http://{}", addr));
        let req = MockReq::new("/").inertia();
        let resp = InertiaResponse::new("Home")
            .with_config(cfg)
            .resolve(&req)
            .await
            .unwrap();
        let _ = body_to_string(resp.into_hyper().into_body());
        // Slack for spurious accept races: we only assert the SSR
        // handler was NOT triggered.
        assert!(
            !handler_called.load(std::sync::atomic::Ordering::SeqCst),
            "XHR responses must not contact the SSR worker"
        );
    }
}

// ---- Infinite scroll: Inertia::scroll() + scrollProps + merge intent ----

use suprnova::ScrollMetadata;

#[tokio::test]
async fn scroll_initial_visit_emits_metadata_with_reset_true() {
    let req = MockReq::new("/users").inertia();
    let resp = InertiaResponse::new("Users/Index")
        .scroll(
            "users",
            ScrollMetadata::new("page").current(1).next(2),
            serde_json::json!([{"id": 1, "name": "Alice"}]),
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Value is in props.
    assert_eq!(page["props"]["users"][0]["name"], "Alice");
    // Pagination metadata in scrollProps with reset: true (fresh load).
    let scroll = &page["scrollProps"]["users"];
    assert_eq!(scroll["pageName"], "page");
    assert_eq!(scroll["currentPage"], 1);
    assert_eq!(scroll["nextPage"], 2);
    assert_eq!(scroll["previousPage"], serde_json::Value::Null);
    assert_eq!(scroll["reset"], true);
    // No merge metadata on initial visit.
    let obj = page.as_object().unwrap();
    assert!(!obj.contains_key("mergeProps") || obj["mergeProps"].as_array().unwrap().is_empty());
    assert!(
        !obj.contains_key("prependProps")
            || obj["prependProps"].as_array().unwrap().is_empty()
    );
}

#[tokio::test]
async fn scroll_append_intent_emits_merge_props_no_reset() {
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Infinite-Scroll-Merge-Intent", "append");
    let resp = InertiaResponse::new("Users/Index")
        .scroll(
            "users",
            ScrollMetadata::new("page").current(2).next(3).previous(1),
            serde_json::json!([{"id": 21}]),
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let scroll = &page["scrollProps"]["users"];
    assert_eq!(scroll["reset"], false, "append fetch must not reset");
    let merge: Vec<&str> = page["mergeProps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(merge.contains(&"users"));
}

#[tokio::test]
async fn scroll_prepend_intent_emits_prepend_props_no_reset() {
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Infinite-Scroll-Merge-Intent", "prepend");
    let resp = InertiaResponse::new("Users/Index")
        .scroll(
            "users",
            ScrollMetadata::new("page").current(0).previous(-1).next(1),
            serde_json::json!([{"id": 0}]),
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(page["scrollProps"]["users"]["reset"], false);
    let prepend: Vec<&str> = page["prependProps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(prepend.contains(&"users"));
}

#[tokio::test]
async fn scroll_unknown_intent_treated_as_fresh() {
    // Invalid intent values (only "append" / "prepend" are valid) must
    // not be silently accepted as append — they fall back to fresh
    // (reset: true) so the client doesn't accumulate junk.
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Infinite-Scroll-Merge-Intent", "garbage");
    let resp = InertiaResponse::new("Users/Index")
        .scroll(
            "users",
            ScrollMetadata::new("page").current(1).next(2),
            serde_json::json!([]),
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["scrollProps"]["users"]["reset"], true);
}

#[tokio::test]
async fn scroll_with_async_resolver_runs_closure() {
    let req = MockReq::new("/users").inertia();
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();
    let resp = InertiaResponse::new("Users/Index")
        .scroll_with(
            "users",
            ScrollMetadata::new("page").current(1),
            move || {
                let c = counter.clone();
                async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<_, suprnova::FrameworkError>(serde_json::json!([{"id": 1}]))
                }
            },
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(page["props"]["users"][0]["id"], 1);
    assert_eq!(page["scrollProps"]["users"]["currentPage"], 1);
}

#[tokio::test]
async fn scroll_props_field_omitted_when_no_scroll_props() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home")
        .with("title", "x")
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("scrollProps"));
}

#[tokio::test]
async fn scroll_intent_wins_over_x_inertia_reset() {
    // Documented behavior: if a client sends both X-Inertia-Reset for
    // a scroll prop AND X-Inertia-Infinite-Scroll-Merge-Intent, the
    // scroll intent wins — the prop emits with merge metadata and
    // `reset: false`. X-Inertia-Reset is a regular-merge concept; for
    // scroll props the merge direction comes from the intent header.
    let req = MockReq::new("/users")
        .inertia()
        .header("X-Inertia-Partial-Component", "Users/Index")
        .header("X-Inertia-Partial-Data", "users")
        .header("X-Inertia-Reset", "users")
        .header("X-Inertia-Infinite-Scroll-Merge-Intent", "append");
    let resp = InertiaResponse::new("Users/Index")
        .scroll(
            "users",
            ScrollMetadata::new("page").current(2).next(3),
            serde_json::json!([{"id": 21}]),
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Intent wins: reset is false; key shows up in mergeProps.
    assert_eq!(page["scrollProps"]["users"]["reset"], false);
    let merge: Vec<&str> = page["mergeProps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(merge.contains(&"users"));
}

#[tokio::test]
async fn scroll_metadata_handles_string_cursor() {
    // Cursor pagination uses string identifiers, not numbers.
    let req = MockReq::new("/posts").inertia();
    let resp = InertiaResponse::new("Posts/Index")
        .scroll(
            "posts",
            ScrollMetadata::new("cursor")
                .current("c-100")
                .next("c-200")
                .previous("c-50"),
            serde_json::json!([]),
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    let scroll = &page["scrollProps"]["posts"];
    assert_eq!(scroll["pageName"], "cursor");
    assert_eq!(scroll["currentPage"], "c-100");
    assert_eq!(scroll["nextPage"], "c-200");
}

// ---- Purpose: prefetch header ----

#[tokio::test]
async fn is_prefetch_detects_purpose_header() {
    let req = MockReq::new("/").inertia().header("Purpose", "prefetch");
    assert!(req.is_prefetch());
    assert!(req.is_inertia(), "prefetch is independent of is_inertia");
}

#[tokio::test]
async fn is_prefetch_case_insensitive() {
    let req = MockReq::new("/").header("Purpose", "Prefetch");
    assert!(req.is_prefetch());
    let req = MockReq::new("/").header("Purpose", "PREFETCH");
    assert!(req.is_prefetch());
}

#[tokio::test]
async fn is_prefetch_false_when_header_missing_or_other_value() {
    let req = MockReq::new("/");
    assert!(!req.is_prefetch());
    let req = MockReq::new("/").header("Purpose", "navigation");
    assert!(!req.is_prefetch());
    let req = MockReq::new("/").header("Purpose", "");
    assert!(!req.is_prefetch());
}

// ---- X-Inertia-Error-Bag header ----

#[tokio::test]
async fn errors_default_is_flat_empty_object() {
    let req = MockReq::new("/").inertia();
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    // No error bag → flat `errors: {}` shape.
    assert!(page["props"]["errors"].is_object());
    assert!(page["props"]["errors"].as_object().unwrap().is_empty());
}

#[tokio::test]
async fn errors_scoped_under_named_bag_when_header_set() {
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Error-Bag", "registration");
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Errors are now `errors: { registration: {} }`.
    let errors = page["props"]["errors"].as_object().unwrap();
    assert!(errors.contains_key("registration"));
    assert!(errors["registration"].is_object());
}

#[tokio::test]
async fn error_bag_wraps_handler_injected_errors() {
    // Regression test: previously the bag scoping was done at the
    // start of resolve_props with an empty object, then user props
    // could overwrite it — silently losing the bag wrapping. The fix
    // moves scoping to after all props resolve.
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Error-Bag", "checkout");
    let resp = InertiaResponse::new("Home")
        .with(
            "errors",
            serde_json::json!({"email": "must be valid", "card": "expired"}),
        )
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    let errors = page["props"]["errors"].as_object().unwrap();
    assert!(
        errors.contains_key("checkout"),
        "handler-injected errors must be wrapped under bag, got: {:?}",
        errors
    );
    assert_eq!(errors["checkout"]["email"], "must be valid");
    assert_eq!(errors["checkout"]["card"], "expired");
}

#[tokio::test]
async fn empty_error_bag_header_treated_as_unset() {
    let req = MockReq::new("/")
        .inertia()
        .header("X-Inertia-Error-Bag", "  ");
    let resp = InertiaResponse::new("Home").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Whitespace-only / empty bag should fall back to flat shape.
    let errors = page["props"]["errors"].as_object().unwrap();
    assert!(errors.is_empty(), "expected flat errors, got {:?}", errors);
}

// ---- X-Inertia-Reset header ----
//
// When the client sends X-Inertia-Reset with merge-prop key names, the
// server resolves those props normally but suppresses the merge
// metadata so the client treats the response as a fresh replacement
// (not an append).

#[tokio::test]
async fn x_inertia_reset_strips_merge_metadata() {
    let req = MockReq::new("/posts")
        .inertia()
        .header("X-Inertia-Partial-Component", "Posts/Index")
        .header("X-Inertia-Partial-Data", "posts")
        .header("X-Inertia-Reset", "posts");
    let resp = InertiaResponse::new("Posts/Index")
        .merge("posts", serde_json::json!([{"id": 1}]))
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Value is present
    assert_eq!(page["props"]["posts"], serde_json::json!([{"id": 1}]));
    // …but merge metadata is suppressed because client asked for reset.
    let obj = page.as_object().unwrap();
    let merge_props = obj
        .get("mergeProps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let names: Vec<&str> = merge_props.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        !names.contains(&"posts"),
        "reset key must NOT appear in mergeProps"
    );
}

#[tokio::test]
async fn x_inertia_reset_does_not_affect_non_reset_merges() {
    let req = MockReq::new("/posts")
        .inertia()
        .header("X-Inertia-Partial-Component", "Posts/Index")
        .header("X-Inertia-Partial-Data", "posts,comments")
        .header("X-Inertia-Reset", "comments");
    let resp = InertiaResponse::new("Posts/Index")
        .merge("posts", serde_json::json!([{"id": 1}]))
        .merge("comments", serde_json::json!([{"id": 2}]))
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let merge_props: Vec<&str> = page["mergeProps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(merge_props.contains(&"posts"));
    assert!(!merge_props.contains(&"comments"));
}

#[tokio::test]
async fn x_inertia_reset_empty_header_is_noop() {
    let req = MockReq::new("/posts")
        .inertia()
        .header("X-Inertia-Partial-Component", "Posts/Index")
        .header("X-Inertia-Partial-Data", "posts")
        .header("X-Inertia-Reset", "");
    let resp = InertiaResponse::new("Posts/Index")
        .merge("posts", serde_json::json!([{"id": 1}]))
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    let merge_props: Vec<&str> = page["mergeProps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(merge_props.contains(&"posts"));
}

// ---- Cross-redirect carry: Redirect::preserve_fragment() round-trip ----
//
// These tests drive the full chain: a `Redirect::preserve_fragment()`
// chainable flashes `_inertia.preserve_fragment` to the session, and
// the next request's `InertiaResponse::resolve()` consumes the flag
// and emits `preserveFragment: true`. Each test scopes the session
// via `session_scope_for_test` (mirroring what `SessionMiddleware`
// does at runtime) so the `task_local!` slot is bound.

#[tokio::test]
async fn redirect_preserve_fragment_flashes_session_flag() {
    use suprnova::Redirect;
    use suprnova::session::{new_session_slot_for_test, session_scope_for_test};

    let slot = new_session_slot_for_test();
    session_scope_for_test(slot.clone(), async {
        let _: suprnova::Response = Redirect::to("/article/new")
            .preserve_fragment()
            .into();
    })
    .await;

    // The chainable should have set a *new* flash entry (before aging).
    let s = slot.lock().unwrap();
    let session = s.as_ref().expect("session present");
    assert!(
        session.has("_flash.new._inertia.preserve_fragment"),
        "expected new-flash entry after Redirect::preserve_fragment() conversion"
    );
}

#[tokio::test]
async fn redirect_route_preserve_fragment_flashes_session_flag() {
    // The `RedirectRouteBuilder::From<...>` impl has a separate code
    // path (it can short-circuit on missing route). Ensure
    // `.preserve_fragment()` flashes on the happy path too — they
    // share a helper but the helper must actually be called from both.
    use suprnova::Redirect;
    use suprnova::routing::register_route_name;
    use suprnova::session::{new_session_slot_for_test, session_scope_for_test};

    // Register a route with a unique name so this test doesn't collide
    // with other tests touching the process-global route registry.
    register_route_name(
        "_test_redirect_preserve_fragment_target",
        "/test/article/new",
    );

    let slot = new_session_slot_for_test();
    session_scope_for_test(slot.clone(), async {
        let resp: suprnova::Response = Redirect::route("_test_redirect_preserve_fragment_target")
            .preserve_fragment()
            .into();
        assert!(resp.is_ok(), "route should resolve");
    })
    .await;

    let s = slot.lock().unwrap();
    let session = s.as_ref().expect("session present");
    assert!(
        session.has("_flash.new._inertia.preserve_fragment"),
        "RedirectRouteBuilder::preserve_fragment must flash the same key as Redirect::preserve_fragment"
    );
}

#[tokio::test]
async fn redirect_route_missing_does_not_flash() {
    // When the route doesn't exist, From<RedirectRouteBuilder> returns
    // a 500 Err. Skipping the flash is intentional — otherwise a stray
    // `_inertia.preserve_fragment` would attach to whatever page the
    // user navigates to next.
    use suprnova::Redirect;
    use suprnova::session::{new_session_slot_for_test, session_scope_for_test};

    let slot = new_session_slot_for_test();
    session_scope_for_test(slot.clone(), async {
        let resp: suprnova::Response = Redirect::route("_test_nonexistent_route_xyz")
            .preserve_fragment()
            .into();
        assert!(resp.is_err(), "missing route should yield Err");
    })
    .await;

    let s = slot.lock().unwrap();
    let session = s.as_ref().expect("session present");
    assert!(
        !session.has("_flash.new._inertia.preserve_fragment"),
        "missing-route 500 must NOT flash a stray preserve-fragment"
    );
}

#[tokio::test]
async fn inertia_resolve_picks_up_flashed_preserve_fragment() {
    use suprnova::session::{new_session_slot_for_test, session_scope_for_test};

    let slot = new_session_slot_for_test();
    // Pre-populate as if a previous request flashed it and the session
    // middleware aged it (moving `_flash.new.*` → `_flash.old.*`).
    {
        let mut g = slot.lock().unwrap();
        let s = g.as_mut().unwrap();
        s.put("_flash.old._inertia.preserve_fragment", true);
    }
    let req = MockReq::new("/article/new").inertia();
    let page: serde_json::Value = session_scope_for_test(slot.clone(), async move {
        let resp = InertiaResponse::new("Article/Show")
            .resolve(&req)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        serde_json::from_str(&body).unwrap()
    })
    .await;
    assert_eq!(page["preserveFragment"], true);
}

#[tokio::test]
async fn per_response_false_defeats_flashed_true() {
    // Advisor's critical negative test: explicit `preserve_fragment(false)`
    // on the destination must override a flashed `true` from a redirect.
    use suprnova::session::{new_session_slot_for_test, session_scope_for_test};

    let slot = new_session_slot_for_test();
    {
        let mut g = slot.lock().unwrap();
        let s = g.as_mut().unwrap();
        s.put("_flash.old._inertia.preserve_fragment", true);
    }
    let req = MockReq::new("/article").inertia();
    let page: serde_json::Value = session_scope_for_test(slot.clone(), async move {
        let resp = InertiaResponse::new("Article/Show")
            .preserve_fragment(false)
            .resolve(&req)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        serde_json::from_str(&body).unwrap()
    })
    .await;
    assert!(
        !page.as_object().unwrap().contains_key("preserveFragment"),
        "preserve_fragment(false) must defeat a flashed true"
    );
}

#[tokio::test]
async fn flashed_preserve_fragment_is_one_shot() {
    // After one Inertia response consumes the flashed flag, the next
    // response in the same session must NOT see it again.
    use suprnova::session::{new_session_slot_for_test, session_scope_for_test};

    let slot = new_session_slot_for_test();
    {
        let mut g = slot.lock().unwrap();
        let s = g.as_mut().unwrap();
        s.put("_flash.old._inertia.preserve_fragment", true);
    }

    // First resolve consumes the flash.
    let req1 = MockReq::new("/article").inertia();
    let page1: serde_json::Value = session_scope_for_test(slot.clone(), async move {
        let resp = InertiaResponse::new("Article/Show")
            .resolve(&req1)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        serde_json::from_str(&body).unwrap()
    })
    .await;
    assert_eq!(page1["preserveFragment"], true);

    // Second resolve sees nothing (same session, but flash was drained).
    let req2 = MockReq::new("/article").inertia();
    let page2: serde_json::Value = session_scope_for_test(slot.clone(), async move {
        let resp = InertiaResponse::new("Article/Show")
            .resolve(&req2)
            .await
            .unwrap();
        let body = body_to_string(resp.into_hyper().into_body());
        serde_json::from_str(&body).unwrap()
    })
    .await;
    assert!(
        !page2.as_object().unwrap().contains_key("preserveFragment"),
        "second resolve must not see a re-emitted preserveFragment"
    );
}

#[tokio::test]
async fn no_session_scope_silently_drops_preserve_fragment_flash() {
    // Defensive: Redirect::preserve_fragment() outside a session scope
    // is a documented no-op. It must not panic. The destination
    // response (also outside session scope) sees no flag.
    use suprnova::Redirect;

    let _: suprnova::Response = Redirect::to("/x").preserve_fragment().into();
    let req = MockReq::new("/x").inertia();
    let resp = InertiaResponse::new("X").resolve(&req).await.unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!page.as_object().unwrap().contains_key("preserveFragment"));
}

#[tokio::test]
async fn three_browser_history_flags_combine_without_coupling() {
    // encryptHistory, clearHistory, preserveFragment are independent
    // top-level fields. Setting all three should emit all three with
    // value `true` and not interfere with each other.
    let req = MockReq::new("/secure").inertia();
    let resp = InertiaResponse::new("Secure/Page")
        .encrypt_history(true)
        .clear_history()
        .preserve_fragment(true)
        .resolve(&req)
        .await
        .unwrap();
    let body = body_to_string(resp.into_hyper().into_body());
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["encryptHistory"], true);
    assert_eq!(page["clearHistory"], true);
    assert_eq!(page["preserveFragment"], true);
}

// ---- version-mismatch middleware ----
//
// These tests drive the middleware directly via the Middleware trait
// rather than booting a Server. They construct a `Next` closure that
// either captures whether it was called (proceed) or returns a sentinel
// response (so the test can tell pass-through from short-circuit).

mod version_mw {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use suprnova::{HttpResponse, InertiaVersionMiddleware, Middleware};

    /// Build a `Next` that records whether it was invoked and returns a
    /// trivial 200 response when called.
    fn passthrough_next() -> (Arc<AtomicBool>, suprnova::Next) {
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let next: suprnova::Next = Arc::new(move |_req| {
            let f = f.clone();
            Box::pin(async move {
                f.store(true, Ordering::SeqCst);
                Ok(HttpResponse::text("through"))
            })
        });
        (flag, next)
    }

    // Test note: full Request construction requires `hyper::body::Incoming`
    // which can't be built outside hyper. The middleware tests therefore
    // live in this submodule with a dedicated runner that exercises the
    // middleware's logic against the actual `Request` type through a
    // minimal hyper service setup. We use `hyper::Request::builder()`
    //   + `http_body_util::Empty` as the body, then convert via
    // `Request::new` after collecting a wrapped Incoming.
    //
    // Since hyper doesn't expose a way to construct Incoming, we
    // instead test the middleware behavior end-to-end by binding a
    // tokio TCP listener on `127.0.0.1:0` and sending real HTTP
    // requests through a hyper client.
    //
    // That setup is heavier than the direct invocation pattern used for
    // the rest of these integration tests, so we use a separate test
    // fixture below.
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;

    /// Boot a one-shot HTTP server that wraps the given middleware around
    /// a fixed "fallthrough" handler, send an HTTP request to it, return
    /// the response.
    async fn drive(
        mw: InertiaVersionMiddleware,
        req: hyper::Request<Empty<Bytes>>,
    ) -> hyper::Response<Bytes> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();

        let mw = Arc::new(mw);

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            let mw = mw.clone();
            let service = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
                let mw = mw.clone();
                async move {
                    let req = suprnova::Request::new(hyper_req);
                    let (_flag, next) = passthrough_next();
                    let response = mw.handle(req, next).await;
                    let http = response.unwrap_or_else(|e| e);
                    Ok::<_, Infallible>(http.into_hyper())
                }
            });
            http1::Builder::new()
                .serve_connection(io, service)
                .await
                .ok();
        });

        // Build the request via hyper client.
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
                .await
                .unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let req = req;
        let resp = sender.send_request(req).await.unwrap();
        let (parts, body) = resp.into_parts();
        let collected = body.collect().await.unwrap();
        hyper::Response::from_parts(parts, collected.to_bytes())
    }

    fn request(method: &str, version_header: Option<&str>, inertia: bool) -> hyper::Request<Empty<Bytes>> {
        let mut b = hyper::Request::builder()
            .method(method)
            .uri("http://localhost/users");
        if inertia {
            b = b.header("X-Inertia", "true");
        }
        if let Some(v) = version_header {
            b = b.header("X-Inertia-Version", v);
        }
        b.body(Empty::<Bytes>::new()).unwrap()
    }

    // Sentinel: when the middleware proceeds, the handler returns "through".
    // When it short-circuits with 409, the body is empty.
    fn _sentinel() -> &'static str {
        "through"
    }

    #[tokio::test]
    async fn matching_version_passes_through() {
        let mw = InertiaVersionMiddleware::new("v1");
        let resp = drive(mw, request("GET", Some("v1"), true)).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body().as_ref(), b"through");
    }

    #[tokio::test]
    async fn mismatched_version_on_inertia_get_returns_409_with_location() {
        let mw = InertiaVersionMiddleware::new("v2");
        let resp = drive(mw, request("GET", Some("v1"), true)).await;
        assert_eq!(resp.status(), 409);
        let location = resp
            .headers()
            .get("X-Inertia-Location")
            .expect("X-Inertia-Location header");
        assert_eq!(location, "/users");
    }

    #[tokio::test]
    async fn mismatched_version_on_inertia_post_passes_through() {
        // Per spec, only GET mismatches trigger 409 — other methods rely on
        // their post-action GET redirect to surface the mismatch.
        let mw = InertiaVersionMiddleware::new("v2");
        let resp = drive(mw, request("POST", Some("v1"), true)).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body().as_ref(), b"through");
    }

    #[tokio::test]
    async fn non_inertia_request_passes_through_even_with_version_mismatch() {
        let mw = InertiaVersionMiddleware::new("v2");
        let resp = drive(mw, request("GET", Some("v1"), false)).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body().as_ref(), b"through");
    }

    #[tokio::test]
    async fn missing_version_header_on_inertia_get_is_treated_as_mismatch() {
        // Per spec, the client should always send X-Inertia-Version with
        // an Inertia request. A missing header is effectively an empty
        // version string, which doesn't match a configured non-empty one.
        let mw = InertiaVersionMiddleware::new("v1");
        let resp = drive(mw, request("GET", None, true)).await;
        assert_eq!(resp.status(), 409);
    }

    #[tokio::test]
    async fn missing_version_header_matches_empty_configured_version() {
        // Reverse case: server has empty version (default unset), client
        // sends no header. They match.
        let mw = InertiaVersionMiddleware::new("");
        let resp = drive(mw, request("GET", None, true)).await;
        assert_eq!(resp.status(), 200);
    }
}

// ---- helpers ----

fn body_to_string(
    body: http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>,
) -> String {
    use http_body_util::BodyExt;
    let bytes = futures_lite_block_on(async move { body.collect().await.unwrap().to_bytes() });
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Minimal block-on for collecting the response body in sync tests, without
/// pulling in the full tokio runtime (these tests don't otherwise need one).
fn futures_lite_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};
    let mut fut = pin!(fut);
    let waker = Waker::noop();
    let mut ctx = Context::from_waker(waker);
    loop {
        match fut.as_mut().poll(&mut ctx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
