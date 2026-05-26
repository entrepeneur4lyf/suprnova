//! Cookie handling for suprnova framework
//!
//! Provides Laravel-like cookie API with secure defaults.

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use std::collections::HashMap;
use std::time::Duration;

/// Bytes that must be percent-encoded when serializing a cookie name or
/// value into a Set-Cookie header per RFC 6265 cookie-octet rules.
///
/// `CONTROLS` covers 0x00–0x1F + 0x7F, so CR (`\r`), LF (`\n`), NUL, and
/// every other ASCII control character is encoded — closing the
/// header-injection class of bugs where an attacker-controlled cookie
/// name or value containing CRLF would split the response.
///
/// On top of CONTROLS we add the cookie-octet exclusions from RFC 6265
/// §4.1.1 (whitespace, `"`, `,`, `;`, `\`, `%`) plus the gen-delims and
/// sub-delims so non-ASCII bytes and reserved URL characters also get
/// percent-encoded.
const COOKIE_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// SameSite cookie attribute
#[derive(Clone, Debug, PartialEq, Default)]
pub enum SameSite {
    Strict,
    #[default]
    Lax,
    None,
}

/// Cookie options with secure defaults
#[derive(Clone, Debug)]
pub struct CookieOptions {
    pub http_only: bool,
    pub secure: bool,
    pub same_site: SameSite,
    pub path: String,
    pub domain: Option<String>,
    pub max_age: Option<Duration>,
}

impl Default for CookieOptions {
    fn default() -> Self {
        Self {
            http_only: true,
            secure: true,
            same_site: SameSite::Lax,
            path: "/".to_string(),
            domain: None,
            max_age: None,
        }
    }
}

/// Cookie builder with fluent API
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::Cookie;
/// use std::time::Duration;
///
/// let cookie = Cookie::new("session", "abc123")
///     .http_only(true)
///     .secure(true)
///     .max_age(Duration::from_secs(3600));
/// ```
#[derive(Clone, Debug)]
pub struct Cookie {
    name: String,
    value: String,
    options: CookieOptions,
}

impl Cookie {
    /// Create a new cookie with the given name and value
    ///
    /// Default options:
    /// - HttpOnly: true
    /// - Secure: true
    /// - SameSite: Lax
    /// - Path: "/"
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            options: CookieOptions::default(),
        }
    }

    /// Get the cookie name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the cookie value
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Set the HttpOnly flag (default: true)
    ///
    /// HttpOnly cookies are not accessible via JavaScript, protecting against XSS.
    pub fn http_only(mut self, value: bool) -> Self {
        self.options.http_only = value;
        self
    }

    /// Set the Secure flag (default: true)
    ///
    /// Secure cookies are only sent over HTTPS connections.
    pub fn secure(mut self, value: bool) -> Self {
        self.options.secure = value;
        self
    }

    /// Set the SameSite attribute (default: Lax)
    ///
    /// Controls when the cookie is sent with cross-site requests.
    pub fn same_site(mut self, value: SameSite) -> Self {
        self.options.same_site = value;
        self
    }

    /// Set the cookie's max age
    ///
    /// The cookie will expire after this duration.
    pub fn max_age(mut self, duration: Duration) -> Self {
        self.options.max_age = Some(duration);
        self
    }

    /// Set the cookie path (default: "/")
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.options.path = path.into();
        self
    }

    /// Set the cookie domain
    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.options.domain = Some(domain.into());
        self
    }

    /// Build the Set-Cookie header value
    pub fn to_header_value(&self) -> String {
        let mut parts = vec![format!(
            "{}={}",
            url_encode(&self.name),
            url_encode(&self.value)
        )];

        parts.push(format!("Path={}", self.options.path));

        if self.options.http_only {
            parts.push("HttpOnly".to_string());
        }

        if self.options.secure {
            parts.push("Secure".to_string());
        }

        match self.options.same_site {
            SameSite::Strict => parts.push("SameSite=Strict".to_string()),
            SameSite::Lax => parts.push("SameSite=Lax".to_string()),
            SameSite::None => parts.push("SameSite=None".to_string()),
        }

        if let Some(ref domain) = self.options.domain {
            parts.push(format!("Domain={}", domain));
        }

        if let Some(max_age) = self.options.max_age {
            parts.push(format!("Max-Age={}", max_age.as_secs()));
        }

        parts.join("; ")
    }

    /// Create a cookie that deletes itself (for logout)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let forget = Cookie::forget("session");
    /// response.cookie(forget)
    /// ```
    pub fn forget(name: impl Into<String>) -> Self {
        Self::new(name, "")
            .max_age(Duration::from_secs(0))
            .http_only(true)
            .secure(true)
    }

    /// Create a permanent cookie (5 years)
    pub fn forever(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(name, value).max_age(Duration::from_secs(5 * 365 * 24 * 60 * 60))
    }

    /// Build a cookie whose value is the AES-256-GCM ciphertext of
    /// `plaintext`, base64-url-no-pad encoded. Requires
    /// `Crypt::is_initialized()`.
    ///
    /// # Errors
    ///
    /// Returns a `FrameworkError::Internal` if encryption fails (most
    /// commonly because `Crypt` has not been initialized — `APP_KEY` not
    /// set at server boot).
    pub fn encrypted(
        name: impl Into<String>,
        plaintext: impl AsRef<str>,
    ) -> Result<Self, crate::FrameworkError> {
        let wire = crate::crypto::Crypt::encrypt_string(plaintext.as_ref())?;
        Ok(Self::new(name, wire))
    }

    /// Decrypt a cookie value produced by [`Self::encrypted`]. Returns
    /// the UTF-8 plaintext.
    pub fn read_encrypted(wire: &str) -> Result<String, crate::FrameworkError> {
        crate::crypto::Crypt::decrypt_string(wire)
    }
}

/// Parse cookies from a Cookie header value
///
/// # Example
///
/// ```rust,ignore
/// let cookies = parse_cookies("session=abc123; user_id=42");
/// assert_eq!(cookies.get("session"), Some(&"abc123".to_string()));
/// ```
pub fn parse_cookies(header: &str) -> HashMap<String, String> {
    header
        .split(';')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let mut parts = part.splitn(2, '=');
            let name = parts.next()?.trim();
            let value = parts.next().unwrap_or("").trim();
            Some((url_decode(name), url_decode(value)))
        })
        .collect()
}

/// Percent-encode cookie names and values per [`COOKIE_ENCODE`].
///
/// The previous hand-rolled version only encoded ASCII printables and
/// passed CR/LF, control characters, and non-ASCII bytes through
/// unchanged — a header-injection class bug. Routing through
/// `percent_encoding::utf8_percent_encode` guarantees:
///
/// - Every CTL byte (including CR `\r`, LF `\n`) is percent-encoded.
/// - Every non-ASCII byte (UTF-8 sequences) is percent-encoded.
/// - Cookie-octet exclusions from RFC 6265 §4.1.1 are percent-encoded.
fn url_encode(s: &str) -> String {
    utf8_percent_encode(s, COOKIE_ENCODE).to_string()
}

/// Percent-decode a cookie name or value.
///
/// Multi-byte UTF-8 sequences (e.g. `%C3%A9` for `é`) round-trip
/// correctly: `percent_decode_str` accumulates bytes first, then the
/// `decode_utf8_lossy` call interprets the byte buffer as UTF-8. The
/// previous hand-rolled version pushed each decoded byte as a separate
/// `char` (Latin-1 interpretation), corrupting every multi-byte UTF-8
/// cookie value.
///
/// `+` is also translated to a space in line with the
/// `application/x-www-form-urlencoded` decoding that browsers use for
/// cookie-like contexts. Pre-`+` translation keeps `percent_decode_str`
/// happy (it never sees the literal `+`).
fn url_decode(s: &str) -> String {
    // Translate `+` → space first so percent_decode_str sees the same
    // byte stream the encoder produced when round-tripping form-style
    // values. (Cookie spec doesn't strictly require `+` ↔ space, but
    // keeping the prior behaviour avoids surprising clients that
    // produced `+`-encoded values.)
    let translated: String = s.chars().map(|c| if c == '+' { ' ' } else { c }).collect();
    percent_decode_str(&translated)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cookie_builder() {
        let cookie = Cookie::new("test", "value")
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Strict)
            .path("/app")
            .max_age(Duration::from_secs(3600));

        let header = cookie.to_header_value();
        assert!(header.contains("test=value"));
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("Secure"));
        assert!(header.contains("SameSite=Strict"));
        assert!(header.contains("Path=/app"));
        assert!(header.contains("Max-Age=3600"));
    }

    #[test]
    fn test_parse_cookies() {
        let cookies = parse_cookies("session=abc123; user_id=42; empty=");
        assert_eq!(cookies.get("session"), Some(&"abc123".to_string()));
        assert_eq!(cookies.get("user_id"), Some(&"42".to_string()));
        assert_eq!(cookies.get("empty"), Some(&"".to_string()));
    }

    #[test]
    fn test_forget_cookie() {
        let cookie = Cookie::forget("session");
        let header = cookie.to_header_value();
        assert!(header.contains("Max-Age=0"));
        assert!(header.contains("session="));
    }

    /// Domain 3a audit fix DR2: a CRLF in a cookie name MUST be
    /// percent-encoded so it cannot inject additional headers into the
    /// Set-Cookie response. Before the fix, the raw `\r\n` passed
    /// through `url_encode` and either landed in the response verbatim
    /// (header injection) or panicked the per-connection task when
    /// hyper rejected the value.
    #[test]
    fn cookie_name_with_crlf_is_percent_encoded() {
        let cookie = Cookie::new("evil\r\nX-Injected: yes", "v");
        let header = cookie.to_header_value();
        assert!(
            !header.contains('\r') && !header.contains('\n'),
            "encoded cookie header must contain no raw CR or LF; got: {header}"
        );
        assert!(
            header.contains("%0D%0A"),
            "CRLF must round-trip as %0D%0A in the encoded cookie name; got: {header}"
        );
    }

    /// Same fix applied to the value side. A user-controlled value
    /// containing CR/LF cannot inject headers.
    #[test]
    fn cookie_value_with_crlf_is_percent_encoded() {
        let cookie = Cookie::new("session", "abc\r\nX-Injected: yes");
        let header = cookie.to_header_value();
        assert!(
            !header.contains('\r') && !header.contains('\n'),
            "encoded cookie value must contain no raw CR/LF; got: {header}"
        );
        assert!(
            header.contains("%0D%0A"),
            "CRLF in the value must be percent-encoded; got: {header}"
        );
    }

    /// Domain 3a audit fix DR3: non-ASCII bytes in a cookie value get
    /// percent-encoded so the resulting Set-Cookie header is pure ASCII
    /// per RFC 6265.
    #[test]
    fn cookie_value_with_non_ascii_is_percent_encoded() {
        let cookie = Cookie::new("lang", "café");
        let header = cookie.to_header_value();
        assert!(
            header.is_ascii(),
            "encoded cookie header must be pure ASCII; got: {header}"
        );
        assert!(
            header.contains("caf%C3%A9"),
            "UTF-8 multi-byte sequence must percent-encode each byte; got: {header}"
        );
    }

    /// Domain 3a audit fix DR4: percent-encoded UTF-8 round-trips as
    /// the original UTF-8 string. Before the fix, `%C3%A9` decoded to
    /// two Latin-1 chars (`Ã©`) instead of `é`.
    #[test]
    fn cookie_utf8_round_trip_preserves_multi_byte_chars() {
        let original = "café — naïve façade";
        let encoded = url_encode(original);
        let decoded = url_decode(&encoded);
        assert_eq!(
            decoded, original,
            "UTF-8 round-trip must preserve multi-byte chars; \
             encoded: {encoded}, decoded: {decoded}"
        );
    }

    /// `parse_cookies` consumes a real Cookie header from the browser
    /// containing percent-encoded UTF-8. After the decode fix, the
    /// resulting HashMap holds the correct decoded values.
    #[test]
    fn parse_cookies_handles_percent_encoded_utf8() {
        let cookies = parse_cookies("display_name=caf%C3%A9; lang=fr");
        assert_eq!(cookies.get("display_name"), Some(&"café".to_string()));
        assert_eq!(cookies.get("lang"), Some(&"fr".to_string()));
    }
}
