//! Provider-neutral payments surface.
//!
//! See `docs/core/payments.md` for the user-facing guide.

pub mod dto;
pub mod entities;
pub mod error;
pub mod migrations;
pub mod mock;
pub mod money;
pub mod registry;
pub mod traits;

pub use dto::*;
pub use error::{PaymentError, PaymentResult};
pub use mock::MockPaymentProvider;
pub use money::{Currency, Money};
pub use registry::{PaymentProviderEntry, PaymentProviderRegistry};
pub use traits::{Checkout, CustomerStore, Payment, PaymentProvider, Subscription, WebhookHandler};
