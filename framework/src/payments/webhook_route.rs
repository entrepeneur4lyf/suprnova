//! Webhook ingress route for the payments subsystem.
//!
//! Registers `POST /webhooks/payments/{provider}` on a Suprnova `Router`.
//! The handler:
//! 1. Resolves the named provider from `PaymentProviderRegistry`.
//! 2. Verifies the inbound signature via `WebhookHandler::verify`.
//! 3. Parses the body into a neutral `WebhookEvent`.
//! 4. Short-circuits with 200 if `(provider, provider_event_id)` already
//!    exists in `payments_webhook_events` (idempotency for retrying providers).
//! 5. Inserts the audit row and marks it processed.
//!
//! T9+ will expand `process_webhook` to hydrate mirror tables and dispatch
//! typed framework events. For T8 only persistence + idempotency are in scope.

use crate::http::{text, HttpResponse, Response};
use crate::payments::{
    entities::webhook_event,
    PaymentError, PaymentProviderRegistry, WebhookContext, WebhookEvent,
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

    // 8. Process (stub — T9+ expands)
    if let Err(e) = process_webhook(db, &event).await {
        tracing::error!(error = %e, "webhook processing failed");
        let _ = mark_failed(db, &event, &format!("{e}")).await;
        return err_response(500, "process");
    }

    let _ = mark_processed(db, &event).await;
    text("ok")
}

async fn process_webhook(
    _db: &DatabaseConnection,
    _event: &WebhookEvent,
) -> Result<(), PaymentError> {
    // T9+ hydrates mirror tables and dispatches typed framework events.
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
