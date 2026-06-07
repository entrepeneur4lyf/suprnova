//! Provider-neutral payments surface.
//!
//! See `manual/payments.md` for the user-facing guide.

pub mod dto;
pub mod entities;
pub mod error;
pub mod migrations;
pub mod mock;
pub mod money;
pub mod registry;
pub mod traits;
pub mod webhook_route;

pub use dto::*;
pub use error::{PaymentError, PaymentResult};
pub use mock::MockPaymentProvider;
pub use money::{Currency, Money};
pub use registry::{PaymentProviderEntry, PaymentProviderRegistry};
pub use traits::{
    Checkout, CustomerSnapshot, CustomerStore, PayloadIds, Payment, PaymentProvider,
    PaymentSnapshot, Subscription, WebhookHandler,
};
pub use webhook_route::webhook_routes;
