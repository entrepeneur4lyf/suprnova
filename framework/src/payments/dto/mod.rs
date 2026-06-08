//! Provider-neutral data-transfer types for the payments subsystem.
//!
//! These types are the wire format the framework hands to providers
//! (e.g. [`StartSessionRequest`], [`ChargeRequest`]) and the shapes
//! providers hand back (e.g. [`SessionPayload`], [`ChargeResult`]).
//! They are deliberately provider-agnostic — every concrete provider
//! adapter translates between its own SDK and these types so that
//! application code stays portable across rails.

pub mod country;
pub mod customer;
pub mod payment;
pub mod payment_method;
pub mod phone;
pub mod session;
pub mod subscription;
pub mod webhook;

pub use country::CountryCode;
pub use customer::{CreateCustomerRequest, CustomerRef, UpdateCustomerRequest};
pub use payment::{ChargeRequest, ChargeResult, PaymentStatus, RefundRequest, RefundResult};
pub use payment_method::{MobileMoneyOperator, PaymentMethod, StablecoinAsset};
pub use phone::PhoneNumber;
pub use session::{SessionMode, SessionPayload, StartSessionRequest};
pub use subscription::{
    SubscribeRequest, SubscriptionItemSnapshot, SubscriptionResult, SubscriptionStatus,
    UpdateSubscriptionRequest,
};
pub use webhook::{NeutralEventKind, WebhookContext, WebhookEvent};
