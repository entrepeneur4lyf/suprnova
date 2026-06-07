//! Trusted-proxy gating for header-derived request accessors.
//!
//! Several `Request` accessors — `ip()`, `ips()`, `secure()`, `host()`,
//! `http_host()`, `port()` — historically honoured `X-Forwarded-*` and
//! `X-Real-IP` headers ahead of the actual TCP peer. That trust is
//! correct behind a real terminating proxy (nginx, ALB, Cloudflare)
//! that scrubs these headers from client requests and re-stamps them
//! itself; it is a security hole anywhere else, because any client
//! can mint arbitrary `X-Forwarded-For: 1.2.3.4` and the framework
//! will believe it.
//!
//! [`TrustedProxiesConfig`] is the explicit allowlist that gates this
//! trust. The default — empty allowlist — means proxy headers are
//! **ignored**: `Request::ip()` falls back to the TCP peer, `secure()`
//! to the URI scheme, `host()` to the `Host` header, and so on. The
//! operator opts in by listing proxy IPs (the addresses of the
//! terminating edge they actually run); only when the TCP peer
//! matches one of those is the request's `X-Forwarded-*` chain
//! honoured.
//!
//! ## Resolution
//!
//! [`crate::server::handle_request_with_peer`] resolves the
//! configured allowlist once per request (via `Config::get`) and
//! threads it into the [`Request`](crate::Request) builder. This
//! keeps the accessor methods pure `&self` (no global lookups) and
//! makes parallel tests trivial — bind a `TrustedProxiesConfig`
//! directly into the test's `Request` via
//! [`Request::with_trusted_proxies`](crate::Request::with_trusted_proxies)
//! without touching the global container.
//!
//! ## Configuration
//!
//! Operators wire this through [`AppConfig`](crate::config::AppConfig)
//! at boot:
//!
//! ```rust,ignore
//! use std::net::IpAddr;
//! use suprnova::{AppConfig, TrustedProxiesConfig};
//!
//! // Trust the loopback edge running our terminating nginx.
//! let trusted = TrustedProxiesConfig::with_ips([IpAddr::from([127, 0, 0, 1])]);
//! AppConfig::set_trusted_proxies(trusted);
//! ```
//!
//! ## Deployment guidance
//!
//! Deployments *not* behind a terminating proxy must leave the
//! allowlist empty — any inbound `X-Forwarded-*` from a direct client
//! is hostile. Deployments behind a real proxy must list every
//! address from which the proxy hops can reach the framework, NOT
//! every client IP; the proxy itself terminates the TCP connection.

use std::net::IpAddr;
use std::sync::Arc;

/// Allowlist of TCP peer addresses whose `X-Forwarded-*` / `X-Real-IP`
/// headers may be trusted.
///
/// The default constructor returns an **empty** allowlist — proxy
/// headers are ignored on every request. This is fail-safe: a
/// deployment that forgets to configure trusted proxies cannot have
/// its `Request::ip()` spoofed.
///
/// Internally backed by an `Arc<[IpAddr]>` so the config is cheap to
/// clone into every `Request` for the lifetime of the request
/// builder.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxiesConfig {
    proxies: Arc<[IpAddr]>,
}

impl TrustedProxiesConfig {
    /// Construct an empty allowlist — proxy headers ignored on every
    /// request. Equivalent to [`TrustedProxiesConfig::default()`].
    pub fn empty() -> Self {
        Self::default()
    }

    /// Construct an allowlist from an iterator of trusted proxy IPs.
    ///
    /// Duplicate addresses are silently deduplicated (peer-match is
    /// O(N) over the slice; trimming duplicates is free).
    pub fn with_ips<I>(ips: I) -> Self
    where
        I: IntoIterator<Item = IpAddr>,
    {
        let mut v: Vec<IpAddr> = ips.into_iter().collect();
        v.sort();
        v.dedup();
        Self {
            proxies: v.into_boxed_slice().into(),
        }
    }

    /// Whether the configured allowlist is empty. Useful for short-
    /// circuiting before the per-request peer match.
    pub fn is_empty(&self) -> bool {
        self.proxies.is_empty()
    }

    /// Whether the supplied TCP peer is in the allowlist.
    ///
    /// Used by the [`Request`](crate::Request) accessors to gate
    /// proxy-header trust on each call. A request with no recorded
    /// peer (i.e. `peer_addr == None`, common in unit tests that build
    /// a `Request` directly) is treated as untrusted.
    pub fn trusts(&self, peer: Option<IpAddr>) -> bool {
        let Some(peer) = peer else { return false };
        self.proxies.contains(&peer)
    }

    /// Read-only view of the allowlist. Order is the sort order from
    /// [`with_ips`](Self::with_ips); duplicates have been removed.
    pub fn proxies(&self) -> &[IpAddr] {
        &self.proxies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_trusts_no_peer() {
        let cfg = TrustedProxiesConfig::empty();
        assert!(!cfg.trusts(Some("127.0.0.1".parse().unwrap())));
        assert!(!cfg.trusts(None));
    }

    #[test]
    fn with_ips_trusts_listed_peers() {
        let cfg = TrustedProxiesConfig::with_ips([
            "127.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
        ]);
        assert!(cfg.trusts(Some("127.0.0.1".parse().unwrap())));
        assert!(cfg.trusts(Some("10.0.0.1".parse().unwrap())));
        assert!(!cfg.trusts(Some("8.8.8.8".parse().unwrap())));
    }

    #[test]
    fn no_peer_is_never_trusted() {
        let cfg = TrustedProxiesConfig::with_ips(["127.0.0.1".parse().unwrap()]);
        assert!(!cfg.trusts(None));
    }

    #[test]
    fn dedups_and_sorts_inputs() {
        let cfg = TrustedProxiesConfig::with_ips([
            "127.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        ]);
        assert_eq!(cfg.proxies().len(), 2);
        assert!(cfg.trusts(Some("127.0.0.1".parse().unwrap())));
        assert!(cfg.trusts(Some("10.0.0.1".parse().unwrap())));
    }

    #[test]
    fn is_empty_reflects_allowlist_state() {
        assert!(TrustedProxiesConfig::empty().is_empty());
        assert!(!TrustedProxiesConfig::with_ips(["127.0.0.1".parse().unwrap()]).is_empty());
    }
}
