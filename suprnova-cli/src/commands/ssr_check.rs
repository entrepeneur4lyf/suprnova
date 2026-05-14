//! `suprnova ssr:check` — verify the Inertia SSR worker is reachable.
//!
//! TCP-level reachability ping. Either the worker is listening on the
//! configured URL's host:port (exit 0), or it isn't (exit 1). The
//! check is deliberately protocol-agnostic — POSTing a fake page to
//! `/render` would surface false negatives when a real page renderer
//! errors on the dummy input. We just verify the worker is up.
//!
//! Use this in CI or your deploy-pipeline smoke tests:
//!
//!     suprnova ssr:start &
//!     ./wait-until.sh suprnova ssr:check
//!     # …run e2e tests…

use std::net::TcpStream;
use std::time::Duration;

/// Resolve the SSR worker URL from flag → env → default. Public for
/// test coverage of the precedence chain.
pub(crate) fn resolve_url(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("SUPRNOVA_SSR_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:13714".to_string())
}

/// Parse a URL's host and port for a TCP probe. Returns
/// `Err(reason)` for inputs we can't make sense of. We don't depend on
/// the `url` crate in `suprnova-cli`, so this is a hand-rolled parser
/// targeting the narrow `http[s]://host[:port][/path]` shape.
pub(crate) fn parse_host_port(url: &str) -> Result<(String, u16), String> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("http://") {
        ("http", r)
    } else if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else {
        return Err("URL must start with http:// or https://".into());
    };
    // Trim trailing path so just "host[:port]" remains.
    let host_port = rest.split('/').next().unwrap_or(rest);
    if host_port.is_empty() {
        return Err("missing host".into());
    }
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| format!("invalid port: {p}"))?;
            (h.to_string(), port)
        }
        None => (
            host_port.to_string(),
            if scheme == "https" { 443 } else { 80 },
        ),
    };
    if host.is_empty() {
        return Err("missing host".into());
    }
    Ok((host, port))
}

pub fn run(url: Option<String>, timeout_ms: u64) {
    let url = resolve_url(url);
    let (host, port) = match parse_host_port(&url) {
        Ok(hp) => hp,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(2);
        }
    };

    let addr = format!("{}:{}", host, port);
    let timeout = Duration::from_millis(timeout_ms);

    // Resolve and connect with a hard timeout. `to_socket_addrs` is
    // synchronous; the connect uses the timeout variant so the probe
    // doesn't wedge if the host is unreachable but DNS resolved.
    use std::net::ToSocketAddrs;
    let mut socket_addrs = match addr.to_socket_addrs() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: DNS resolution for {} failed: {e}", addr);
            std::process::exit(1);
        }
    };

    let socket = match socket_addrs.next() {
        Some(s) => s,
        None => {
            eprintln!("Error: no addresses for {}", addr);
            std::process::exit(1);
        }
    };

    match TcpStream::connect_timeout(&socket, timeout) {
        Ok(_) => {
            println!("OK: SSR worker reachable at {}", url);
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("FAIL: SSR worker not reachable at {} ({e})", url);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_url_prefers_flag() {
        let r = resolve_url(Some("http://example.com:9000".into()));
        assert_eq!(r, "http://example.com:9000");
    }

    #[test]
    fn resolve_url_falls_back_to_default() {
        if std::env::var("SUPRNOVA_SSR_URL").is_err() {
            let r = resolve_url(None);
            assert_eq!(r, "http://127.0.0.1:13714");
        }
    }

    #[test]
    fn parse_host_port_explicit_port() {
        let (h, p) = parse_host_port("http://127.0.0.1:13714").unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 13714);
    }

    #[test]
    fn parse_host_port_https_default_443() {
        let (_, p) = parse_host_port("https://ssr.example.com").unwrap();
        assert_eq!(p, 443);
    }

    #[test]
    fn parse_host_port_http_default_80() {
        let (_, p) = parse_host_port("http://ssr.example.com").unwrap();
        assert_eq!(p, 80);
    }

    #[test]
    fn parse_host_port_rejects_garbage() {
        assert!(parse_host_port("not a url").is_err());
    }
}
