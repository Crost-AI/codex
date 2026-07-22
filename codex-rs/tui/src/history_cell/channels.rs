//! History cell for `/channels`: the session's channel opt-ins and how each
//! entry resolved, including which config source the matched server came from
//! and whether the connected server actually declares the `codex/channel`
//! capability.

use codex_app_server_protocol::McpServerStatus;
use codex_channels::ChannelResolutionState;
use codex_channels::ChannelsPolicy;
use codex_channels::resolve_channels;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::PlainHistoryCell;

/// Renders `/channels` from the requested entries, the effective policy, and
/// the app-server's authoritative MCP server statuses.
pub(crate) fn new_channels_output(
    entries: &[String],
    policy: &ChannelsPolicy,
    statuses: &[McpServerStatus],
) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = vec![
        "/channels".magenta().into(),
        "".into(),
        vec!["📡  ".into(), "Channels".bold()].into(),
        "".into(),
    ];

    if entries.is_empty() {
        lines.push(
            "  • No channels are enabled for this session."
                .italic()
                .into(),
        );
        lines.push(
            "    Launch with --channels server:<name> (or set [channels] entries) to let an MCP server push events."
                .dim()
                .into(),
        );
        return PlainHistoryCell::new(lines);
    }

    let configured_servers: BTreeSet<String> =
        statuses.iter().map(|status| status.name.clone()).collect();
    // The app-server labels plugin-contributed servers via their source; use
    // that to resolve plugin:<id> entries against the authoritative list.
    let plugin_ids_by_server: BTreeMap<String, String> = statuses
        .iter()
        .filter_map(|status| {
            let source = status.source.as_deref()?;
            let plugin_id = source
                .strip_prefix("selected plugin `")
                .or_else(|| source.strip_prefix("plugin `"))?
                .strip_suffix('`')?;
            Some((status.name.clone(), plugin_id.to_string()))
        })
        .collect();
    let setup = resolve_channels(entries, policy, &configured_servers, &plugin_ids_by_server);

    for resolution in &setup.resolutions {
        let (state_span, servers): (Span<'static>, Vec<String>) = match &resolution.state {
            ChannelResolutionState::Active { servers } => ("active".green(), servers.clone()),
            ChannelResolutionState::DisabledByConfig => {
                ("blocked (channels disabled by config)".red(), Vec::new())
            }
            ChannelResolutionState::NotAllowed => (
                "blocked (not in [channels] allowed)".red(),
                Vec::new(),
            ),
            ChannelResolutionState::InvalidSpec { error } => {
                (format!("invalid ({error})").red(), Vec::new())
            }
            ChannelResolutionState::NoMatchingServer => {
                ("matched no configured MCP server".red(), Vec::new())
            }
        };
        lines.push(
            vec![
                "  • ".into(),
                resolution.entry.clone().bold(),
                " — ".dim(),
                state_span,
            ]
            .into(),
        );

        for server in servers {
            let Some(status) = statuses.iter().find(|status| status.name == server) else {
                continue;
            };
            let mut server_line: Vec<Span<'static>> =
                vec!["    • ".into(), server.clone().into()];
            if let Some(source) = &status.source {
                server_line.push(format!(" (from {source}").dim());
                if status.overridden_sources.is_empty() {
                    server_line.push(")".dim());
                } else {
                    server_line.push(
                        format!(
                            "; overrides definition from {})",
                            status.overridden_sources.join(", ")
                        )
                        .dim(),
                    );
                }
            }
            lines.push(server_line.into());
            let capability_line: Line<'static> = match status.declares_channel_capability {
                Some(true) => "      • declares codex/channel".green().into(),
                Some(false) => {
                    "      • connected but does NOT declare the codex/channel capability"
                        .red()
                        .into()
                }
                None => "      • not connected yet (capability unknown)".dim().into(),
            };
            lines.push(capability_line);
        }
    }

    PlainHistoryCell::new(lines)
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
