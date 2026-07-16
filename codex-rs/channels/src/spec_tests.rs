use pretty_assertions::assert_eq;

use super::ChannelSpec;
use super::ChannelSpecError;
use super::split_channel_entries;

#[test]
fn parses_server_entries() {
    assert_eq!(
        ChannelSpec::parse("server:discord"),
        Ok(ChannelSpec::Server("discord".to_string()))
    );
    assert_eq!(
        ChannelSpec::parse("  server: my-server_2  "),
        Ok(ChannelSpec::Server("my-server_2".to_string()))
    );
}

#[test]
fn parses_plugin_entries() {
    assert_eq!(
        ChannelSpec::parse("plugin:acme.chatops"),
        Ok(ChannelSpec::Plugin("acme.chatops".to_string()))
    );
}

#[test]
fn rejects_malformed_entries() {
    assert_eq!(ChannelSpec::parse(""), Err(ChannelSpecError::Empty));
    assert_eq!(ChannelSpec::parse("   "), Err(ChannelSpecError::Empty));
    assert_eq!(
        ChannelSpec::parse("discord"),
        Err(ChannelSpecError::MissingKind {
            entry: "discord".to_string(),
        })
    );
    assert_eq!(
        ChannelSpec::parse("webhook:foo"),
        Err(ChannelSpecError::UnknownKind {
            kind: "webhook".to_string(),
        })
    );
    assert_eq!(
        ChannelSpec::parse("server:"),
        Err(ChannelSpecError::InvalidName {
            kind: "server",
            name: String::new(),
        })
    );
    assert_eq!(
        ChannelSpec::parse("server:bad name!"),
        Err(ChannelSpecError::InvalidName {
            kind: "server",
            name: "bad name!".to_string(),
        })
    );
}

#[test]
fn canonical_round_trips() {
    let spec = ChannelSpec::parse("server:discord").unwrap();
    assert_eq!(spec.canonical(), "server:discord");
    let spec = ChannelSpec::parse(" plugin:acme ").unwrap();
    assert_eq!(spec.canonical(), "plugin:acme");
}

#[test]
fn splits_comma_separated_entries() {
    assert_eq!(
        split_channel_entries(&["server:a,server:b", " server:c ", "", " , "]),
        vec![
            "server:a".to_string(),
            "server:b".to_string(),
            "server:c".to_string(),
        ]
    );
}
