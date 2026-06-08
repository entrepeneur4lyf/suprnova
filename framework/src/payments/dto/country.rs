//! [`CountryCode`] — ISO 3166-1 alpha-2 country code, validated on construction.

use crate::payments::PaymentError;
use serde::{Deserialize, Serialize};

/// ISO 3166-1 alpha-2 country code — two uppercase ASCII letters.
///
/// Used for routing decisions (which Mobile Money operator is valid in which
/// country, EU-vs-US tax handling, accepted-by-provider checks).
///
/// # Wire format
///
/// Serializes as a single string — "ZM", "US", "KE", etc.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CountryCode(String);

impl CountryCode {
    /// Parse and validate an ISO 3166-1 alpha-2 country code.
    ///
    /// Accepts mixed case; normalizes to uppercase.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentError::InvalidCountryCode`] when the input is not
    /// exactly two ASCII alphabetic characters.
    pub fn new(code: impl AsRef<str>) -> Result<Self, PaymentError> {
        let raw = code.as_ref().trim();
        if raw.len() != 2 || !raw.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Err(PaymentError::InvalidCountryCode(raw.to_owned()));
        }
        Ok(Self(raw.to_ascii_uppercase()))
    }

    /// Returns the normalized two-letter code.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for CountryCode {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for CountryCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
