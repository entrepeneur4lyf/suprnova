//! Laravel-13 parity tests for the additional `HttpResponse` builder
//! methods: `with_headers`, `without_header`, `replace_header`,
//! `with_cookies`, `without_cookie`, and their `ResponseExt` mirrors.

use suprnova::{Cookie, HttpResponse, ResponseExt};

#[test]
fn with_headers_iter_attaches_all() {
    let resp = HttpResponse::text("ok").with_headers([
        ("X-First", "1"),
        ("X-Second", "2"),
        ("X-Third", "3"),
    ]);
    let h = resp.into_hyper();
    assert_eq!(h.headers().get("X-First").unwrap(), "1");
    assert_eq!(h.headers().get("X-Second").unwrap(), "2");
    assert_eq!(h.headers().get("X-Third").unwrap(), "3");
}

#[test]
fn without_header_removes_case_insensitive() {
    let resp = HttpResponse::text("ok")
        .header("X-Drop", "value")
        .header("X-Keep", "value")
        .without_header("x-drop");
    let h = resp.into_hyper();
    assert!(h.headers().get("X-Drop").is_none());
    assert_eq!(h.headers().get("X-Keep").unwrap(), "value");
}

#[test]
fn replace_header_collapses_duplicates() {
    let resp = HttpResponse::text("ok")
        .header("X-Dup", "old1")
        .header("X-Dup", "old2")
        .replace_header("X-Dup", "fresh");
    let h = resp.into_hyper();
    // Only one value remains, and it's the fresh one.
    let values: Vec<_> = h.headers().get_all("X-Dup").iter().collect();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0], "fresh");
}

#[test]
fn header_value_reads_back() {
    let resp = HttpResponse::text("ok").header("X-Custom", "abc");
    assert_eq!(resp.header_value("X-Custom"), Some("abc"));
    assert_eq!(resp.header_value("x-custom"), Some("abc"));
    assert_eq!(resp.header_value("X-Missing"), None);
}

#[test]
fn with_cookies_attaches_multiple_set_cookie() {
    let resp =
        HttpResponse::text("ok").with_cookies([Cookie::new("a", "1"), Cookie::new("b", "2")]);
    let h = resp.into_hyper();
    let cookies: Vec<_> = h
        .headers()
        .get_all("Set-Cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert_eq!(cookies.len(), 2);
    assert!(cookies.iter().any(|c| c.contains("a=1")));
    assert!(cookies.iter().any(|c| c.contains("b=2")));
}

#[test]
fn without_cookie_emits_max_age_zero_forget() {
    let resp = HttpResponse::text("ok").without_cookie("session");
    let h = resp.into_hyper();
    let sc = h.headers().get("Set-Cookie").unwrap().to_str().unwrap();
    assert!(sc.contains("session="));
    assert!(sc.contains("Max-Age=0"), "got: {sc}");
}

// ResponseExt trait mirrors — chainable on `Response = Result<..>`.

#[test]
fn response_ext_with_headers_chains() {
    let r: suprnova::Response = Ok(HttpResponse::text("ok"));
    let r = r.with_headers([("X-A", "1"), ("X-B", "2")]);
    let inner = match r {
        Ok(h) => h,
        Err(_) => panic!("Result was Err"),
    };
    let resp = inner.into_hyper();
    assert_eq!(resp.headers().get("X-A").unwrap(), "1");
    assert_eq!(resp.headers().get("X-B").unwrap(), "2");
}

#[test]
fn response_ext_without_header_chains() {
    let r: suprnova::Response = Ok(HttpResponse::text("ok").header("X-Drop", "x"));
    let r = r.without_header("X-Drop");
    let inner = match r {
        Ok(h) => h,
        Err(_) => panic!("Result was Err"),
    };
    let resp = inner.into_hyper();
    assert!(resp.headers().get("X-Drop").is_none());
}

#[test]
fn response_ext_cookie_and_with_cookies_chain() {
    let r: suprnova::Response = Ok(HttpResponse::text("ok"));
    let r = r
        .cookie(Cookie::new("first", "v1"))
        .with_cookies([Cookie::new("second", "v2")]);
    let inner = match r {
        Ok(h) => h,
        Err(_) => panic!("Result was Err"),
    };
    let resp = inner.into_hyper();
    let count = resp.headers().get_all("Set-Cookie").iter().count();
    assert_eq!(count, 2);
}

#[test]
fn response_ext_without_cookie_chains() {
    let r: suprnova::Response = Ok(HttpResponse::text("ok"));
    let r = r.without_cookie("session");
    let inner = match r {
        Ok(h) => h,
        Err(_) => panic!("Result was Err"),
    };
    let resp = inner.into_hyper();
    let sc = resp.headers().get("Set-Cookie").unwrap().to_str().unwrap();
    assert!(sc.contains("session="));
    assert!(sc.contains("Max-Age=0"));
}
