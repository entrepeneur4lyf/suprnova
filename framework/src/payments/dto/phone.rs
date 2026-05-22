use crate::payments::PaymentError;
use serde::{Deserialize, Serialize};

/// E.164-style phone number — `+` prefix followed by 8 to 15 ASCII digits.
///
/// Required for Mobile Money checkout flows (MTN MoMo, M-Pesa, Airtel Money,
/// Orange Money, Lipila) where the user is identified by phone, not email.
///
/// Constructed via `PhoneNumber::new("+260971234567")` or
/// `PhoneNumber::new("260971234567")` — leading `+` is added if absent.
///
/// # Wire format
///
/// Serializes as a single string ("+countrycode...digits") — frontend SDKs and
/// provider APIs both consume this shape directly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PhoneNumber(String);

impl PhoneNumber {
    /// Construct a phone number from a string with optional leading `+`.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentError::InvalidPhoneNumber`] when the digit count is
    /// outside the E.164 length range (8–15) or when non-digit characters
    /// appear after the optional `+` prefix.
    pub fn new(value: impl AsRef<str>) -> Result<Self, PaymentError> {
        let raw = value.as_ref().trim();
        let digits = raw.strip_prefix('+').unwrap_or(raw);
        if !(8..=15).contains(&digits.len())
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(PaymentError::InvalidPhoneNumber(raw.to_owned()));
        }
        Ok(Self(format!("+{digits}")))
    }

    /// Returns the normalized E.164 representation (`+` followed by digits).
    #[inline]
    #[must_use]
    pub fn as_e164(&self) -> &str {
        &self.0
    }

    /// Returns the digits-only representation (no leading `+`).
    #[inline]
    #[must_use]
    pub fn digits(&self) -> &str {
        &self.0[1..]
    }
}

impl AsRef<str> for PhoneNumber {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_e164()
    }
}

impl std::fmt::Display for PhoneNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_e164())
    }
}
