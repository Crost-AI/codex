use std::collections::BTreeMap;

/// Channel event bodies are truncated past this size so a single event can
/// never flood the model context.
pub const CHANNEL_EVENT_MAX_CONTENT_BYTES: usize = 100_000;

const TRUNCATION_MARKER: &str = "\n[truncated: channel event exceeded 100000 bytes]";

/// Renders a channel event as the model-visible envelope:
///
/// ```text
/// <channel source="<server-name>" key="value">
/// <content verbatim>
/// </channel>
/// ```
///
/// `source` is always the connected MCP server name; a meta entry can never
/// override it. Meta keys must match `[A-Za-z0-9_]+` (others are dropped),
/// the reserved key `source` is dropped, and attribute values are
/// entity-escaped so a value can never break out of the tag. The body is kept
/// verbatim apart from truncation.
pub fn render_channel_event(
    server_name: &str,
    content: &str,
    meta: &BTreeMap<String, String>,
) -> String {
    let mut attributes = format!("source=\"{}\"", escape_attribute(server_name));
    for (key, value) in meta {
        if key == "source" || !is_valid_meta_key(key) {
            continue;
        }
        attributes.push_str(&format!(" {key}=\"{}\"", escape_attribute(value)));
    }
    let body = truncate_content(content);
    format!("<channel {attributes}>\n{body}\n</channel>")
}

fn is_valid_meta_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn escape_attribute(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\n' => escaped.push_str("&#10;"),
            '\r' => escaped.push_str("&#13;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn truncate_content(content: &str) -> String {
    if content.len() <= CHANNEL_EVENT_MAX_CONTENT_BYTES {
        return content.to_string();
    }
    let mut cut = CHANNEL_EVENT_MAX_CONTENT_BYTES;
    while !content.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &content[..cut])
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
