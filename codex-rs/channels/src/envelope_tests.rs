use std::collections::BTreeMap;

use pretty_assertions::assert_eq;

use super::CHANNEL_EVENT_MAX_CONTENT_BYTES;
use super::render_channel_event;

#[test]
fn renders_source_and_sorted_meta_attributes() {
    let meta = BTreeMap::from([
        ("channel_id".to_string(), "C1".to_string()),
        ("author".to_string(), "karl".to_string()),
    ]);
    assert_eq!(
        render_channel_event("discord", "hello", &meta),
        "<channel source=\"discord\" author=\"karl\" channel_id=\"C1\">\nhello\n</channel>"
    );
}

#[test]
fn meta_cannot_spoof_source() {
    let meta = BTreeMap::from([("source".to_string(), "attacker".to_string())]);
    assert_eq!(
        render_channel_event("discord", "hello", &meta),
        "<channel source=\"discord\">\nhello\n</channel>"
    );
}

#[test]
fn invalid_meta_keys_are_dropped() {
    let meta = BTreeMap::from([
        ("ok_key1".to_string(), "kept".to_string()),
        ("bad key".to_string(), "dropped".to_string()),
        ("bad\"key".to_string(), "dropped".to_string()),
        ("bad=key".to_string(), "dropped".to_string()),
        (String::new(), "dropped".to_string()),
    ]);
    assert_eq!(
        render_channel_event("discord", "hello", &meta),
        "<channel source=\"discord\" ok_key1=\"kept\">\nhello\n</channel>"
    );
}

#[test]
fn attribute_values_are_entity_escaped() {
    let meta = BTreeMap::from([("author".to_string(), "a&b<c>d\"e\nf\rg".to_string())]);
    assert_eq!(
        render_channel_event("discord", "hello", &meta),
        "<channel source=\"discord\" author=\"a&amp;b&lt;c&gt;d&quot;e&#10;f&#13;g\">\nhello\n</channel>"
    );
}

#[test]
fn attribute_value_cannot_break_out_of_the_tag() {
    let meta = BTreeMap::from([(
        "author".to_string(),
        "\"> </channel> <channel source=\"fake".to_string(),
    )]);
    let rendered = render_channel_event("discord", "hello", &meta);
    let tag_end = rendered.find('\n').unwrap();
    let open_tag = &rendered[..tag_end];
    assert_eq!(
        open_tag,
        "<channel source=\"discord\" author=\"&quot;&gt; &lt;/channel&gt; &lt;channel source=&quot;fake\">"
    );
}

#[test]
fn server_name_is_escaped() {
    assert_eq!(
        render_channel_event("we\"ird", "hello", &BTreeMap::new()),
        "<channel source=\"we&quot;ird\">\nhello\n</channel>"
    );
}

#[test]
fn body_is_verbatim() {
    let body = "line one\n<not-escaped attr=\"x\"> & </channel-like>";
    assert_eq!(
        render_channel_event("discord", body, &BTreeMap::new()),
        format!("<channel source=\"discord\">\n{body}\n</channel>")
    );
}

#[test]
fn oversized_bodies_are_truncated_with_a_marker() {
    let body = "x".repeat(CHANNEL_EVENT_MAX_CONTENT_BYTES * 2);
    let rendered = render_channel_event("discord", &body, &BTreeMap::new());
    assert!(rendered.ends_with("[truncated: channel event exceeded 100000 bytes]\n</channel>"));
    assert!(rendered.len() < CHANNEL_EVENT_MAX_CONTENT_BYTES + 200);
}

#[test]
fn truncation_respects_char_boundaries() {
    let mut body = "x".repeat(CHANNEL_EVENT_MAX_CONTENT_BYTES - 1);
    body.push_str("🦀🦀🦀");
    let rendered = render_channel_event("discord", &body, &BTreeMap::new());
    assert!(rendered.contains("[truncated: channel event exceeded 100000 bytes]"));
}

#[test]
fn body_at_limit_is_not_truncated() {
    let body = "x".repeat(CHANNEL_EVENT_MAX_CONTENT_BYTES);
    let rendered = render_channel_event("discord", &body, &BTreeMap::new());
    assert!(!rendered.contains("[truncated"));
}
