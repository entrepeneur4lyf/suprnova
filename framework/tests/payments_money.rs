use suprnova::payments::{Money, Currency};
use rust_decimal::Decimal;
use std::str::FromStr;

#[test]
fn money_constructs_from_decimal_dollars() {
    let m = Money::from_decimal(Decimal::from_str("12.34").unwrap(), Currency::USD);
    assert_eq!(m.minor_units(), 1234);
    assert_eq!(m.currency(), Currency::USD);
}

#[test]
fn money_constructs_from_minor_units() {
    let m = Money::from_minor_units(1234, Currency::USD);
    assert_eq!(m.minor_units(), 1234);
    assert_eq!(m.as_decimal(), Decimal::from_str("12.34").unwrap());
}

#[test]
fn zero_decimal_currency_minor_units_equal_major_units() {
    let m = Money::from_decimal(Decimal::from(1234), Currency::JPY);
    assert_eq!(m.minor_units(), 1234);
    assert_eq!(m.as_decimal(), Decimal::from(1234));
}

#[test]
fn money_serde_roundtrip_preserves_minor_units() {
    let m = Money::from_minor_units(9999, Currency::EUR);
    let json = serde_json::to_string(&m).unwrap();
    let back: Money = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

#[test]
fn money_arithmetic_within_same_currency() {
    let a = Money::from_minor_units(100, Currency::USD);
    let b = Money::from_minor_units(250, Currency::USD);
    assert_eq!((a + b).minor_units(), 350);
    assert_eq!((b - a).minor_units(), 150);
}

#[test]
#[should_panic(expected = "currency mismatch")]
fn money_arithmetic_panics_on_currency_mismatch() {
    let a = Money::from_minor_units(100, Currency::USD);
    let b = Money::from_minor_units(100, Currency::EUR);
    let _ = a + b;
}
