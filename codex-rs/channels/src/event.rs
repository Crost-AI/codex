use std::collections::BTreeMap;

use serde_json::Value;

/// A validated channel event pushed by an MCP server via the
/// `notifications/codex/channel` notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelEvent {
    /// Verbatim event text supplied by the channel server.
    pub content: String,
    /// Optional string-to-string metadata rendered as envelope attributes.
    pub meta: BTreeMap<String, String>,
}

impl ChannelEvent {
    /// Validates notification params. Returns `None` for malformed events
    /// (missing or non-string `content`), which are dropped silently.
    /// Non-string `meta` values are ignored; unknown fields are tolerated.
    pub fn parse_notification_params(params: Option<&Value>) -> Option<Self> {
        let params = params?.as_object()?;
        let content = params.get("content")?.as_str()?.to_string();
        let meta = params
            .get("meta")
            .and_then(Value::as_object)
            .map(|meta| {
                meta.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self { content, meta })
    }
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
