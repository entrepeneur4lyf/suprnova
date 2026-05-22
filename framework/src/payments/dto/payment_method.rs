use super::{CountryCode, PhoneNumber};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaymentMethod {
    Card {
        brand: String,
        last4: String,
        exp_month: u8,
        exp_year: u16,
    },
    BankTransfer {
        bank_name: String,
        last4: String,
    },
    EWallet {
        provider: String,
        identifier: String,
    },
    /// Mobile Money — payer identified by phone + operator + country.
    ///
    /// The user completes the payment via a USSD prompt or operator app
    /// notification. The frontend renders a `SessionPayload::MobileMoneyPrompt`
    /// telling the user to check their phone.
    MobileMoney {
        operator: MobileMoneyOperator,
        phone: PhoneNumber,
        country: CountryCode,
    },
    /// Stablecoin payment — pegged crypto, different UX from generic crypto
    /// (no volatility risk, often treated by providers as cash-equivalent).
    Stablecoin {
        asset: StablecoinAsset,
        /// Optional network preference (e.g. Ethereum vs Solana for USDC).
        network: Option<String>,
    },
    /// Generic cryptocurrency — non-pegged.
    Crypto {
        network: String,
        address: String,
    },
    /// Escape hatch for regional / provider-specific methods not yet modeled.
    Custom {
        kind: String,
        descriptor: String,
    },
}

/// Mobile Money operator. The `Custom` variant covers regional operators we
/// haven't enumerated yet without forcing a framework release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MobileMoneyOperator {
    MtnMomo,
    Mpesa,
    AirtelMoney,
    OrangeMoney,
    Lipila,
    /// Operator-specific identifier (e.g. "tigopesa", "vodafone_cash") for
    /// providers we haven't enumerated. Lowercase, no whitespace.
    Custom {
        identifier: String,
    },
}

/// Stablecoin asset. `Custom` covers stablecoins we haven't enumerated
/// (e.g. PYUSD, regional CBDCs) without forcing a framework release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StablecoinAsset {
    Usdc,
    Usdt,
    Dai,
    /// Other stablecoin by ticker symbol (uppercase).
    Custom {
        ticker: String,
    },
}
