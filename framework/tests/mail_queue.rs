use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use suprnova::FrameworkError;
use suprnova::async_trait;
use suprnova::mail::address::Attachment;
use suprnova::mail::memory::InMemoryMailTransport;
use suprnova::mail::send_job::SendMailJob;
use suprnova::mail::{Address, Mail, Mailable};
use suprnova::queue::Queue;
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::queue::worker::{WorkerConfig, register_job, run_worker};
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct WelcomeMail {
    name: String,
}

#[async_trait]
impl Mailable for WelcomeMail {
    fn mailable_name() -> &'static str {
        "WelcomeMail"
    }
    fn subject(&self) -> String {
        format!("Welcome, {}", self.name)
    }
    fn text_template_source(&self) -> Option<String> {
        Some("Hi {{ name }}!".into())
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EmptyBodyMail;

#[async_trait]
impl Mailable for EmptyBodyMail {
    fn mailable_name() -> &'static str {
        "EmptyBodyMail"
    }
    fn subject(&self) -> String {
        "nope".into()
    }
    // No html_template_source, no text_template_source.
}

#[tokio::test]
#[serial]
async fn mail_queue_dispatches_through_queue_and_send_job_renders_via_transport() {
    let capture = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(capture.clone());

    let _ = suprnova::mail::register_mailable_factory::<WelcomeMail>();
    register_job::<SendMailJob>();

    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    Mail::to("alice@example.org")
        .queue(WelcomeMail {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let handle = tokio::spawn(run_worker(
        driver.clone(),
        WorkerConfig {
            visibility_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(5),
            max_jobs: None,
        },
        CancellationToken::new(),
    ));

    for _ in 0..200 {
        if !capture.captured().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.abort();

    let msgs = capture.captured();
    assert_eq!(msgs.len(), 1, "queued mail must end up in the transport");
    assert_eq!(msgs[0].subject, "Welcome, Alice");
    assert_eq!(msgs[0].text.as_deref(), Some("Hi Alice!"));
    assert_eq!(msgs[0].to.len(), 1);
    assert_eq!(msgs[0].to[0].email, "alice@example.org");
    assert_eq!(msgs[0].from.email, "noreply@suprnova.dev");
}

#[tokio::test]
#[serial]
async fn mail_queue_rejects_mailable_without_any_body_at_push_time() {
    // Defense layer 1: MailBuilder::queue's empty-body guard must fire
    // before any envelope is created. The queue driver stays empty.
    let capture = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(capture.clone());

    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    let err = Mail::to("alice@example.org")
        .queue(EmptyBodyMail)
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("EmptyBodyMail"),
        "error mentions the Mailable name: {msg}"
    );
    assert!(
        msg.contains("text_template_source") || msg.contains("html_template_source"),
        "error suggests which methods to implement: {msg}"
    );

    // The queue stays empty — no envelope was committed.
    let popped = driver.pop(Duration::from_secs(60)).await.unwrap();
    assert!(popped.is_none(), "no envelope should have been pushed");
}

#[tokio::test]
#[serial]
async fn mail_queue_unregistered_mailable_surfaces_unknown_error_from_job() {
    // If a Mailable's factory isn't registered, SendMailJob::handle
    // returns `unknown mailable: {name}` from the registry. We invoke
    // handle directly rather than round-tripping through the worker so
    // the assertion is targeted at the registry lookup, not at
    // end-to-end retry/dead-letter behavior. This protects against
    // silent retry loops on a typo'd mailable_name.
    use suprnova::queue::Job;

    // A transport must be bound — handle resolves the transport AFTER
    // the registry lookup, but we still set one to keep failure modes
    // unambiguous.
    let _ = Mail::set_transport(Arc::new(InMemoryMailTransport::new()));

    let job = SendMailJob {
        to: vec!["alice@example.org".into()],
        cc: vec![],
        bcc: vec![],
        reply_to: vec![],
        from_override: None,
        mailable_name: "TotallyUnregisteredMailable".to_string(),
        mailable_payload: serde_json::json!({}),
        tags: vec![],
        metadata: Default::default(),
        priority: None,
        headers: vec![],
        return_path: None,
        subject_override: None,
        attachments: vec![],
    };
    let err = job.handle().await.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("unknown mailable"),
        "error names the missing registry entry: {msg}"
    );
    assert!(
        msg.contains("TotallyUnregisteredMailable"),
        "error names the missing mailable: {msg}"
    );
}

// Regression: push-time guard must match `MailBuilder::send`'s guard
// semantically. A Mailable with no template source but an override of
// `render_html` that returns Some(...) must be accepted by BOTH paths.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct OverriddenRenderMail;

#[async_trait]
impl Mailable for OverriddenRenderMail {
    fn mailable_name() -> &'static str {
        "OverriddenRenderMail"
    }
    fn subject(&self) -> String {
        "rendered".into()
    }
    // No template_source — relies entirely on the render override below.
    fn render_html(&self) -> Result<Option<String>, FrameworkError> {
        Ok(Some("<p>pre-rendered html, no template source</p>".into()))
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn mail_queue_accepts_mailable_that_overrides_render_without_template_source() {
    let capture = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(capture.clone());

    let _ = suprnova::mail::register_mailable_factory::<OverriddenRenderMail>();
    register_job::<SendMailJob>();

    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    // Must succeed — the override produces a body even though
    // html_template_source returns None.
    Mail::to("alice@example.org")
        .queue(OverriddenRenderMail)
        .await
        .expect("queue accepts mailables that override render_html");

    // And the worker delivers the pre-rendered html through the transport.
    let handle = tokio::spawn(run_worker(
        driver.clone(),
        WorkerConfig {
            visibility_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(5),
            max_jobs: None,
        },
        CancellationToken::new(),
    ));
    for _ in 0..200 {
        if !capture.captured().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.abort();

    let msgs = capture.captured();
    assert_eq!(msgs.len(), 1, "queued override-mailable must deliver");
    assert_eq!(
        msgs[0].html.as_deref(),
        Some("<p>pre-rendered html, no template source</p>")
    );
    assert!(
        msgs[0].text.is_none(),
        "no text body — only the html override produced output"
    );
}

// Regression: a builder-side `.subject("...")` override and `.attach(...)`
// extras applied to `Mail::to(...).queue(mailable)` must reach the rendered
// outgoing message exactly the way they would on the sync `.send(...)` path.
// Prior to the fix, both were silently dropped by `build_send_job`.
#[tokio::test]
#[serial]
async fn mail_queue_threads_builder_subject_override_and_attachments_to_worker() {
    let capture = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(capture.clone());

    let _ = suprnova::mail::register_mailable_factory::<WelcomeMail>();
    register_job::<SendMailJob>();

    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    let invoice = Attachment::new("invoice.txt", b"PAID".to_vec(), "text/plain");

    Mail::to("alice@example.org")
        .subject("Override Subject")
        .attach(invoice.clone())
        .queue(WelcomeMail {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let handle = tokio::spawn(run_worker(
        driver.clone(),
        WorkerConfig {
            visibility_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(5),
            max_jobs: None,
        },
        CancellationToken::new(),
    ));
    for _ in 0..200 {
        if !capture.captured().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.abort();

    let msgs = capture.captured();
    assert_eq!(msgs.len(), 1, "queued mail must end up in the transport");
    assert_eq!(
        msgs[0].subject, "Override Subject",
        "builder .subject(...) must override the mailable's render_subject on the queue path",
    );
    assert_eq!(
        msgs[0].attachments.len(),
        1,
        "builder .attach(...) must reach the rendered message on the queue path",
    );
    assert_eq!(msgs[0].attachments[0].filename, "invoice.txt");
    assert_eq!(msgs[0].attachments[0].content, b"PAID");
}
