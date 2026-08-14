use std::collections::BTreeMap;
use std::collections::BTreeSet;

use pretty_assertions::assert_eq;

use super::ChannelResolution;
use super::ChannelResolutionState;
use super::resolve_channels;
use crate::policy::ChannelsPolicy;

fn servers(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(ToString::to_string).collect()
}

#[test]
fn resolves_active_server_entry() {
    let setup = resolve_channels(
        &["server:discord"],
        &ChannelsPolicy::default(),
        &servers(&["discord", "github"]),
        &BTreeMap::new(),
    );
    assert_eq!(
        setup.resolutions,
        vec![ChannelResolution {
            entry: "server:discord".to_string(),
            state: ChannelResolutionState::Active {
                servers: vec!["discord".to_string()],
            },
        }]
    );
    assert!(setup.is_server_active("discord"));
    assert!(!setup.is_server_active("github"));
}

#[test]
fn configured_mcp_server_is_not_active_without_opt_in() {
    let setup = resolve_channels(
        &[] as &[&str],
        &ChannelsPolicy::default(),
        &servers(&["discord"]),
        &BTreeMap::new(),
    );
    assert_eq!(setup.resolutions, Vec::new());
    assert!(!setup.is_server_active("discord"));
}

#[test]
fn disabled_config_blocks_every_entry() {
    let policy = ChannelsPolicy {
        enabled: false,
        allowed: None,
    };
    let setup = resolve_channels(
        &["server:discord"],
        &policy,
        &servers(&["discord"]),
        &BTreeMap::new(),
    );
    assert_eq!(
        setup.resolutions[0].state,
        ChannelResolutionState::DisabledByConfig
    );
    assert!(setup.active_servers.is_empty());
}

#[test]
fn allowlist_blocks_unlisted_entries() {
    let policy = ChannelsPolicy {
        enabled: true,
        allowed: Some(vec!["server:slack".to_string()]),
    };
    let setup = resolve_channels(
        &["server:discord", "server:slack"],
        &policy,
        &servers(&["discord", "slack"]),
        &BTreeMap::new(),
    );
    assert_eq!(
        setup.resolutions,
        vec![
            ChannelResolution {
                entry: "server:discord".to_string(),
                state: ChannelResolutionState::NotAllowed,
            },
            ChannelResolution {
                entry: "server:slack".to_string(),
                state: ChannelResolutionState::Active {
                    servers: vec!["slack".to_string()],
                },
            },
        ]
    );
    assert_eq!(setup.active_servers, servers(&["slack"]));
}

#[test]
fn invalid_entries_are_reported() {
    let setup = resolve_channels(
        &["nonsense"],
        &ChannelsPolicy::default(),
        &servers(&["discord"]),
        &BTreeMap::new(),
    );
    let ChannelResolutionState::InvalidSpec { error } = &setup.resolutions[0].state else {
        panic!("expected invalid spec, got {:?}", setup.resolutions[0]);
    };
    assert!(error.contains("nonsense"));
    assert!(setup.active_servers.is_empty());
}

#[test]
fn unmatched_server_entries_are_reported() {
    let setup = resolve_channels(
        &["server:missing"],
        &ChannelsPolicy::default(),
        &servers(&["discord"]),
        &BTreeMap::new(),
    );
    assert_eq!(
        setup.resolutions[0].state,
        ChannelResolutionState::NoMatchingServer
    );
}

#[test]
fn plugin_entries_match_contributed_servers() {
    let plugin_ids = BTreeMap::from([
        ("acme-chat".to_string(), "acme".to_string()),
        ("acme-ci".to_string(), "acme".to_string()),
        ("other".to_string(), "other-plugin".to_string()),
    ]);
    let setup = resolve_channels(
        &["plugin:acme"],
        &ChannelsPolicy::default(),
        &servers(&["acme-chat", "acme-ci", "other"]),
        &plugin_ids,
    );
    assert_eq!(
        setup.resolutions[0].state,
        ChannelResolutionState::Active {
            servers: vec!["acme-chat".to_string(), "acme-ci".to_string()],
        }
    );
    assert_eq!(setup.active_servers, servers(&["acme-chat", "acme-ci"]));
}

#[test]
fn comma_separated_and_duplicate_entries_are_normalized() {
    let setup = resolve_channels(
        &["server:discord,server:github", "server:discord"],
        &ChannelsPolicy::default(),
        &servers(&["discord", "github"]),
        &BTreeMap::new(),
    );
    assert_eq!(setup.resolutions.len(), 2);
    assert_eq!(setup.active_servers, servers(&["discord", "github"]));
}
