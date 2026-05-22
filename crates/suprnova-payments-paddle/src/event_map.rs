//! Maps Paddle event_type strings to `NeutralEventKind`.

use suprnova::payments::NeutralEventKind;

/// Map a Paddle event_type string (e.g. `"transaction.completed"`) to the
/// framework's provider-agnostic `NeutralEventKind`, or `None` if no mapping
/// exists. Callers should fall through to `provider_event_type` + raw payload
/// for unmapped events.
pub fn paddle_event_to_neutral(t: &str) -> Option<NeutralEventKind> {
    Some(match t {
        "transaction.completed" | "transaction.paid" => NeutralEventKind::PaymentSucceeded,
        "transaction.payment_failed" => NeutralEventKind::PaymentFailed,
        "adjustment.created" | "adjustment.updated" => NeutralEventKind::PaymentRefunded,
        "subscription.created" => NeutralEventKind::SubscriptionCreated,
        "subscription.updated"
        | "subscription.activated"
        | "subscription.paused"
        | "subscription.resumed"
        | "subscription.trialing" => NeutralEventKind::SubscriptionUpdated,
        "subscription.canceled" => NeutralEventKind::SubscriptionCanceled,
        "transaction.billed" => NeutralEventKind::InvoicePaid,
        "customer.created" => NeutralEventKind::CustomerCreated,
        "customer.updated" => NeutralEventKind::CustomerUpdated,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_events_map_correctly() {
        assert_eq!(
            paddle_event_to_neutral("transaction.completed"),
            Some(NeutralEventKind::PaymentSucceeded)
        );
        assert_eq!(
            paddle_event_to_neutral("subscription.created"),
            Some(NeutralEventKind::SubscriptionCreated)
        );
        assert_eq!(
            paddle_event_to_neutral("subscription.canceled"),
            Some(NeutralEventKind::SubscriptionCanceled)
        );
        assert_eq!(
            paddle_event_to_neutral("adjustment.created"),
            Some(NeutralEventKind::PaymentRefunded)
        );
    }

    #[test]
    fn unknown_event_returns_none() {
        assert_eq!(paddle_event_to_neutral("address.created"), None);
        assert_eq!(paddle_event_to_neutral(""), None);
    }
}
