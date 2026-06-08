//! Framework-owned SeaORM mirror entities for the payments subsystem.
//!
//! One module per `payments_*` table. The provider is the source of truth
//! for each record — these mirrors exist so the app can join billing
//! state against its own data without round-tripping to the provider on
//! every read. Webhook ingress keeps the mirrors fresh; reconciliation
//! jobs paper over dropped webhooks via the corresponding `get_*` /
//! `status` calls on the provider traits.

pub mod customer;
pub mod payment_method;
pub mod subscription;
pub mod subscription_item;
pub mod transaction;
pub mod webhook_event;
