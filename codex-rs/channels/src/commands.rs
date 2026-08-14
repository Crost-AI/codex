//! Host-executed channel slash commands.
//!
//! A channel server may opt into having a small set of session commands
//! (`/status`, `/channels`, `/help`) answered by the host instead of the
//! model: it declares a `commands` reply-routing descriptor as the value of
//! its [`crate::CHANNEL_CAPABILITY`] capability. When an inbound event's
//! body *is* such a command (and the event is not bot-authored), the host
//! executes it and routes the output back by calling the declared reply
//! tool — the event never reaches the model. Everything else, including
//! unrecognized `/commands`, injects as a normal channel event.

use serde_json::Value;

use crate::ChannelResolutionState;
use crate::ChannelSetup;

/// Reply-routing descriptor a channel server declares under the `"commands"`
/// key of its [`crate::CHANNEL_CAPABILITY`] capability value:
///
/// ```json
/// "codex/channel": {
///   "commands": {
///     "reply_tool": "send_message",
///     "target_meta": "channel_id",
///     "target_arg": "channel_id",
///     "content_arg": "content",
///     "extra_args": { "thread_ts": "thread_ts" }
///   }
/// }
/// ```
///
/// The host calls `reply_tool` with
/// `{target_arg: <event meta[target_meta]>, content_arg: <output>}`, plus
/// each `extra_args` entry (tool argument name → event meta key) whose meta
/// key is present on the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelCommandsDescriptor {
    /// Tool to call with the command output (e.g. `send_message`).
    pub reply_tool: String,
    /// Event meta key holding the reply target (e.g. `channel_id`). Events
    /// missing this key cannot be replied to and are not intercepted.
    pub target_meta: String,
    /// Tool argument name to pass the target as.
    pub target_arg: String,
    /// Tool argument name to pass the command output as.
    pub content_arg: String,
    /// Additional `(tool argument name, event meta key)` pairs copied into
    /// the call when the meta key is present on the event.
    pub extra_args: Vec<(String, String)>,
}

/// Parses the `commands` descriptor out of a [`crate::CHANNEL_CAPABILITY`]
/// capability *value*. Returns `None` when the value has no `commands`
/// object or any required field is missing, empty, or not a string — a
/// malformed descriptor disables host-side commands rather than producing
/// misrouted tool calls.
pub fn parse_channel_commands_descriptor(
    capability_value: &serde_json::Map<String, Value>,
) -> Option<ChannelCommandsDescriptor> {
    let commands = capability_value.get("commands")?.as_object()?;
    let required = |key: &str| -> Option<String> {
        commands
            .get(key)?
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let mut extra_args: Vec<(String, String)> = Vec::new();
    if let Some(map) = commands.get("extra_args").and_then(Value::as_object) {
        for (arg, meta_key) in map {
            if let Some(meta_key) = meta_key.as_str().filter(|value| !value.is_empty())
                && !arg.is_empty()
            {
                extra_args.push((arg.clone(), meta_key.to_string()));
            }
        }
    }
    Some(ChannelCommandsDescriptor {
        reply_tool: required("reply_tool")?,
        target_meta: required("target_meta")?,
        target_arg: required("target_arg")?,
        content_arg: required("content_arg")?,
        extra_args,
    })
}

/// Extracts the slash-command name from an inbound channel event body, if
/// the body *is* a slash command: leading `/` followed by a `[A-Za-z0-9_-]+`
/// name, then end-of-input or whitespace (any trailing text is ignored).
/// Returns the name lowercased. Bodies like `/path/to/file` (name followed
/// by non-whitespace) or `run /status` (command not leading) are not
/// commands.
pub fn parse_channel_command(content: &str) -> Option<String> {
    let rest = content.trim().strip_prefix('/')?;
    let name_end = rest
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }
    match rest[name_end..].chars().next() {
        None => {}
        Some(c) if c.is_whitespace() => {}
        Some(_) => return None,
    }
    Some(rest[..name_end].to_ascii_lowercase())
}

/// Help text for host-executed channel slash commands (`/help`).
pub fn channel_command_help() -> String {
    "Commands available over this channel (run by the Codex host, not the agent):\n\
     •  /help — this list\n\
     •  /status (or /session) — session id, model, working directory, approval/sandbox policy\n\
     •  /channels — channel entries and their resolution state\n\
     Anything else — including other /commands — goes to the agent as a normal message."
        .to_string()
}

/// Renders a channel `/channels` reply from the session's resolved setup.
pub fn channel_setup_status_text(setup: &ChannelSetup) -> String {
    if setup.resolutions.is_empty() {
        return "No channels enabled for this session. Opt in per session with \
                `codex --channels server:<name>` or `codex --channels plugin:<id>`."
            .to_string();
    }
    let mut lines = vec![format!("Channels ({} entries):", setup.resolutions.len())];
    for resolution in &setup.resolutions {
        let state = match &resolution.state {
            ChannelResolutionState::Active { servers } => {
                format!("active (servers: {})", servers.join(", "))
            }
            ChannelResolutionState::DisabledByConfig => {
                "blocked: [channels] enabled = false in config".to_string()
            }
            ChannelResolutionState::NotAllowed => {
                "blocked: not in the [channels] allowed list".to_string()
            }
            ChannelResolutionState::InvalidSpec { error } => format!("invalid: {error}"),
            ChannelResolutionState::NoMatchingServer => {
                "matched no configured MCP server".to_string()
            }
        };
        lines.push(format!("  {} — {}", resolution.entry, state));
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
