//! ClientFrame + ServerFrame serde round-trips. Pins the wire
//! protocol shape — JS / native clients must serialize/deserialize
//! to exactly this JSON.

use serde_json::json;
use suprnova::broadcasting::{ClientFrame, ServerFrame};

#[test]
fn subscribe_frame_parses_without_data() {
    let raw = r#"{"action":"subscribe","channel":"chat.42"}"#;
    let parsed: ClientFrame = serde_json::from_str(raw).unwrap();
    match parsed {
        ClientFrame::Subscribe { channel, data } => {
            assert_eq!(channel, "chat.42");
            assert!(data.is_null(), "data defaults to Null when omitted");
        }
        _ => panic!("expected Subscribe"),
    }
}

#[test]
fn subscribe_with_auth_data_parses() {
    let raw = r#"{"action":"subscribe","channel":"chat.42","data":{"token":"abc"}}"#;
    let parsed: ClientFrame = serde_json::from_str(raw).unwrap();
    match parsed {
        ClientFrame::Subscribe { channel, data } => {
            assert_eq!(channel, "chat.42");
            assert_eq!(data["token"], "abc");
        }
        _ => panic!("expected Subscribe"),
    }
}

#[test]
fn unsubscribe_frame_parses() {
    let raw = r#"{"action":"unsubscribe","channel":"chat.42"}"#;
    let parsed: ClientFrame = serde_json::from_str(raw).unwrap();
    assert!(matches!(
        parsed,
        ClientFrame::Unsubscribe { channel } if channel == "chat.42"
    ));
}

#[test]
fn publish_frame_parses_with_event_and_data() {
    let raw = r#"{"action":"publish","channel":"chat.42","event":"MessagePosted","data":{"text":"hi"}}"#;
    let parsed: ClientFrame = serde_json::from_str(raw).unwrap();
    match parsed {
        ClientFrame::Publish { channel, event, data } => {
            assert_eq!(channel, "chat.42");
            assert_eq!(event, "MessagePosted");
            assert_eq!(data["text"], "hi");
        }
        _ => panic!("expected Publish"),
    }
}

#[test]
fn unknown_action_fails_to_parse() {
    let raw = r#"{"action":"nope","channel":"chat.42"}"#;
    let result: Result<ClientFrame, _> = serde_json::from_str(raw);
    assert!(result.is_err(), "unknown action should not parse");
}

#[test]
fn subscribed_frame_serializes() {
    let frame = ServerFrame::Subscribed { channel: "chat.42".into() };
    let json = serde_json::to_string(&frame).unwrap();
    assert!(json.contains(r#""action":"subscribed""#), "got: {json}");
    assert!(json.contains(r#""channel":"chat.42""#));
}

#[test]
fn event_frame_serializes() {
    let frame = ServerFrame::Event {
        channel: "chat.42".into(),
        event: "MessagePosted".into(),
        data: json!({ "text": "hi" }),
    };
    let json = serde_json::to_string(&frame).unwrap();
    assert!(json.contains(r#""action":"event""#));
    assert!(json.contains(r#""channel":"chat.42""#));
    assert!(json.contains(r#""event":"MessagePosted""#));
}

#[test]
fn error_frame_with_channel_serializes() {
    let frame = ServerFrame::Error {
        channel: Some("chat.42".into()),
        reason: "unauthorized".into(),
    };
    let json = serde_json::to_string(&frame).unwrap();
    assert!(json.contains(r#""action":"error""#));
    assert!(json.contains(r#""channel":"chat.42""#));
    assert!(json.contains(r#""reason":"unauthorized""#));
}

#[test]
fn error_frame_without_channel_omits_or_nulls() {
    let frame = ServerFrame::Error {
        channel: None,
        reason: "bad envelope".into(),
    };
    let json = serde_json::to_string(&frame).unwrap();
    assert!(json.contains(r#""action":"error""#));
    assert!(json.contains(r#""reason":"bad envelope""#));
    // channel may serialize as `"channel":null` or be omitted depending
    // on whether we annotate the field; both are acceptable. Just
    // verify the rest of the frame is intact.
}
