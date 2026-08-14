use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::ChannelEvent;

#[test]
fn parses_content_and_meta() {
    let params = json!({
        "content": "hello",
        "meta": { "channel_id": "C1", "author": "karl" },
    });
    assert_eq!(
        ChannelEvent::parse_notification_params(Some(&params)),
        Some(ChannelEvent {
            content: "hello".to_string(),
            meta: BTreeMap::from([
                ("channel_id".to_string(), "C1".to_string()),
                ("author".to_string(), "karl".to_string()),
            ]),
        })
    );
}

#[test]
fn meta_is_optional_and_unknown_fields_are_tolerated() {
    let params = json!({ "content": "hello", "extra": 42 });
    assert_eq!(
        ChannelEvent::parse_notification_params(Some(&params)),
        Some(ChannelEvent {
            content: "hello".to_string(),
            meta: BTreeMap::new(),
        })
    );
}

#[test]
fn drops_malformed_events() {
    assert_eq!(ChannelEvent::parse_notification_params(None), None);
    assert_eq!(
        ChannelEvent::parse_notification_params(Some(&json!("not an object"))),
        None
    );
    assert_eq!(
        ChannelEvent::parse_notification_params(Some(&json!({ "meta": {} }))),
        None
    );
    assert_eq!(
        ChannelEvent::parse_notification_params(Some(&json!({ "content": 42 }))),
        None
    );
}

#[test]
fn non_string_meta_values_are_ignored() {
    let params = json!({
        "content": "hello",
        "meta": { "ok": "yes", "bad": { "nested": true }, "num": 7 },
    });
    assert_eq!(
        ChannelEvent::parse_notification_params(Some(&params)),
        Some(ChannelEvent {
            content: "hello".to_string(),
            meta: BTreeMap::from([("ok".to_string(), "yes".to_string())]),
        })
    );
}
