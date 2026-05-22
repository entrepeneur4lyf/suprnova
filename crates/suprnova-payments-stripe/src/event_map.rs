//! Maps Stripe event type strings to `NeutralEventKind`.
//!
//! Only the subset of Stripe events that have a well-defined neutral mapping are
//! covered here. Unknown or provider-specific events return `None`; callers
//! should fall through to the raw `provider_event_type` in that case.

use suprnova::payments::NeutralEventKind;

/// Map a Stripe event type string (e.g. `"payment_intent.succeeded"`) to the
/// framework's provider-agnostic `NeutralEventKind`, or `None` if no mapping
/// exists.
pub fn stripe_event_to_neutral(event_type: &str) -> Option<NeutralEventKind> {
    match event_type {
        // PaymentIntent succeeded — all capture paths land here.
        "payment_intent.succeeded" => Some(NeutralEventKind::PaymentSucceeded),
        // PaymentIntent failed — covers both auth failures and insufficient funds.
        "payment_intent.payment_failed" => Some(NeutralEventKind::PaymentFailed),
        // Charge refunded fully or partially.
        "charge.refunded" => Some(NeutralEventKind::PaymentRefunded),
        // Dispute / chargeback opened.
        "charge.dispute.created" => Some(NeutralEventKind::PaymentDisputed),

        // Subscription lifecycle.
        "customer.subscription.created" => Some(NeutralEventKind::SubscriptionCreated),
        "customer.subscription.updated" => Some(NeutralEventKind::SubscriptionUpdated),
        "customer.subscription.deleted" => Some(NeutralEventKind::SubscriptionCanceled),
        "customer.subscription.paused" => Some(NeutralEventKind::SubscriptionUpdated),
        "customer.subscription.resumed" => Some(NeutralEventKind::SubscriptionUpdated),
        "customer.subscription.trial_will_end" => Some(NeutralEventKind::SubscriptionUpdated),

        // Invoice events — cover recurring billing.
        "invoice.payment_succeeded" | "invoice.paid" => Some(NeutralEventKind::InvoicePaid),
        "invoice.payment_failed" => Some(NeutralEventKind::InvoiceFailed),

        // Customer lifecycle.
        "customer.created" => Some(NeutralEventKind::CustomerCreated),
        "customer.updated" => Some(NeutralEventKind::CustomerUpdated),

        // Everything else — caller handles via raw payload.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_events_map_correctly() {
        assert_eq!(
            stripe_event_to_neutral("payment_intent.succeeded"),
            Some(NeutralEventKind::PaymentSucceeded)
        );
        assert_eq!(
            stripe_event_to_neutral("customer.subscription.deleted"),
            Some(NeutralEventKind::SubscriptionCanceled)
        );
        assert_eq!(
            stripe_event_to_neutral("invoice.paid"),
            Some(NeutralEventKind::InvoicePaid)
        );
        assert_eq!(
            stripe_event_to_neutral("invoice.payment_succeeded"),
            Some(NeutralEventKind::InvoicePaid)
        );
    }

    #[test]
    fn unknown_event_returns_none() {
        assert_eq!(stripe_event_to_neutral("radar.early_fraud_warning.created"), None);
        assert_eq!(stripe_event_to_neutral("payout.created"), None);
        assert_eq!(stripe_event_to_neutral(""), None);
    }
}
