use std::collections::BTreeSet;
use std::collections::HashMap;

use codex_channels::ChannelEvent;
use codex_channels::ChannelSetup;
use codex_config::McpServerConfig;
use codex_config::McpServerTransportConfig;
use codex_mcp::EffectiveMcpServer;
use pretty_assertions::assert_eq;

use super::ChannelHub;
use codex_channels::CHANNEL_EVENT_MARKER;
use codex_channels::CHANNEL_EVENT_PREAMBLE;
use codex_channels::CHANNEL_EVENT_SEPARATOR;
use super::apply_channel_env_overlay;

fn stdio_server(env: Option<HashMap<String, String>>) -> EffectiveMcpServer {
    let config: McpServerConfig = serde_json::from_value(serde_json::json!({
        "command": "node",
        "args": ["server.mjs"],
    }))
    .expect("valid MCP server config");
    let mut config = config;
    if let McpServerTransportConfig::Stdio {
        env: config_env, ..
    } = &mut config.transport
    {
        *config_env = env;
    }
    EffectiveMcpServer::configured(config)
}

fn server_env(server: &EffectiveMcpServer) -> Option<HashMap<String, String>> {
    let config = server.config();
    match &config.transport {
        McpServerTransportConfig::Stdio { env, .. } => env.clone(),
        McpServerTransportConfig::StreamableHttp { .. } => panic!("expected stdio transport"),
    }
}

#[tokio::test]
async fn hub_drops_events_from_non_active_servers() {
    let hub = ChannelHub::new();
    hub.set_setup_for_tests(ChannelSetup {
        resolutions: Vec::new(),
        active_servers: BTreeSet::from(["discord".to_string()]),
    });

    hub.on_channel_event(
        "github".to_string(),
        ChannelEvent {
            content: "sneaky".to_string(),
            meta: Default::default(),
        },
    )
    .await;
    assert_eq!(hub.drain_events(), Vec::<String>::new());

    hub.on_channel_event(
        "discord".to_string(),
        ChannelEvent {
            content: "hello".to_string(),
            meta: Default::default(),
        },
    )
    .await;
    assert_eq!(
        hub.drain_events(),
        vec!["<channel source=\"discord\">\nhello\n</channel>".to_string()]
    );
}

#[test]
fn env_overlay_applies_only_to_active_stdio_servers() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let discord_dir = codex_home.path().join("channels").join("discord");
    std::fs::create_dir_all(&discord_dir).expect("create channel dir");
    std::fs::write(
        discord_dir.join(".env"),
        "DISCORD_BOT_TOKEN=from-dotenv\nEXTRA=1\n",
    )
    .expect("write .env");

    let mut servers = HashMap::from([
        ("discord".to_string(), stdio_server(None)),
        ("github".to_string(), stdio_server(None)),
    ]);
    apply_channel_env_overlay(
        &mut servers,
        &BTreeSet::from(["discord".to_string()]),
        codex_home.path(),
    );

    assert_eq!(
        server_env(&servers["discord"]),
        Some(HashMap::from([
            ("DISCORD_BOT_TOKEN".to_string(), "from-dotenv".to_string()),
            ("EXTRA".to_string(), "1".to_string()),
            ("CHANNEL_NAMESPACES".to_string(), "codex".to_string()),
            (
                "CHANNEL_ENV_FILE".to_string(),
                discord_dir.join(".env").to_string_lossy().into_owned()
            ),
        ]))
    );
    // Non-active servers never receive channel credentials.
    assert_eq!(server_env(&servers["github"]), None);
}

#[test]
fn env_overlay_lets_explicit_config_env_win() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let discord_dir = codex_home.path().join("channels").join("discord");
    std::fs::create_dir_all(&discord_dir).expect("create channel dir");
    std::fs::write(
        discord_dir.join(".env"),
        "DISCORD_BOT_TOKEN=from-dotenv\nONLY_IN_DOTENV=yes\n",
    )
    .expect("write .env");

    let mut servers = HashMap::from([(
        "discord".to_string(),
        stdio_server(Some(HashMap::from([(
            "DISCORD_BOT_TOKEN".to_string(),
            "from-config".to_string(),
        )]))),
    )]);
    apply_channel_env_overlay(
        &mut servers,
        &BTreeSet::from(["discord".to_string()]),
        codex_home.path(),
    );

    assert_eq!(
        server_env(&servers["discord"]),
        Some(HashMap::from([
            ("DISCORD_BOT_TOKEN".to_string(), "from-config".to_string()),
            ("ONLY_IN_DOTENV".to_string(), "yes".to_string()),
            ("CHANNEL_NAMESPACES".to_string(), "codex".to_string()),
            (
                "CHANNEL_ENV_FILE".to_string(),
                discord_dir.join(".env").to_string_lossy().into_owned()
            ),
        ]))
    );
}

#[test]
fn env_overlay_missing_dotenv_still_injects_host_defaults() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let mut servers = HashMap::from([("discord".to_string(), stdio_server(None))]);
    apply_channel_env_overlay(
        &mut servers,
        &BTreeSet::from(["discord".to_string()]),
        codex_home.path(),
    );
    assert_eq!(
        server_env(&servers["discord"]),
        Some(HashMap::from([
            ("CHANNEL_NAMESPACES".to_string(), "codex".to_string()),
            (
                "CHANNEL_ENV_FILE".to_string(),
                codex_home
                    .path()
                    .join("channels")
                    .join("discord")
                    .join(".env")
                    .to_string_lossy()
                    .into_owned()
            ),
        ]))
    );
}

#[test]
fn preamble_is_delivered_once_then_marker() {
    let hub = ChannelHub::new();
    let first = hub.render_batch(&["a".to_string(), "b".to_string()]);
    assert_eq!(
        first,
        format!("{CHANNEL_EVENT_PREAMBLE}\n\na{CHANNEL_EVENT_SEPARATOR}b")
    );
    let second = hub.render_batch(&["c".to_string()]);
    assert_eq!(second, format!("{CHANNEL_EVENT_MARKER}\n\nc"));
    assert!(!second.contains("arrived over external channels"));
}
