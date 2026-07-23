use pretty_assertions::assert_eq;

use super::*;
use crate::ChannelResolution;

fn capability_value(json: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    json.as_object().expect("test value is an object").clone()
}

#[test]
fn parses_commands_descriptor() {
    let descriptor = parse_channel_commands_descriptor(&capability_value(serde_json::json!({
        "commands": {
            "reply_tool": "send_message",
            "target_meta": "channel_id",
            "target_arg": "channel_id",
            "content_arg": "content",
            "extra_args": { "thread_ts": "thread_ts", "": "dropped", "bad": "" }
        }
    })))
    .expect("valid descriptor");
    assert_eq!(descriptor.reply_tool, "send_message");
    assert_eq!(descriptor.target_meta, "channel_id");
    assert_eq!(descriptor.target_arg, "channel_id");
    assert_eq!(descriptor.content_arg, "content");
    assert_eq!(
        descriptor.extra_args,
        vec![("thread_ts".to_string(), "thread_ts".to_string())]
    );
}

#[test]
fn commands_descriptor_optional_and_strict() {
    // Empty capability value (the pre-commands form) — no descriptor.
    assert!(parse_channel_commands_descriptor(&capability_value(serde_json::json!({}))).is_none());
    // commands present but a required field missing, wrong type, or empty.
    assert!(
        parse_channel_commands_descriptor(&capability_value(serde_json::json!({
            "commands": { "reply_tool": "send_message" }
        })))
        .is_none()
    );
    assert!(
        parse_channel_commands_descriptor(&capability_value(serde_json::json!({
            "commands": {
                "reply_tool": "send_message",
                "target_meta": "channel_id",
                "target_arg": 7,
                "content_arg": "content"
            }
        })))
        .is_none()
    );
    assert!(
        parse_channel_commands_descriptor(&capability_value(serde_json::json!({
            "commands": {
                "reply_tool": "",
                "target_meta": "channel_id",
                "target_arg": "channel_id",
                "content_arg": "content"
            }
        })))
        .is_none()
    );
    // extra_args stays optional.
    let minimal = parse_channel_commands_descriptor(&capability_value(serde_json::json!({
        "commands": {
            "reply_tool": "send_message",
            "target_meta": "channel_id",
            "target_arg": "channel_id",
            "content_arg": "content"
        }
    })))
    .expect("minimal descriptor");
    assert!(minimal.extra_args.is_empty());
}

#[test]
fn parse_channel_command_extracts_leading_slash_word() {
    assert_eq!(parse_channel_command("/status"), Some("status".to_string()));
    assert_eq!(
        parse_channel_command("  /STATUS  "),
        Some("status".to_string())
    );
    assert_eq!(
        parse_channel_command("/channels please"),
        Some("channels".to_string())
    );
    assert_eq!(
        parse_channel_command("/my_cmd-2"),
        Some("my_cmd-2".to_string())
    );
}

#[test]
fn parse_channel_command_rejects_non_commands() {
    // Not leading.
    assert_eq!(parse_channel_command("run /status"), None);
    // Paths and non-word followers.
    assert_eq!(parse_channel_command("/path/to/file"), None);
    assert_eq!(parse_channel_command("/status?"), None);
    // Bare or empty.
    assert_eq!(parse_channel_command("/"), None);
    assert_eq!(parse_channel_command(""), None);
    assert_eq!(parse_channel_command("hello"), None);
    // Non-ASCII name start.
    assert_eq!(parse_channel_command("/émoji"), None);
}

#[test]
fn channel_setup_status_text_lists_every_resolution() {
    let empty = channel_setup_status_text(&ChannelSetup::default());
    assert!(empty.contains("No channels enabled"));

    let setup = ChannelSetup {
        resolutions: vec![
            ChannelResolution {
                entry: "server:discord".to_string(),
                state: ChannelResolutionState::Active {
                    servers: vec!["discord".to_string()],
                },
            },
            ChannelResolution {
                entry: "plugin:missing".to_string(),
                state: ChannelResolutionState::NoMatchingServer,
            },
        ],
        active_servers: ["discord".to_string()].into(),
    };
    let text = channel_setup_status_text(&setup);
    assert!(text.contains("Channels (2 entries):"));
    assert!(text.contains("server:discord — active (servers: discord)"));
    assert!(text.contains("plugin:missing — matched no configured MCP server"));
}
