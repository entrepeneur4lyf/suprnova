//! Tests for the `SessionConfig` knobs added in the Laravel-13 parity
//! sweep — `expire_on_close`, `cookie_domain`, `cookie_partitioned`,
//! `connection`. Each knob is exercised by building a config, building
//! the outbound session cookie via the public `Cookie` builder shape,
//! and asserting the `Set-Cookie` header value emits / omits the right
//! attributes.

use std::time::Duration;
use suprnova::Cookie;
use suprnova::session::SessionConfig;

/// Helper — build a session cookie the same way `SessionMiddleware`
/// does, exercising every config flag.
fn build_session_cookie(cfg: &SessionConfig, value: &str) -> Cookie {
    let mut cookie = Cookie::new(&cfg.cookie_name, value)
        .http_only(cfg.cookie_http_only)
        .secure(cfg.cookie_secure)
        .path(&cfg.cookie_path)
        .partitioned(cfg.cookie_partitioned);
    if !cfg.expire_on_close {
        cookie = cookie.max_age(cfg.lifetime);
    }
    if let Some(ref domain) = cfg.cookie_domain {
        cookie = cookie.domain(domain);
    }
    cookie = match cfg.cookie_same_site.to_lowercase().as_str() {
        "strict" => cookie.same_site(suprnova::SameSite::Strict),
        "none" => cookie.same_site(suprnova::SameSite::None),
        _ => cookie.same_site(suprnova::SameSite::Lax),
    };
    cookie
}

#[test]
fn default_emits_max_age_lax_secure() {
    let cfg = SessionConfig::default();
    let header = build_session_cookie(&cfg, "id1").to_header_value();
    assert!(header.contains("Max-Age=7200"), "{header}");
    assert!(header.contains("Secure"), "{header}");
    assert!(header.contains("SameSite=Lax"), "{header}");
    assert!(header.contains("HttpOnly"), "{header}");
    assert!(!header.contains("Partitioned"), "{header}");
    assert!(!header.contains("Domain="), "{header}");
}

#[test]
fn expire_on_close_omits_max_age() {
    let cfg = SessionConfig::default().expire_on_close(true);
    let header = build_session_cookie(&cfg, "id1").to_header_value();
    assert!(!header.contains("Max-Age"), "{header}");
}

#[test]
fn domain_is_emitted_when_set() {
    let cfg = SessionConfig::default().domain(".example.com");
    let header = build_session_cookie(&cfg, "id1").to_header_value();
    assert!(header.contains("Domain=.example.com"), "{header}");
}

#[test]
fn partitioned_is_emitted_when_set() {
    let cfg = SessionConfig::default().partitioned(true);
    let header = build_session_cookie(&cfg, "id1").to_header_value();
    assert!(header.contains("Partitioned"), "{header}");
}

#[test]
fn connection_round_trips() {
    let cfg = SessionConfig::default().connection("logs");
    assert_eq!(cfg.connection.as_deref(), Some("logs"));
}

#[test]
fn fluent_setters_chain() {
    let cfg = SessionConfig::new()
        .lifetime(Duration::from_secs(60))
        .cookie_name("foo")
        .secure(false)
        .remember_lifetime(Duration::from_secs(86_400))
        .domain("example.test")
        .partitioned(true)
        .expire_on_close(true)
        .connection("sessions_db");

    assert_eq!(cfg.lifetime, Duration::from_secs(60));
    assert_eq!(cfg.cookie_name, "foo");
    assert!(!cfg.cookie_secure);
    assert_eq!(cfg.remember_lifetime, Duration::from_secs(86_400));
    assert_eq!(cfg.cookie_domain.as_deref(), Some("example.test"));
    assert!(cfg.cookie_partitioned);
    assert!(cfg.expire_on_close);
    assert_eq!(cfg.connection.as_deref(), Some("sessions_db"));
}

#[test]
fn default_keeps_partitioned_off_and_no_domain() {
    let cfg = SessionConfig::default();
    assert!(!cfg.cookie_partitioned);
    assert!(cfg.cookie_domain.is_none());
    assert!(!cfg.expire_on_close);
    assert!(cfg.connection.is_none());
}
