use codex_app_server_protocol::McpAuthStatus;
use codex_app_server_protocol::McpServerStatus;
use codex_channels::ChannelsPolicy;
use insta::assert_snapshot;

use super::new_channels_output;
use crate::history_cell::HistoryCell;

fn status(
    name: &str,
    source: Option<&str>,
    overridden_sources: &[&str],
    declares_channel_capability: Option<bool>,
) -> McpServerStatus {
    McpServerStatus {
        name: name.to_string(),
        server_info: None,
        tools: Default::default(),
        resources: Vec::new(),
        resource_templates: Vec::new(),
        auth_status: McpAuthStatus::Unsupported,
        source: source.map(str::to_string),
        overridden_sources: overridden_sources.iter().map(ToString::to_string).collect(),
        declares_channel_capability,
    }
}

fn render(cell: &dyn HistoryCell) -> String {
    cell.display_lines(80)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn channels_output_shows_every_resolution_state() {
    let entries = vec![
        "server:discord".to_string(),
        "server:missing".to_string(),
        "server:blocked".to_string(),
        "nonsense".to_string(),
    ];
    let policy = ChannelsPolicy {
        enabled: true,
        allowed: Some(vec![
            "server:discord".to_string(),
            "server:missing".to_string(),
        ]),
    };
    let statuses = vec![
        status(
            "discord",
            Some("config.toml (user)"),
            &["config.toml (project)"],
            Some(true),
        ),
        status("blocked", Some("config.toml (user)"), &[], Some(false)),
    ];
    let cell = new_channels_output(&entries, &policy, &statuses);
    assert_snapshot!(render(&cell));
}

#[test]
fn channels_output_flags_missing_capability_and_unknown_state() {
    let entries = vec!["server:discord".to_string(), "server:github".to_string()];
    let statuses = vec![
        status("discord", Some("config.toml (user)"), &[], Some(false)),
        status("github", Some("config.toml (user)"), &[], None),
    ];
    let cell = new_channels_output(&entries, &ChannelsPolicy::default(), &statuses);
    assert_snapshot!(render(&cell));
}

#[test]
fn channels_output_without_entries_points_at_the_flag() {
    let cell = new_channels_output(&[], &ChannelsPolicy::default(), &[]);
    assert_snapshot!(render(&cell));
}

#[test]
fn channels_output_resolves_plugin_entries_from_source_labels() {
    let entries = vec!["plugin:acme".to_string()];
    let statuses = vec![
        status("acme-chat", Some("plugin `acme`"), &[], Some(true)),
        status("other", Some("config.toml (user)"), &[], Some(true)),
    ];
    let cell = new_channels_output(&entries, &ChannelsPolicy::default(), &statuses);
    let rendered = render(&cell);
    assert!(rendered.contains("plugin:acme"));
    assert!(rendered.contains("acme-chat"));
    assert!(!rendered.contains("• other"));
}
