//! Laravel-13 parity tests for the `Redirect` builder surface.
//!
//! Covers the new `back`, `away`, `refresh`, `intended`, `with`,
//! `with_input`, `with_errors`, `with_fragment` / `without_fragment`,
//! `with_cookies`, `with_headers`, and status-aware builders.
//!
//! Session-flashing tests run inside an isolated `session_scope_for_test`
//! and are gated `#[serial]` because they touch the process-wide
//! `SESSION_CONTEXT` task-local — a fully-parallel run can let
//! sibling tests stomp the same slot.

use serial_test::serial;
use suprnova::Redirect;
use suprnova::routing::register_route_name;
use suprnova::session::{new_session_slot_for_test, session, session_mut, session_scope_for_test};

fn into_response_url(r: impl Into<suprnova::Response>) -> String {
    let resp: suprnova::Response = r.into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    http.into_hyper()
        .headers()
        .get("Location")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .expect("Location header missing")
}

#[tokio::test]
#[serial]
async fn back_uses_session_previous_url() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        session_mut(|s| s.set_previous_url("/dashboard?tab=metrics"));
        let url = into_response_url(Redirect::back("/fallback"));
        assert_eq!(url, "/dashboard?tab=metrics");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn back_falls_back_when_session_empty() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        let url = into_response_url(Redirect::back("/home"));
        assert_eq!(url, "/home");
    })
    .await;
}

#[tokio::test]
async fn back_falls_back_when_no_session_scope() {
    // Outside any SessionMiddleware scope — back() must fall through
    // to the fallback cleanly (no panic).
    let url = into_response_url(Redirect::back("/login"));
    assert_eq!(url, "/login");
}

#[tokio::test]
#[serial]
async fn intended_pulls_and_clears_the_url() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        session_mut(|s| s.put("url.intended", "/admin/users"));
        let url = into_response_url(Redirect::intended("/home"));
        assert_eq!(url, "/admin/users");
        // The intended URL was PULLED, so a second call falls through
        // to the default.
        let url2 = into_response_url(Redirect::intended("/home"));
        assert_eq!(url2, "/home");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn set_intended_url_writes_to_session() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        Redirect::set_intended_url("/billing");
        let url = into_response_url(Redirect::intended("/home"));
        assert_eq!(url, "/billing");
    })
    .await;
}

#[tokio::test]
async fn away_is_alias_for_to() {
    let url = into_response_url(Redirect::away("https://external.example.com/path"));
    assert_eq!(url, "https://external.example.com/path");
}

#[tokio::test]
#[serial]
async fn refresh_uses_session_previous_url() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        session_mut(|s| s.set_previous_url("/current/path"));
        let url = into_response_url(Redirect::refresh());
        assert_eq!(url, "/current/path");
    })
    .await;
}

#[tokio::test]
async fn refresh_falls_back_to_root_when_no_session() {
    let url = into_response_url(Redirect::refresh());
    assert_eq!(url, "/");
}

#[tokio::test]
async fn with_fragment_appends_anchor() {
    let url = into_response_url(Redirect::to("/profile").with_fragment("about"));
    assert_eq!(url, "/profile#about");
}

#[tokio::test]
async fn with_fragment_replaces_existing_fragment() {
    let url = into_response_url(Redirect::to("/page#old").with_fragment("new"));
    assert_eq!(url, "/page#new");
}

#[tokio::test]
async fn with_fragment_accepts_leading_hash() {
    let url = into_response_url(Redirect::to("/page").with_fragment("#section"));
    assert_eq!(url, "/page#section");
}

#[tokio::test]
async fn without_fragment_strips_anchor() {
    let url = into_response_url(Redirect::to("/page#anchor").without_fragment());
    assert_eq!(url, "/page");
}

#[tokio::test]
async fn status_setter_emits_chosen_code() {
    let resp: suprnova::Response = Redirect::to("/login").status(303).into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    assert_eq!(http.status_code(), 303);
}

#[tokio::test]
async fn permanent_emits_301() {
    let resp: suprnova::Response = Redirect::to("/new-home").permanent().into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    assert_eq!(http.status_code(), 301);
}

#[tokio::test]
async fn header_attaches_to_redirect_response() {
    let resp: suprnova::Response = Redirect::to("/x")
        .header("X-Test", "yes")
        .header("X-Other", "true")
        .into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    let h = http.into_hyper();
    assert_eq!(h.headers().get("X-Test").unwrap(), "yes");
    assert_eq!(h.headers().get("X-Other").unwrap(), "true");
}

#[tokio::test]
async fn with_headers_iter_attaches_all() {
    let resp: suprnova::Response = Redirect::to("/x")
        .with_headers([("X-A", "1"), ("X-B", "2")])
        .into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    let h = http.into_hyper();
    assert_eq!(h.headers().get("X-A").unwrap(), "1");
    assert_eq!(h.headers().get("X-B").unwrap(), "2");
}

#[tokio::test]
async fn cookie_attaches_set_cookie_header() {
    let cookie = suprnova::Cookie::new("session", "abc");
    let resp: suprnova::Response = Redirect::to("/x").cookie(cookie).into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    let h = http.into_hyper();
    let sc = h.headers().get("Set-Cookie").unwrap().to_str().unwrap();
    assert!(sc.contains("session=abc"), "got: {sc}");
}

#[tokio::test]
async fn with_cookies_iterator_attaches_all() {
    let resp: suprnova::Response = Redirect::to("/x")
        .with_cookies([
            suprnova::Cookie::new("a", "1"),
            suprnova::Cookie::new("b", "2"),
        ])
        .into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    let h = http.into_hyper();
    let cookies: Vec<&str> = h
        .headers()
        .get_all("Set-Cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert_eq!(cookies.len(), 2);
    assert!(cookies.iter().any(|c| c.contains("a=1")));
    assert!(cookies.iter().any(|c| c.contains("b=2")));
}

#[tokio::test]
#[serial]
async fn with_flashes_session_value() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        let _: suprnova::Response = Redirect::to("/x").with("status", "User created").into();
        // The flash mechanism stages keys under `_flash.new.<key>`;
        // reading them back requires advancing the flash on the next
        // request. We assert the staged key directly.
        let s = session().expect("session present in scope");
        let staged: Option<String> = s.get("_flash.new.status");
        assert_eq!(staged.as_deref(), Some("User created"));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn with_input_round_trips_through_old_input() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        let _: suprnova::Response = Redirect::to("/x")
            .with_input([("email", "shawn@example.com"), ("name", "Shawn")])
            .into();
        // The flash bag stages old-input under the canonical key
        // `_flash.new._old_input`. To verify it lands where
        // `Session::get_old_input` reads, we age the flash bag (move
        // new → old) and then read via the public accessor.
        session_mut(|s| {
            s.age_flash_data();
        });
        let s = session().expect("session present in scope");
        let email: Option<String> = s.get_old_input("email");
        assert_eq!(email.as_deref(), Some("shawn@example.com"));
        let name: Option<String> = s.get_old_input("name");
        assert_eq!(name.as_deref(), Some("Shawn"));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn with_errors_stages_default_bag() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        let _: suprnova::Response = Redirect::to("/x")
            .with_errors([("email", "Must be a valid email")])
            .into();
        let s = session().expect("session present in scope");
        let bag: Option<serde_json::Value> = s.get("_flash.new.errors.default");
        let bag = bag.expect("default bag staged");
        assert_eq!(bag["email"][0], "Must be a valid email");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn with_errors_flows_into_inertia_errors_prop() {
    // Lock in the bridge: a redirect's with_errors flash is consumed
    // by the receiving page's Inertia response via the canonical
    // session.pull_errors_flash() path. Without this the documented
    // "Inertia surfaces them automatically" promise was vapor.
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        // Stage: a redirect on the previous request flashed errors.
        let _: suprnova::Response = Redirect::to("/login")
            .with_errors([("email", "Invalid")])
            .into();

        // Age: the flash queue rolls new -> old at the start of the
        // next request (SessionMiddleware does this in production).
        session_mut(|s| {
            s.age_flash_data();
        });

        // Drain: the inertia response's errors-seed path reads the
        // aged bag via SessionData::pull_errors_flash.
        let drained: serde_json::Map<String, serde_json::Value> =
            session_mut(|s| s.pull_errors_flash()).unwrap_or_default();
        let default_bag = drained
            .get("default")
            .expect("default error bag must be present after drain");
        assert_eq!(default_bag["email"][0], "Invalid");
        // Second drain must be empty — the flash is one-shot.
        let drained_twice = session_mut(|s| s.pull_errors_flash()).unwrap_or_default();
        assert!(drained_twice.is_empty(), "errors flash must be one-shot");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn with_errors_bag_writes_named_bag() {
    let slot = new_session_slot_for_test();
    session_scope_for_test(slot, async {
        let _: suprnova::Response = Redirect::to("/x")
            .with_errors_bag("login", [("password", "Required")])
            .into();
        let s = session().expect("session present in scope");
        let bag: Option<serde_json::Value> = s.get("_flash.new.errors.login");
        let bag = bag.expect("named bag staged");
        assert_eq!(bag["password"][0], "Required");
    })
    .await;
}

#[tokio::test]
async fn route_builder_with_fragment() {
    register_route_name("_test_redirect_frag", "/items/{id}");
    let resp: suprnova::Response = Redirect::route("_test_redirect_frag")
        .with("id", "7")
        .with_fragment("details")
        .into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    let url = http
        .into_hyper()
        .headers()
        .get("Location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(url, "/items/7#details");
}

#[tokio::test]
async fn route_builder_status_setter() {
    register_route_name("_test_redirect_status", "/things");
    let resp: suprnova::Response = Redirect::route("_test_redirect_status").status(307).into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    assert_eq!(http.status_code(), 307);
}

#[tokio::test]
async fn route_builder_cookies_and_headers() {
    register_route_name("_test_redirect_ch", "/dashboard");
    let resp: suprnova::Response = Redirect::route("_test_redirect_ch")
        .cookie(suprnova::Cookie::new("welcome", "yes"))
        .header("X-Trace", "abc")
        .into();
    let http = match resp {
        Ok(r) => r,
        Err(_) => panic!("redirect conversion produced Err"),
    };
    let h = http.into_hyper();
    let sc = h.headers().get("Set-Cookie").unwrap().to_str().unwrap();
    assert!(sc.contains("welcome=yes"));
    assert_eq!(h.headers().get("X-Trace").unwrap(), "abc");
}
