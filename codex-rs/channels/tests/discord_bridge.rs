//! Host-side integration test for the bundled Discord channel bridge.
//!
//! Spawns the real `discord-channel.mjs` (no token, so it serves MCP without
//! a gateway connection) through the host's real MCP client code path and
//! asserts the pieces the bridge's own e2e test cannot: that the host parses
//! the `codex/channel` capability from the initialize result and sees the
//! bridge's tools. When a capability or routing bug appears, this test
//! bisects host-vs-bridge.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use codex_rmcp_client::LocalStdioServerLauncher;
use codex_rmcp_client::RmcpClient;
use futures::FutureExt as _;
use rmcp::model::ClientCapabilities;
use rmcp::model::Implementation;
use rmcp::model::InitializeRequestParams;
use rmcp::model::ProtocolVersion;

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn discord_bridge_declares_channel_capability_and_tools() -> anyhow::Result<()> {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return Ok(());
    }
    let bridge = codex_utils_cargo_bin::find_resource!("examples/discord/discord-channel.mjs")?;

    let client = RmcpClient::new_stdio_client(
        OsString::from("node"),
        vec![bridge.into_os_string()],
        /*env*/ None,
        &[],
        /*cwd*/ None,
        Arc::new(LocalStdioServerLauncher::new(std::env::current_dir()?)),
    )
    .await?;

    let params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("codex-channels-test", "0.0.0-test").with_title("Codex channels test"),
    )
    .with_protocol_version(ProtocolVersion::V_2025_06_18);
    let initialize_result = client
        .initialize(
            params,
            Some(Duration::from_secs(20)),
            Box::new(|_, _| async { anyhow::bail!("bridge should not elicit") }.boxed()),
            /*custom_notification_handler*/ None,
        )
        .await?;

    // The host gates channel registration on this experimental capability.
    let experimental = initialize_result
        .capabilities
        .experimental
        .as_ref()
        .expect("bridge should declare experimental capabilities");
    assert!(
        experimental.contains_key(codex_channels::CHANNEL_CAPABILITY),
        "bridge must declare the {} capability; got {experimental:?}",
        codex_channels::CHANNEL_CAPABILITY,
    );
    // The capability value carries the host slash-command reply descriptor.
    let descriptor = experimental
        .get(codex_channels::CHANNEL_CAPABILITY)
        .and_then(codex_channels::parse_channel_commands_descriptor)
        .expect("bridge should declare the commands reply-routing descriptor");
    assert_eq!(descriptor.reply_tool, "send_message");
    assert_eq!(descriptor.target_meta, "channel_id");
    assert_eq!(descriptor.target_arg, "channel_id");
    assert_eq!(descriptor.content_arg, "content");
    assert!(
        initialize_result
            .instructions
            .as_deref()
            .is_some_and(|instructions| instructions.contains("send_message")),
        "bridge instructions should reference the send_message reply tool",
    );

    let tools = client
        .list_tools_with_connector_ids(/*params*/ None, Some(Duration::from_secs(20)))
        .await?;
    let tool_names: BTreeSet<String> = tools
        .tools
        .iter()
        .map(|tool| tool.tool.name.to_string())
        .collect();
    assert_eq!(
        tool_names,
        BTreeSet::from([
            "add_reaction".to_string(),
            "create_poll".to_string(),
            "create_thread".to_string(),
            "end_poll".to_string(),
            "read_attachment".to_string(),
            "read_messages".to_string(),
            "read_poll".to_string(),
            "send_file".to_string(),
            "send_message".to_string(),
        ])
    );

    client.shutdown().await;
    Ok(())
}
