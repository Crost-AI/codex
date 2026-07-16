use super::ChannelsPolicy;
use super::resolve_channels_enabled;
use crate::spec::ChannelSpec;

#[test]
fn enabled_defaults_to_true() {
    assert!(resolve_channels_enabled(None, None, None));
}

#[test]
fn user_config_wins_over_default() {
    assert!(!resolve_channels_enabled(None, None, Some(false)));
}

#[test]
fn env_var_wins_over_user_config() {
    assert!(!resolve_channels_enabled(None, Some("false"), Some(true)));
    assert!(resolve_channels_enabled(None, Some("1"), Some(false)));
}

#[test]
fn managed_config_wins_over_env_var_and_user_config() {
    assert!(!resolve_channels_enabled(
        Some(false),
        Some("true"),
        Some(true)
    ));
    assert!(resolve_channels_enabled(
        Some(true),
        Some("false"),
        Some(false)
    ));
}

#[test]
fn unparseable_env_value_is_ignored() {
    assert!(!resolve_channels_enabled(None, Some("maybe"), Some(false)));
    assert!(resolve_channels_enabled(None, Some(""), None));
}

#[test]
fn env_value_parsing_accepts_common_spellings() {
    for on in ["1", "true", "TRUE", "yes", "On"] {
        assert!(resolve_channels_enabled(None, Some(on), Some(false)));
    }
    for off in ["0", "false", "No", "OFF"] {
        assert!(!resolve_channels_enabled(None, Some(off), Some(true)));
    }
}

#[test]
fn missing_allowlist_allows_everything() {
    let policy = ChannelsPolicy::default();
    assert!(policy.entry_allowed(&ChannelSpec::Server("discord".to_string())));
}

#[test]
fn empty_allowlist_allows_nothing() {
    let policy = ChannelsPolicy {
        enabled: true,
        allowed: Some(Vec::new()),
    };
    assert!(!policy.entry_allowed(&ChannelSpec::Server("discord".to_string())));
}

#[test]
fn allowlist_matches_canonical_and_unnormalized_entries() {
    let policy = ChannelsPolicy {
        enabled: true,
        allowed: Some(vec![
            "server:discord".to_string(),
            " server: slack ".to_string(),
        ]),
    };
    assert!(policy.entry_allowed(&ChannelSpec::Server("discord".to_string())));
    assert!(policy.entry_allowed(&ChannelSpec::Server("slack".to_string())));
    assert!(!policy.entry_allowed(&ChannelSpec::Server("github".to_string())));
    assert!(!policy.entry_allowed(&ChannelSpec::Plugin("discord".to_string())));
}
