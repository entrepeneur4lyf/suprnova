//! Webhook ingress route for the payments subsystem.
//!
//! Registers `POST /webhooks/payments/{provider}` on a Suprnova `Router`.
//!
//! Production-grade contract:
//!
//! 1. **Idempotency, retry-aware.** A duplicate of an event that was
//!    *successfully* processed (`processed_at IS NOT NULL`) returns 200
//!    `duplicate` immediately. A duplicate of an event that previously
//!    *failed* (`processed_at IS NULL`, `process_error` set) re-attempts
//!    hydration — the provider's retry is the recovery mechanism.
//! 2. **Atomic hydration.** All mirror-table writes for one event happen
//!    inside a single DB transaction along with `mark_processed`. Partial
//!    state is not observable: either the mirror catches up AND
//!    `processed_at` is set, or both are rolled back together.
//! 3. **5xx on failure.** Hydration errors return 503 so the provider
//!    retries with backoff. The audit row's `process_error` is updated
//!    outside the rolled-back transaction so operators can see the
//!    failure across retries.
//! 4. **Auditability.** Every accepted event lands in
//!    `payments_webhook_events` before hydration begins — even failures
//!    leave an audit trail.

use crate::http::{text, HttpResponse, Response};
use crate::payments::entities::{customer, subscription, subscription_item, transaction, webhook_event};
use crate::payments::{
    CustomerSnapshot, NeutralEventKind, PaymentError, PaymentProvider, PaymentProviderRegistry,
    PaymentSnapshot, SubscriptionResult, SubscriptionStatus, WebhookContext, WebhookEvent,
};
use crate::routing::Router;
use crate::Request;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set, TransactionTrait,
};
use std::sync::Arc;

fn err_response(status: u16, body: &str) -> Response {
    Ok(HttpResponse::text(body).status(status))
}

/// A `UNIQUE` violation surfaces differently per backend (SQLite says
/// `UNIQUE constraint failed`, Postgres says `duplicate key value violates
/// unique constraint`, MySQL says `Duplicate entry`). This catches all
/// three by substring — the alternative is per-driver SQLSTATE matching,
/// which SeaORM abstracts away.
fn is_unique_violation(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("unique") || m.contains("duplicate")
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

    // 2. Parse remote_addr (best-effort; never blocks the request)
    let remote_addr = remote_addr_str
        .as_deref()
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok());

    // 3. Verify signature
    let ctx = WebhookContext {
        body: &body,
        headers: &headers,
        remote_addr,
    };
    if let Err(e) = provider.verify(&ctx) {
        tracing::warn!(provider = %provider_name, error = %e, "webhook signature verification failed");
        return err_response(401, "signature");
    }

    // 4. Parse event
    let event: WebhookEvent = match provider.parse_event(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(provider = %provider_name, error = %e, "webhook parse failed");
            return err_response(400, "malformed");
        }
    };

    // 5. Idempotency, retry-aware. Successfully-processed events short-circuit
    //    here; previously-failed events fall through to retry hydration.
    let existing = match webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "db error checking webhook idempotency");
            return err_response(500, "db");
        }
    };
    if let Some(row) = &existing
        && row.processed_at.is_some()
    {
        tracing::debug!(
            provider = %provider_name,
            event_id = %event.provider_event_id,
            "duplicate webhook — already processed"
        );
        return err_response(200, "duplicate");
    }

    // 6. Ensure audit row exists. INSERT race on (provider, event_id) UNIQUE
    //    is treated as a concurrent duplicate — return 200 so the racing
    //    caller can retry against the now-existing row if it later fails.
    if existing.is_none() {
        let neutral_str = event.neutral.and_then(|k| {
            serde_json::to_value(k)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        });
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
            if is_unique_violation(&msg) {
                return err_response(200, "duplicate-race");
            }
            tracing::error!(error = %e, "failed to persist webhook event");
            return err_response(500, "persist");
        }
    } else {
        // Retry: clear stale process_error from a previous failed attempt so
        // the audit row reflects the current attempt's outcome.
        if let Some(row) = existing {
            let mut am: webhook_event::ActiveModel = row.into();
            am.process_error = Set(None);
            if let Err(e) = am.update(db).await {
                tracing::error!(error = %e, "failed to clear stale process_error before retry");
                // Continue — the retry can still succeed and overwrite this.
            }
        }
    }

    // 7. Hydrate inside a transaction. On failure, audit row + process_error
    //    survive the rollback; the response is 503 so the provider retries.
    match try_hydrate(db, provider.as_ref(), &event).await {
        Ok(()) => text("ok"),
        Err(e) => {
            tracing::error!(
                provider = %provider_name,
                event_id = %event.provider_event_id,
                error = %e,
                "webhook hydration failed"
            );
            let _ = mark_failed(db, &event, &format!("{e}")).await;
            err_response(503, "hydration-failed")
        }
    }
}

/// Run `process_webhook` and `mark_processed` inside one DB transaction.
/// On error, rolls back so partial mirror state isn't observable.
async fn try_hydrate(
    db: &DatabaseConnection,
    provider: &dyn PaymentProvider,
    event: &WebhookEvent,
) -> Result<(), PaymentError> {
    let txn = db
        .begin()
        .await
        .map_err(|e| PaymentError::Internal(format!("begin tx: {e}")))?;

    match process_webhook(&txn, provider, event).await {
        Ok(()) => match mark_processed(&txn, event).await {
            Ok(()) => txn
                .commit()
                .await
                .map_err(|e| PaymentError::Internal(format!("commit: {e}"))),
            Err(e) => {
                let _ = txn.rollback().await;
                Err(e)
            }
        },
        Err(e) => {
            let _ = txn.rollback().await;
            Err(e)
        }
    }
}

/// Dispatch a parsed [`WebhookEvent`] to the mirror-table hydration paths.
///
/// Unmapped events (no `neutral`) are no-ops — the audit row is the only
/// effect. Mapped events extract IDs / snapshots from the provider, look up
/// or fetch the canonical entity state, then upsert the relevant mirror rows.
/// Missing IDs in events whose neutral kind requires one are treated as
/// validation errors — silent success would leave the mirror stale without
/// operator visibility.
async fn process_webhook<C>(
    db: &C,
    provider: &dyn PaymentProvider,
    event: &WebhookEvent,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let Some(neutral) = event.neutral else {
        return Ok(());
    };

    let ids = provider.extract_payload_ids(event);

    match neutral {
        NeutralEventKind::SubscriptionCreated
        | NeutralEventKind::SubscriptionUpdated
        | NeutralEventKind::SubscriptionCanceled => {
            let sub_id = ids.subscription_id.as_deref().ok_or_else(|| {
                PaymentError::Validation(format!(
                    "subscription webhook missing subscription_id (provider_event_type={})",
                    event.provider_event_type
                ))
            })?;
            let result = provider.get(sub_id).await?;
            upsert_subscription(db, &event.provider, &result, neutral).await?;
            sync_subscription_items(db, &event.provider, sub_id, &result).await?;
        }
        NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated => {
            let cust_id = ids.customer_id.as_deref().ok_or_else(|| {
                PaymentError::Validation(format!(
                    "customer webhook missing customer_id (provider_event_type={})",
                    event.provider_event_type
                ))
            })?;
            let snapshot = provider.extract_customer_snapshot(event);
            update_customer_mirror(db, &event.provider, cust_id, snapshot.as_ref()).await?;
        }
        NeutralEventKind::PaymentSucceeded
        | NeutralEventKind::PaymentFailed
        | NeutralEventKind::PaymentRefunded
        | NeutralEventKind::PaymentDisputed
        | NeutralEventKind::InvoicePaid
        | NeutralEventKind::InvoiceFailed => {
            let Some(snapshot) = provider.extract_payment_snapshot(event) else {
                // Providers may legitimately omit snapshots for events they
                // can't translate (e.g., adjustment events with no charge id).
                // Audit-only is correct here.
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

async fn upsert_subscription<C>(
    db: &C,
    provider: &str,
    result: &SubscriptionResult,
    neutral: NeutralEventKind,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
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

async fn sync_subscription_items<C>(
    db: &C,
    provider: &str,
    provider_subscription_id: &str,
    result: &SubscriptionResult,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
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

async fn upsert_transaction<C>(
    db: &C,
    provider: &str,
    snapshot: &PaymentSnapshot,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
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
            // Preserve original paid_at across refund/dispute events — the
            // original payment time is the canonical reference.
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

/// Update an existing `payments_customers` mirror row from a provider-supplied
/// snapshot.
///
/// We deliberately do NOT insert when no row exists: `user_id` is `NOT NULL`
/// and only the app knows which user a provider-side customer maps to
/// (creation goes through [`crate::payments::CustomerStore::create_customer`]
/// plus an app-controlled DB write). Out-of-band customers (created in the
/// Stripe dashboard, say) are logged but not synthesized.
async fn update_customer_mirror<C>(
    db: &C,
    provider: &str,
    provider_customer_id: &str,
    snapshot: Option<&CustomerSnapshot>,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
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

    let mut am: customer::ActiveModel = model.into();
    if let Some(snap) = snapshot {
        if let Some(email) = snap.email.as_ref() {
            am.email = Set(email.clone());
        }
        am.provider_metadata = Set(snap.provider_metadata.clone());
    }
    am.updated_at = Set(Utc::now().to_rfc3339());
    am.update(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    Ok(())
}

async fn mark_processed<C>(db: &C, event: &WebhookEvent) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let model = webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?
        .ok_or_else(|| PaymentError::Internal("webhook event vanished after insert".into()))?;
    let mut am: webhook_event::ActiveModel = model.into();
    am.processed_at = Set(Some(Utc::now().to_rfc3339()));
    am.process_error = Set(None);
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
                let headers = req.headers().clone();
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
