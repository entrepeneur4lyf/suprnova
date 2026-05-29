//! WebPushChannel — end-to-end through the vendored web-push client.
//!
//! These tests stand a `wiremock` mock server in for a real push service
//! (FCM / Mozilla autopush / Apple) and exercise four contracts:
//!
//! 1. Happy path: a 201 from the push service drives delivery through to
//!    completion with exactly one HTTP request.
//! 2. Subscription gone (404): treated as a non-fatal warn — callers are
//!    expected to remove the dead subscription, but dispatch succeeds.
//! 3. Malformed subscription JSON: surfaces as a `FrameworkError`
//!    carrying enough context to identify the decode failure.
//! 4. 5xx from the push service: propagates as a `FrameworkError` whose
//!    message surfaces the upstream status so operators can triage.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::notifications::channels::webpush::WebPushChannel;
use suprnova::notifications::{
    Channel, DynNotification, Notifiable, Notification, NotificationDispatcher,
};
use suprnova::web_push::{EndpointPolicy, VapidKey, VapidSigner, WebPushClient};
use tracing_test::traced_test;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Public RFC-8291-style test vectors for receiver p256dh + auth. These are
// the keys the upstream `suprnova-web-push` crate's client tests use; the
// channel doesn't care about their content beyond "they're valid base64",
// since wiremock doesn't actually decrypt the body.
const RECEIVER_P256DH: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const RECEIVER_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PingNote;

impl Notification for PingNote {
    fn notification_name() -> &'static str {
        "PingNote"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["webpush"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "title": "ping" })
    }
}

/// Notifiable that returns a JSON-encoded `SubscriptionInfo` for the
/// `"webpush"` channel — the shape callers will receive from the browser
/// via `PushSubscription.toJSON()` and store verbatim.
struct Subscriber {
    endpoint: String,
}

impl Notifiable for Subscriber {
    fn route_for(&self, channel: &str) -> Option<String> {
        if channel == "webpush" {
            Some(
                serde_json::to_string(&serde_json::json!({
                    "endpoint": self.endpoint,
                    "keys": {
                        "p256dh": RECEIVER_P256DH,
                        "auth": RECEIVER_AUTH,
                    },
                }))
                .unwrap(),
            )
        } else {
            None
        }
    }
}

fn build_channel() -> Arc<WebPushChannel> {
    let signer = VapidSigner::new(VapidKey::generate());
    // wiremock serves `http://127.0.0.1:<port>` URLs; the production-default
    // Strict endpoint policy would reject those. Tests opt into AllowAny.
    let client = Arc::new(
        WebPushClient::new(signer, "mailto:admin@suprnova.dev")
            .with_endpoint_policy(EndpointPolicy::AllowAny),
    );
    Arc::new(WebPushChannel::new(client, 60))
}

#[tokio::test]
#[serial]
async fn webpush_channel_posts_to_subscription_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let channel: Arc<dyn Channel> = build_channel();
    let dispatcher = NotificationDispatcher::new().register_channel(channel);

    dispatcher
        .notify(
            &Subscriber {
                endpoint: format!("{}/push", server.uri()),
            },
            &PingNote,
        )
        .await
        .expect("happy-path delivery succeeds");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "exactly one POST to /push");
}

#[tokio::test]
#[serial]
#[traced_test]
async fn webpush_channel_treats_subscription_gone_as_non_fatal() {
    // The push service replies 404 — per WebPushClient that maps to
    // WebPushError::SubscriptionGone, which the channel translates into
    // Ok(()) plus a warn. The caller's contract is "remove the stored
    // subscription from your DB"; the dispatch itself does not fail.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let channel: Arc<dyn Channel> = build_channel();
    let dispatcher = NotificationDispatcher::new().register_channel(channel);

    let endpoint = format!("{}/push", server.uri());
    dispatcher
        .notify(
            &Subscriber {
                endpoint: endpoint.clone(),
            },
            &PingNote,
        )
        .await
        .expect("404 from the push service must not fail dispatch");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "channel attempted exactly one POST");

    // Pin the operator-paper-trail contract — the doc comment promises a
    // structured warn on subscription gone so ops can act on dead
    // subscriptions even though dispatch returned Ok. If a future
    // refactor silently drops the warn, this test must fail.
    assert!(
        logs_contain("subscription gone"),
        "expected the subscription-gone warn message"
    );
    assert!(
        logs_contain(&endpoint),
        "expected the structured endpoint field to make it into the warn event"
    );
    assert!(
        logs_contain("PingNote"),
        "expected the notification name in the warn event"
    );
}

#[tokio::test]
#[serial]
async fn webpush_channel_returns_error_for_malformed_subscription_json() {
    // The "route" the Notifiable returns must be valid SubscriptionInfo
    // JSON. A garbage route surfaces as a contextual decode error —
    // callers can match on the "subscription JSON decode" substring to
    // distinguish bad routes from upstream push-service failures.
    // This test exercises the JSON-decode error path, which fires before
    // the endpoint policy runs — so the policy choice is moot here, but we
    // keep AllowAny for consistency with the other tests in this file.
    let channel = WebPushChannel::new(
        Arc::new(
            WebPushClient::new(
                VapidSigner::new(VapidKey::generate()),
                "mailto:admin@suprnova.dev",
            )
            .with_endpoint_policy(EndpointPolicy::AllowAny),
        ),
        60,
    );

    let err = (&channel as &dyn Channel)
        .deliver("this is not valid subscription json", &PingNote)
        .await
        .expect_err("malformed route must surface as Err");

    let msg = format!("{err}");
    assert!(
        msg.contains("WebPushChannel"),
        "expected channel context in error: {msg}"
    );
    assert!(
        msg.contains("subscription JSON decode"),
        "expected decode-failure context in error: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn webpush_channel_propagates_push_service_5xx() {
    // 500 from the push service is a real failure — the channel surfaces
    // it as Err so the dispatcher returns it to the caller for retry /
    // metrics. Assert the error string mentions the status so operators
    // triaging logs can tell upstream errors from local errors apart.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal push error"))
        .mount(&server)
        .await;

    let channel = build_channel();
    let subscription_route = Subscriber {
        endpoint: format!("{}/push", server.uri()),
    }
    .route_for("webpush")
    .expect("subscriber returns a webpush route");

    let err = (&*channel as &dyn Channel)
        .deliver(&subscription_route, &PingNote as &dyn DynNotification)
        .await
        .expect_err("5xx must surface as Err");

    let msg = format!("{err}");
    assert!(
        msg.contains("WebPushChannel"),
        "expected channel context in error: {msg}"
    );
    assert!(
        msg.contains("500"),
        "expected upstream status in error: {msg}"
    );
}
