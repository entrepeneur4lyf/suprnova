//! Webhook ingress route for the payments subsystem.
//!
//! Registers `POST /webhooks/payments/{provider}` on a Suprnova `Router`.
//! The handler:
//! 1. Resolves the named provider from `PaymentProviderRegistry`.
//! 2. Verifies the inbound signature via `WebhookHandler::verify`.
//! 3. Parses the body into a neutral `WebhookEvent`.
//! 4. Short-circuits with 200 if `(provider, provider_event_id)` already
//!    exists in `payments_webhook_events` (idempotency for retrying providers).
//! 5. Inserts the audit row.
//! 6. Dispatches to the mirror-table hydration paths driven by `event.neutral`
//!    and the IDs returned by `WebhookHandler::extract_payload_ids` /
//!    `WebhookHandler::extract_payment_snapshot`. Hydration failures mark the
//!    audit row as failed but never return non-2xx to the provider, so the
//!    provider does not endlessly retry an event we've already accepted.
//! 7. Marks the audit row processed.

use crate::http::{text, HttpResponse, Response};
use crate::payments::entities::{customer, subscription, subscription_item, transaction, webhook_event};
use crate::payments::{
    NeutralEventKind, PaymentError, PaymentProviderRegistry, PaymentSnapshot, SubscriptionResult,
    SubscriptionStatus, WebhookContext, WebhookEvent,
};
use crate::routing::Router;
use crate::Request;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;

fn err_response(status: u16, body: &str) -> Response {
    Ok(HttpResponse::text(body).status(status))
}

async fn handle_webhook_inner(
    db: &DatabaseConnection,
    provider_name: &str,
    remote_addr_str: Option<String>,
    headers: http::HeaderMap,
    body: bytes::Bytes,
) -> Response {
    // 1. Resolve provider
    let provider = match PaymentProviderRegistry::get(provider_name) {
        Some(p) => p,
        None => {
            tracing::warn!(provider = %provider_name, "webhook received for unregistered provider");
            return err_response(404, "unknown provider");
        }
    };

    // 2. Parse remote_addr
    let remote_addr = remote_addr_str
        .as_deref()
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok());

    // 3. Verify signature (sync)
    let ctx = WebhookContext {
        body: &body,
        headers: &headers,
        remote_addr,
    };
    if let Err(e) = provider.verify(&ctx) {
        tracing::warn!(provider = %provider_name, error = %e, "webhook signature verification failed");
        return err_response(401, "signature");
    }

    // 4. Parse event (sync)
    let event: WebhookEvent = match provider.parse_event(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(provider = %provider_name, error = %e, "webhook parse failed");
            return err_response(400, "malformed");
        }
    };

    // 5. Idempotency check
    let existing = webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await;
    match existing {
        Ok(Some(_)) => {
            tracing::debug!(
                provider = %provider_name,
                event_id = %event.provider_event_id,
                "duplicate webhook — already processed"
            );
            return err_response(200, "duplicate");
        }
        Ok(None) => { /* proceed */ }
        Err(e) => {
            tracing::error!(error = %e, "db error checking webhook idempotency");
            return err_response(500, "db");
        }
    }

    // 6. Serialize neutral_event_kind
    let neutral_str = event.neutral.and_then(|k| {
        serde_json::to_value(k)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
    });

    // 7. Insert audit row
    let record = webhook_event::ActiveModel {
        provider: Set(event.provider.clone()),
        provider_event_id: Set(event.provider_event_id.clone()),
        provider_event_type: Set(event.provider_event_type.clone()),
        neutral_event_kind: Set(neutral_str),
        payload: Set(event.raw_payload.clone()),
        received_at: Set(Utc::now().to_rfc3339()),
        processed_at: Set(None),
        process_error: Set(None),
        ..Default::default()
    };

    if let Err(e) = record.insert(db).await {
        let msg = format!("{e}");
        let is_dup = msg.contains("UNIQUE") || msg.contains("duplicate");
        if is_dup {
            return err_response(200, "duplicate-race");
        }
        tracing::error!(error = %e, "failed to persist webhook event");
        return err_response(500, "persist");
    }

    // 8. Hydrate mirror tables. Hydration errors are logged + recorded but
    //    we still return 2xx so the provider doesn't retry forever — the
    //    audit row + process_error makes the failure visible for operators.
    if let Err(e) = process_webhook(db, provider.as_ref(), &event).await {
        tracing::error!(
            provider = %provider_name,
            event_id = %event.provider_event_id,
            error = %e,
            "webhook hydration failed"
        );
        let _ = mark_failed(db, &event, &format!("{e}")).await;
        return text("ok-with-errors");
    }

    let _ = mark_processed(db, &event).await;
    text("ok")
}

/// Dispatch a parsed [`WebhookEvent`] to the mirror-table hydration paths.
///
/// Unmapped events (no `neutral`) are no-ops — the audit row is the only
/// effect. Mapped events extract IDs / snapshots from the provider, look up
/// or fetch the canonical entity state, then upsert the relevant mirror rows.
async fn process_webhook(
    db: &DatabaseConnection,
    provider: &dyn crate::payments::PaymentProvider,
    event: &WebhookEvent,
) -> Result<(), PaymentError> {
    let Some(neutral) = event.neutral else {
        return Ok(());
    };

    let ids = provider.extract_payload_ids(event);

    match neutral {
        NeutralEventKind::SubscriptionCreated
        | NeutralEventKind::SubscriptionUpdated
        | NeutralEventKind::SubscriptionCanceled => {
            let Some(sub_id) = ids.subscription_id.as_deref() else {
                tracing::warn!(
                    event_id = %event.provider_event_id,
                    provider_event_type = %event.provider_event_type,
                    "subscription webhook missing subscription_id"
                );
                return Ok(());
            };
            let result = provider.get(sub_id).await?;
            upsert_subscription(db, &event.provider, &result, neutral).await?;
            sync_subscription_items(db, &event.provider, sub_id, &result).await?;
        }
        NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated => {
            let Some(cust_id) = ids.customer_id.as_deref() else {
                tracing::warn!(
                    event_id = %event.provider_event_id,
                    provider_event_type = %event.provider_event_type,
                    "customer webhook missing customer_id"
                );
                return Ok(());
            };
            update_customer_mirror(db, &event.provider, cust_id, event).await?;
        }
        NeutralEventKind::PaymentSucceeded
        | NeutralEventKind::PaymentFailed
        | NeutralEventKind::PaymentRefunded
        | NeutralEventKind::PaymentDisputed
        | NeutralEventKind::InvoicePaid
        | NeutralEventKind::InvoiceFailed => {
            let Some(snapshot) = provider.extract_payment_snapshot(event) else {
                tracing::debug!(
                    event_id = %event.provider_event_id,
                    provider_event_type = %event.provider_event_type,
                    "payment webhook produced no snapshot; audit row only"
                );
                return Ok(());
            };
            upsert_transaction(db, &event.provider, &snapshot).await?;
        }
    }
    Ok(())
}

fn subscription_status_to_str(s: SubscriptionStatus) -> &'static str {
    match s {
        SubscriptionStatus::Trialing => "trialing",
        SubscriptionStatus::Active => "active",
        SubscriptionStatus::PastDue => "past_due",
        SubscriptionStatus::Canceled => "canceled",
        SubscriptionStatus::Incomplete => "incomplete",
        SubscriptionStatus::Paused => "paused",
    }
}

async fn upsert_subscription(
    db: &DatabaseConnection,
    provider: &str,
    result: &SubscriptionResult,
    neutral: NeutralEventKind,
) -> Result<(), PaymentError> {
    let existing = subscription::Entity::find()
        .filter(subscription::Column::Provider.eq(provider))
        .filter(subscription::Column::ProviderSubscriptionId.eq(&result.provider_subscription_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let status_str = subscription_status_to_str(result.status);
    let mark_canceled = matches!(neutral, NeutralEventKind::SubscriptionCanceled)
        || matches!(result.status, SubscriptionStatus::Canceled);

    let now = Utc::now().to_rfc3339();

    match existing {
        Some(model) => {
            let was_canceled = model.canceled_at.is_some();
            let mut am: subscription::ActiveModel = model.into();
            am.provider_customer_id = Set(result.provider_customer_id.clone());
            am.status = Set(status_str.to_string());
            am.current_period_start = Set(result.current_period_start.to_rfc3339());
            am.current_period_end = Set(result.current_period_end.to_rfc3339());
            am.cancel_at_period_end = Set(result.cancel_at_period_end);
            if mark_canceled && !was_canceled {
                am.canceled_at = Set(Some(now.clone()));
            }
            am.provider_metadata = Set(result.provider_metadata.clone());
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
        None => {
            let canceled_at = if mark_canceled { Some(now.clone()) } else { None };
            let am = subscription::ActiveModel {
                provider: Set(provider.to_string()),
                provider_subscription_id: Set(result.provider_subscription_id.clone()),
                provider_customer_id: Set(result.provider_customer_id.clone()),
                status: Set(status_str.to_string()),
                current_period_start: Set(result.current_period_start.to_rfc3339()),
                current_period_end: Set(result.current_period_end.to_rfc3339()),
                cancel_at_period_end: Set(result.cancel_at_period_end),
                canceled_at: Set(canceled_at),
                provider_metadata: Set(result.provider_metadata.clone()),
                created_at: Set(now.clone()),
                updated_at: Set(now),
                ..Default::default()
            };
            am.insert(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
    }
    Ok(())
}

async fn sync_subscription_items(
    db: &DatabaseConnection,
    provider: &str,
    provider_subscription_id: &str,
    result: &SubscriptionResult,
) -> Result<(), PaymentError> {
    let parent = subscription::Entity::find()
        .filter(subscription::Column::Provider.eq(provider))
        .filter(subscription::Column::ProviderSubscriptionId.eq(provider_subscription_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?
        .ok_or_else(|| {
            PaymentError::Internal("parent subscription vanished between upsert and item sync".into())
        })?;
    let parent_id = parent.id;

    let existing_items = subscription_item::Entity::find()
        .filter(subscription_item::Column::SubscriptionId.eq(parent_id))
        .all(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let now = Utc::now().to_rfc3339();

    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &result.items {
        keep.insert(item.provider_item_id.clone());
        let existing = existing_items
            .iter()
            .find(|row| row.provider_item_id == item.provider_item_id);
        let (unit_amount, unit_currency) = match item.unit_amount.as_ref() {
            Some(m) => (Some(m.minor_units()), Some(m.currency().code().to_string())),
            None => (None, None),
        };
        match existing {
            Some(model) => {
                let mut am: subscription_item::ActiveModel = model.clone().into();
                am.provider_price_id = Set(item.provider_price_id.clone());
                am.quantity = Set(item.quantity as i32);
                am.unit_amount_minor = Set(unit_amount);
                am.unit_currency = Set(unit_currency);
                am.updated_at = Set(now.clone());
                am.update(db)
                    .await
                    .map_err(|e| PaymentError::Internal(format!("{e}")))?;
            }
            None => {
                let am = subscription_item::ActiveModel {
                    subscription_id: Set(parent_id),
                    provider_item_id: Set(item.provider_item_id.clone()),
                    provider_price_id: Set(item.provider_price_id.clone()),
                    quantity: Set(item.quantity as i32),
                    unit_amount_minor: Set(unit_amount),
                    unit_currency: Set(unit_currency),
                    provider_metadata: Set(serde_json::Value::Null),
                    created_at: Set(now.clone()),
                    updated_at: Set(now.clone()),
                    ..Default::default()
                };
                am.insert(db)
                    .await
                    .map_err(|e| PaymentError::Internal(format!("{e}")))?;
            }
        }
    }

    // Remove items that no longer exist on the provider side. The
    // subscription is the source of truth — once an item is removed by the
    // upstream subscription update, the mirror should reflect that.
    for stale in existing_items
        .iter()
        .filter(|row| !keep.contains(&row.provider_item_id))
    {
        let am: subscription_item::ActiveModel = stale.clone().into();
        am.delete(db)
            .await
            .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    }

    Ok(())
}

async fn upsert_transaction(
    db: &DatabaseConnection,
    provider: &str,
    snapshot: &PaymentSnapshot,
) -> Result<(), PaymentError> {
    let existing = transaction::Entity::find()
        .filter(transaction::Column::Provider.eq(provider))
        .filter(transaction::Column::ProviderTransactionId.eq(&snapshot.provider_transaction_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let now = Utc::now().to_rfc3339();

    match existing {
        Some(model) => {
            let mut am: transaction::ActiveModel = model.into();
            am.provider_customer_id = Set(snapshot.provider_customer_id.clone());
            am.provider_subscription_id = Set(snapshot.provider_subscription_id.clone());
            am.amount_total_minor = Set(snapshot.amount_total_minor);
            am.amount_tax_minor = Set(snapshot.amount_tax_minor);
            am.currency = Set(snapshot.currency.clone());
            am.status = Set(snapshot.status.clone());
            if snapshot.paid_at.is_some() {
                am.paid_at = Set(snapshot.paid_at.map(|t| t.to_rfc3339()));
            }
            am.provider_metadata = Set(snapshot.provider_metadata.clone());
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
        None => {
            let am = transaction::ActiveModel {
                provider: Set(provider.to_string()),
                provider_transaction_id: Set(snapshot.provider_transaction_id.clone()),
                provider_customer_id: Set(snapshot.provider_customer_id.clone()),
                provider_subscription_id: Set(snapshot.provider_subscription_id.clone()),
                amount_total_minor: Set(snapshot.amount_total_minor),
                amount_tax_minor: Set(snapshot.amount_tax_minor),
                currency: Set(snapshot.currency.clone()),
                status: Set(snapshot.status.clone()),
                paid_at: Set(snapshot.paid_at.map(|t| t.to_rfc3339())),
                provider_metadata: Set(snapshot.provider_metadata.clone()),
                created_at: Set(now.clone()),
                updated_at: Set(now),
                ..Default::default()
            };
            am.insert(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
    }
    Ok(())
}

/// Update an existing `payments_customers` mirror row.
///
/// We deliberately do NOT insert when no row exists: `user_id` is `NOT NULL`
/// and only the app knows which user a provider-side customer maps to
/// (creation goes through [`CustomerStore::create_customer`] +
/// app-controlled DB write). Out-of-band customers (created in the Stripe
/// dashboard, say) are logged but not synthesized.
async fn update_customer_mirror(
    db: &DatabaseConnection,
    provider: &str,
    provider_customer_id: &str,
    event: &WebhookEvent,
) -> Result<(), PaymentError> {
    let existing = customer::Entity::find()
        .filter(customer::Column::Provider.eq(provider))
        .filter(customer::Column::ProviderCustomerId.eq(provider_customer_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let Some(model) = existing else {
        tracing::info!(
            provider = %provider,
            provider_customer_id = %provider_customer_id,
            "customer webhook for unknown customer — no mirror row to update"
        );
        return Ok(());
    };

    // Extract the email from the standard Stripe/Paddle shape — both put
    // the customer object at `data.object` (Stripe) or `data` (Paddle).
    // Best-effort: if we can't find it, we still update provider_metadata.
    let new_email = event
        .raw_payload
        .pointer("/data/object/email")
        .or_else(|| event.raw_payload.pointer("/data/email"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let new_metadata = event
        .raw_payload
        .pointer("/data/object")
        .or_else(|| event.raw_payload.pointer("/data"))
        .cloned()
        .unwrap_or_else(|| event.raw_payload.clone());

    let mut am: customer::ActiveModel = model.into();
    if let Some(email) = new_email {
        am.email = Set(email);
    }
    am.provider_metadata = Set(new_metadata);
    am.updated_at = Set(Utc::now().to_rfc3339());
    am.update(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    Ok(())
}

async fn mark_processed(
    db: &DatabaseConnection,
    event: &WebhookEvent,
) -> Result<(), PaymentError> {
    let model = webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?
        .ok_or_else(|| PaymentError::Internal("webhook event vanished after insert".into()))?;
    let mut am: webhook_event::ActiveModel = model.into();
    am.processed_at = Set(Some(Utc::now().to_rfc3339()));
    am.update(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    Ok(())
}

async fn mark_failed(
    db: &DatabaseConnection,
    event: &WebhookEvent,
    err_str: &str,
) -> Result<(), PaymentError> {
    let model = webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?
        .ok_or_else(|| PaymentError::Internal("webhook event vanished after insert".into()))?;
    let mut am: webhook_event::ActiveModel = model.into();
    am.process_error = Set(Some(err_str.into()));
    am.update(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    Ok(())
}

/// Mount the webhook ingress route onto an Axum-compatible Router.
///
/// ```ignore
/// use std::sync::Arc;
/// use suprnova::payments::webhook_routes;
///
/// let router = webhook_routes(db.clone());
/// // Merge into your app router.
/// ```
pub fn webhook_routes(db: Arc<DatabaseConnection>) -> Router {
    // The handler is a `Fn(Request) -> Future` closure that clones Arc per call.
    let db_for_handler = db;
    Router::new().post(
        "/webhooks/payments/{provider}",
        move |req: Request| {
            let db = db_for_handler.clone();
            async move {
                // Extract header map and remote addr before consuming the request body.
                let headers = req.inner().headers().clone();
                let remote_addr_str = headers
                    .get("x-forwarded-for")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let provider_name = match req.param("provider") {
                    Ok(s) => s.to_string(),
                    Err(_) => return err_response(404, "unknown provider"),
                };
                // Consume the request to read the body.
                let (_, body) = match req.body_bytes().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to read webhook body");
                        return err_response(400, "body");
                    }
                };
                handle_webhook_inner(&db, &provider_name, remote_addr_str, headers, body).await
            }
        },
    ).into()
}
