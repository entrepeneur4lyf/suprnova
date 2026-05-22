use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::ops::{Add, Sub};

pub use iso_currency::Currency;

/// A monetary amount stored as i64 minor units (cents/pence/yen/etc.) plus a `Currency`.
///
/// Zero-decimal currencies (JPY, KRW, VND, ...) have `exponent() == Some(0)` so
/// 1 minor unit equals 1 major unit.
///
/// # Invariants
///
/// - Minor units are stored as `i64` — negative values represent debits/refunds.
/// - `Add` and `Sub` panic on currency mismatch; silent cross-currency arithmetic
///   would silently corrupt amounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    minor_units: i64,
    currency: Currency,
}

impl Money {
    /// Construct from raw minor units (cents, pence, yen, etc.) and a currency.
    pub const fn from_minor_units(minor_units: i64, currency: Currency) -> Self {
        Self { minor_units, currency }
    }

    /// Construct from a major-unit decimal amount (e.g. `12.34` USD → 1234 cents).
    ///
    /// For zero-decimal currencies (JPY, KRW, VND) the decimal is treated as
    /// whole units and stored unchanged.
    pub fn from_decimal(major: Decimal, currency: Currency) -> Self {
        let exp: u32 = currency.exponent().unwrap_or(2).into();
        let multiplier = Decimal::from(10u64.pow(exp));
        let minor = (major * multiplier)
            .round()
            .to_i64()
            .expect("Money amount overflows i64 minor units");
        Self { minor_units: minor, currency }
    }

    /// The raw minor-unit value (cents, pence, yen, etc.).
    pub const fn minor_units(&self) -> i64 {
        self.minor_units
    }

    /// The currency.
    pub const fn currency(&self) -> Currency {
        self.currency
    }

    /// Convert back to a major-unit decimal (e.g. 1234 USD cents → `12.34`).
    pub fn as_decimal(&self) -> Decimal {
        let exp: u32 = self.currency.exponent().unwrap_or(2).into();
        let divisor = Decimal::from(10u64.pow(exp));
        Decimal::from(self.minor_units) / divisor
    }

    /// Returns `true` if the amount is exactly zero.
    pub fn is_zero(&self) -> bool {
        self.minor_units == 0
    }
}

impl Add for Money {
    type Output = Money;

    fn add(self, rhs: Money) -> Money {
        assert_eq!(
            self.currency, rhs.currency,
            "currency mismatch: {:?} vs {:?}",
            self.currency, rhs.currency
        );
        Money {
            minor_units: self.minor_units + rhs.minor_units,
            currency: self.currency,
        }
    }
}

impl Sub for Money {
    type Output = Money;

    fn sub(self, rhs: Money) -> Money {
        assert_eq!(
            self.currency, rhs.currency,
            "currency mismatch: {:?} vs {:?}",
            self.currency, rhs.currency
        );
        Money {
            minor_units: self.minor_units - rhs.minor_units,
            currency: self.currency,
        }
    }
}
