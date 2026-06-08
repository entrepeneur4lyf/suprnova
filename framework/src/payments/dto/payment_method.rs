//! Payment-method enum and the operator / asset taxonomies it carries.

use super::{CountryCode, PhoneNumber};
use serde::{Deserialize, Serialize};

/// Provider-neutral payment instrument — covers card, bank, eWallet,
/// Mobile Money, stablecoin, generic crypto, and a custom escape hatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaymentMethod {
    /// Credit / debit card with a tokenised display envelope.
    Card {
        /// Card network brand (`"visa"`, `"mastercard"`, etc.) as
        /// reported by the provider.
        brand: String,
        /// Last four digits of the PAN — safe to display.
        last4: String,
        /// Expiry month, 1–12.
        exp_month: u8,
        /// Expiry year, four-digit (e.g. `2028`).
        exp_year: u16,
    },
    /// Direct bank transfer / debit using a tokenised bank reference.
    BankTransfer {
        /// Display name of the issuing bank.
        bank_name: String,
        /// Last four digits of the account number — safe to display.
        last4: String,
    },
    /// Generic eWallet (PayPal, Alipay, WeChat Pay, etc.).
    EWallet {
        /// Wallet provider identifier (kebab-case — `"paypal"`,
        /// `"alipay"`, `"wechat_pay"`).
        provider: String,
        /// Wallet-side account identifier or token.
        identifier: String,
    },
    /// Mobile Money — payer identified by phone + operator + country.
    ///
    /// The user completes the payment via a USSD prompt or operator app
    /// notification. The frontend renders a `SessionPayload::MobileMoneyPrompt`
    /// telling the user to check their phone.
    MobileMoney {
        /// Mobile Money operator handling the prompt.
        operator: MobileMoneyOperator,
        /// Payer phone number in E.164 form.
        phone: PhoneNumber,
        /// ISO 3166-1 alpha-2 country code; restricts which operators
        /// are valid for this customer.
        country: CountryCode,
    },
    /// Stablecoin payment — pegged crypto, different UX from generic crypto
    /// (no volatility risk, often treated by providers as cash-equivalent).
    Stablecoin {
        /// Specific stablecoin asset (USDC, USDT, etc.).
        asset: StablecoinAsset,
        /// Optional network preference (e.g. Ethereum vs Solana for USDC).
        network: Option<String>,
    },
    /// Generic cryptocurrency — non-pegged.
    Crypto {
        /// Blockchain network identifier (e.g. `"ethereum"`, `"bitcoin"`).
        network: String,
        /// On-chain destination address for the payment.
        address: String,
    },
    /// Escape hatch for regional / provider-specific methods not yet modeled.
    Custom {
        /// Kind discriminator chosen by the integrator (kebab-case).
        kind: String,
        /// Free-form descriptor — exact shape is integration-defined.
        descriptor: String,
    },
}

/// Mobile Money operator. The `Custom` variant covers regional operators we
/// haven't enumerated yet without forcing a framework release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MobileMoneyOperator {
    /// MTN Mobile Money.
    MtnMomo,
    /// Safaricom M-Pesa.
    Mpesa,
    /// Airtel Money.
    AirtelMoney,
    /// Orange Money.
    OrangeMoney,
    /// Lipila (Zambia).
    Lipila,
    /// Operator-specific identifier (e.g. "tigopesa", "vodafone_cash") for
    /// providers we haven't enumerated. Lowercase, no whitespace.
    Custom {
        /// Operator slug — lowercase, no whitespace.
        identifier: String,
    },
}

/// Stablecoin asset. `Custom` covers stablecoins we haven't enumerated
/// (e.g. PYUSD, regional CBDCs) without forcing a framework release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StablecoinAsset {
    /// USD Coin.
    Usdc,
    /// Tether USD.
    Usdt,
    /// MakerDAO Dai.
    Dai,
    /// Other stablecoin by ticker symbol (uppercase).
    Custom {
        /// Ticker symbol — uppercase (e.g. `"PYUSD"`).
        ticker: String,
    },
}
