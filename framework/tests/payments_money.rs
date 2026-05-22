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

#[test]
#[should_panic(expected = "Money arithmetic overflow")]
fn money_add_overflow_panics() {
    let a = Money::from_minor_units(i64::MAX, Currency::USD);
    let b = Money::from_minor_units(1, Currency::USD);
    let _ = a + b;
}

#[test]
#[should_panic(expected = "Money arithmetic overflow")]
fn money_sub_underflow_panics() {
    let a = Money::from_minor_units(i64::MIN, Currency::USD);
    let b = Money::from_minor_units(1, Currency::USD);
    let _ = a - b;
}

#[test]
fn money_is_zero_accessor() {
    assert!(Money::from_minor_units(0, Currency::USD).is_zero());
    assert!(!Money::from_minor_units(1, Currency::USD).is_zero());
    assert!(!Money::from_minor_units(-1, Currency::USD).is_zero());
}

#[test]
fn money_currency_accessor_returns_construction_currency() {
    let m = Money::from_minor_units(500, Currency::GBP);
    assert_eq!(m.currency(), Currency::GBP);
}

#[test]
fn money_supports_negative_amounts_for_refunds() {
    let debit = Money::from_minor_units(-2500, Currency::USD);
    assert_eq!(debit.minor_units(), -2500);
    assert_eq!(debit.as_decimal(), Decimal::from_str("-25.00").unwrap());
    let zero = debit + Money::from_minor_units(2500, Currency::USD);
    assert!(zero.is_zero());
}

#[test]
fn money_add_assign_and_sub_assign() {
    let mut total = Money::from_minor_units(100, Currency::USD);
    total += Money::from_minor_units(50, Currency::USD);
    assert_eq!(total.minor_units(), 150);
    total -= Money::from_minor_units(25, Currency::USD);
    assert_eq!(total.minor_units(), 125);
}
