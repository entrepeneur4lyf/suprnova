//! Cookie handling for suprnova framework
//!
//! Provides Laravel-like cookie API with secure defaults.

use std::collections::HashMap;
use std::time::Duration;

/// SameSite cookie attribute
#[derive(Clone, Debug, PartialEq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

impl Default for SameSite {
    fn default() -> Self {
        Self::Lax
    }
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
            Some((
                url_decode(name),
                url_decode(value),
            ))
        })
        .collect()
}

/// Simple URL encoding for cookie values
fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => result.push_str("%20"),
            '!' => result.push_str("%21"),
            '"' => result.push_str("%22"),
            '#' => result.push_str("%23"),
            '$' => result.push_str("%24"),
            '%' => result.push_str("%25"),
            '&' => result.push_str("%26"),
            '\'' => result.push_str("%27"),
            '(' => result.push_str("%28"),
            ')' => result.push_str("%29"),
            '*' => result.push_str("%2A"),
            '+' => result.push_str("%2B"),
            ',' => result.push_str("%2C"),
            '/' => result.push_str("%2F"),
            ':' => result.push_str("%3A"),
            ';' => result.push_str("%3B"),
            '=' => result.push_str("%3D"),
            '?' => result.push_str("%3F"),
            '@' => result.push_str("%40"),
            '[' => result.push_str("%5B"),
            '\\' => result.push_str("%5C"),
            ']' => result.push_str("%5D"),
            _ => result.push(c),
        }
    }
    result
}

/// Simple URL decoding for cookie values
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            result.push('%');
            result.push_str(&hex);
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
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
}
