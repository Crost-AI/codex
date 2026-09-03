use std::collections::HashSet;
use std::sync::Arc;

use codex_channels::ChannelEvent;
use codex_rmcp_client::CustomNotificationHandler;
use futures::FutureExt;
use futures::future::BoxFuture;
use rmcp::model::CustomNotification;
use tracing::debug;

/// Receives validated channel events from opted-in servers, keyed by the real
/// MCP server name.
pub type ChannelEventSink =
    Arc<dyn Fn(String, ChannelEvent) -> BoxFuture<'static, ()> + Send + Sync>;

/// Session wiring for channel event delivery.
///
/// Only servers in `active_servers` get a notification listener at all;
/// everything else keeps the default behavior of dropping custom
/// notifications. Delivery-side gating happens again in the sink's owner as
/// defense in depth.
pub struct ChannelWiring {
    active_servers: HashSet<String>,
    sink: ChannelEventSink,
}

impl ChannelWiring {
    pub fn new(active_servers: HashSet<String>, sink: ChannelEventSink) -> Self {
        Self {
            active_servers,
            sink,
        }
    }

    pub(crate) fn listens_for(&self, server_name: &str) -> bool {
        self.active_servers.contains(server_name)
    }

    /// Builds the custom-notification handler for one server, or `None` when
    /// the server is not opted in as a channel this session.
    pub(crate) fn handler_for(&self, server_name: &str) -> Option<CustomNotificationHandler> {
        if !self.active_servers.contains(server_name) {
            return None;
        }
        let sink = Arc::clone(&self.sink);
        let server_name = server_name.to_string();
        Some(Arc::new(move |notification: CustomNotification| {
            let sink = Arc::clone(&sink);
            let server_name = server_name.clone();
            async move {
                // Delivery (including `/status` tool replies) must not run on
                // the RMCP service task: that task has to keep reading the
                // JSON-RPC response to `send_message`. Spawn and return.
                tokio::spawn(async move {
                    if notification.method != codex_channels::CHANNEL_NOTIFICATION_METHOD {
                        debug!(
                            "ignoring unknown custom notification `{}` from MCP server `{server_name}`",
                            notification.method
                        );
                        return;
                    }
                    // Malformed events are dropped silently per the channel protocol.
                    let Some(event) =
                        ChannelEvent::parse_notification_params(notification.params.as_ref())
                    else {
                        debug!(
                            "dropping malformed channel event from MCP server `{server_name}`"
                        );
                        return;
                    };
                    sink(server_name, event).await;
                });
            }
            .boxed()
        }))
    }
}
