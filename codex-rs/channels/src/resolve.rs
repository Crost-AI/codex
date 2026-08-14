use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::policy::ChannelsPolicy;
use crate::spec::ChannelSpec;
use crate::spec::split_channel_entries;

/// Why a channel entry is (or is not) active for this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelResolutionState {
    /// The entry is active for these configured MCP server names.
    Active { servers: Vec<String> },
    /// `[channels] enabled` resolved to `false`.
    DisabledByConfig,
    /// The entry is not present in the `[channels] allowed` allowlist.
    NotAllowed,
    /// The entry could not be parsed.
    InvalidSpec { error: String },
    /// The entry parsed but matched no configured MCP server.
    NoMatchingServer,
}

/// The resolution outcome for a single requested channel entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelResolution {
    /// The entry as requested (post comma-splitting and trimming).
    pub entry: String,
    pub state: ChannelResolutionState,
}

/// The session's resolved channel opt-ins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelSetup {
    /// One resolution per requested entry, in request order (deduplicated).
    pub resolutions: Vec<ChannelResolution>,
    /// MCP server names allowed to deliver channel events this session.
    pub active_servers: BTreeSet<String>,
}

impl ChannelSetup {
    pub fn is_server_active(&self, server_name: &str) -> bool {
        self.active_servers.contains(server_name)
    }
}

/// Resolves the requested channel entries against the effective policy and
/// the configured MCP servers.
///
/// `plugin_ids_by_server` maps configured MCP server names to the id of the
/// plugin that contributed them (for `plugin:<id>` entries).
pub fn resolve_channels<S: AsRef<str>>(
    raw_entries: &[S],
    policy: &ChannelsPolicy,
    configured_servers: &BTreeSet<String>,
    plugin_ids_by_server: &BTreeMap<String, String>,
) -> ChannelSetup {
    let mut setup = ChannelSetup::default();
    let mut seen = BTreeSet::new();
    for entry in split_channel_entries(raw_entries) {
        if !seen.insert(entry.clone()) {
            continue;
        }
        let state = resolve_entry(&entry, policy, configured_servers, plugin_ids_by_server);
        if let ChannelResolutionState::Active { servers } = &state {
            setup.active_servers.extend(servers.iter().cloned());
        }
        setup.resolutions.push(ChannelResolution { entry, state });
    }
    setup
}

fn resolve_entry(
    entry: &str,
    policy: &ChannelsPolicy,
    configured_servers: &BTreeSet<String>,
    plugin_ids_by_server: &BTreeMap<String, String>,
) -> ChannelResolutionState {
    let spec = match ChannelSpec::parse(entry) {
        Ok(spec) => spec,
        Err(error) => {
            return ChannelResolutionState::InvalidSpec {
                error: error.to_string(),
            };
        }
    };
    if !policy.enabled {
        return ChannelResolutionState::DisabledByConfig;
    }
    if !policy.entry_allowed(&spec) {
        return ChannelResolutionState::NotAllowed;
    }
    let servers: Vec<String> = match &spec {
        ChannelSpec::Server(name) => {
            if configured_servers.contains(name) {
                vec![name.clone()]
            } else {
                Vec::new()
            }
        }
        ChannelSpec::Plugin(id) => plugin_ids_by_server
            .iter()
            .filter(|(server, plugin_id)| *plugin_id == id && configured_servers.contains(*server))
            .map(|(server, _)| server.clone())
            .collect(),
    };
    if servers.is_empty() {
        ChannelResolutionState::NoMatchingServer
    } else {
        ChannelResolutionState::Active { servers }
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
