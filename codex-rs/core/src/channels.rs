//! Session wiring for channels: MCP servers that push events into a running
//! session via the `notifications/codex/channel` notification.
//!
//! The pure building blocks (entry parsing, policy, envelope rendering, the
//! bounded queue, `.env` parsing) live in the `codex-channels` crate. This
//! module owns the session-side state: which servers are active, the pending
//! event queue, and delivery into the agent loop (wake the session when idle,
//! or hold events until the running turn ends).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;

use codex_channels::BoundedEventQueue;
use codex_channels::CHANNEL_EVENT_SEPARATOR;
use codex_channels::ChannelEvent;
use codex_channels::ChannelSetup;
use codex_channels::render_channel_event;
use codex_channels::resolve_channels;
use codex_config::McpServerTransportConfig;
use codex_mcp::ChannelWiring;
use codex_mcp::EffectiveMcpServer;
use codex_mcp::ToolPluginProvenance;
use codex_protocol::config_types::ModeKind;
use codex_protocol::user_input::UserInput;
use futures::FutureExt;
use tracing::debug;
use tracing::warn;

use crate::config::Config;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::state::ActiveTurn;
use crate::tasks::RegularTask;

/// Session-scoped channel state: the requested entries and policy, the
/// resolved opt-ins, and events waiting for delivery.
pub(crate) struct ChannelHub {
    entries: Mutex<Vec<String>>,
    policy: Mutex<codex_channels::ChannelsPolicy>,
    setup: Mutex<ChannelSetup>,
    queue: Mutex<BoundedEventQueue>,
    session: OnceLock<Weak<Session>>,
}

impl ChannelHub {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            policy: Mutex::new(codex_channels::ChannelsPolicy::default()),
            setup: Mutex::new(ChannelSetup::default()),
            queue: Mutex::new(BoundedEventQueue::default()),
            session: OnceLock::new(),
        }
    }

    /// Records the session's requested entries and effective policy so MCP
    /// runtime rebuilds can re-resolve against a fresh server set.
    pub(crate) fn configure(&self, config: &Config) {
        *self.entries.lock().expect("channel entries lock poisoned") =
            config.channels_entries.clone();
        *self.policy.lock().expect("channel policy lock poisoned") =
            config.channels_policy.clone();
    }

    /// Re-resolves the requested entries against the given MCP servers and
    /// publishes the result as the session's active channel setup.
    pub(crate) fn refresh_setup(
        &self,
        tool_plugin_provenance: &ToolPluginProvenance,
        mcp_servers: &HashMap<String, EffectiveMcpServer>,
    ) -> ChannelSetup {
        let entries = self
            .entries
            .lock()
            .expect("channel entries lock poisoned")
            .clone();
        let policy = self
            .policy
            .lock()
            .expect("channel policy lock poisoned")
            .clone();
        let setup =
            resolve_session_channels(&entries, &policy, tool_plugin_provenance, mcp_servers);
        *self.setup.lock().expect("channel setup lock poisoned") = setup.clone();
        setup
    }

    pub(crate) fn setup(&self) -> ChannelSetup {
        self.setup
            .lock()
            .expect("channel setup lock poisoned")
            .clone()
    }

    #[cfg(test)]
    fn set_setup_for_tests(&self, setup: ChannelSetup) {
        *self.setup.lock().expect("channel setup lock poisoned") = setup;
    }

    /// Lets queued events wake the session. Events arriving before this is
    /// called are held until the first turn-end delivery pass.
    pub(crate) fn attach_session(&self, session: &Arc<Session>) {
        let _ = self.session.set(Arc::downgrade(session));
    }

    /// Entry point for events pushed by channel servers. Gates on the active
    /// set (defense in depth: non-active servers get no listener at all),
    /// renders the envelope, queues it, and wakes the session when idle.
    pub(crate) async fn on_channel_event(&self, server_name: String, event: ChannelEvent) {
        if !self
            .setup
            .lock()
            .expect("channel setup lock poisoned")
            .is_server_active(&server_name)
        {
            debug!("dropping channel event from non-active MCP server `{server_name}`");
            return;
        }
        let rendered = render_channel_event(&server_name, &event.content, &event.meta);
        {
            let mut queue = self.queue.lock().expect("channel queue lock poisoned");
            if queue.push(rendered).is_some() {
                warn!(
                    "channel event queue is full; dropped the oldest pending event \
                     (server `{server_name}`)"
                );
            }
        }
        if let Some(session) = self.session.get().and_then(Weak::upgrade) {
            session.maybe_start_turn_for_channel_events().await;
        }
    }

    fn has_events(&self) -> bool {
        !self
            .queue
            .lock()
            .expect("channel queue lock poisoned")
            .is_empty()
    }

    fn drain_events(&self) -> Vec<String> {
        self.queue
            .lock()
            .expect("channel queue lock poisoned")
            .drain_all()
    }

    fn requeue_front(&self, events: Vec<String>) {
        let mut queue = self.queue.lock().expect("channel queue lock poisoned");
        let pending = queue.drain_all();
        for event in events.into_iter().chain(pending) {
            queue.push(event);
        }
    }
}

impl Session {
    /// Delivers pending channel events by starting a new turn when the
    /// session is idle. Invoked when an event arrives and at every turn end,
    /// so events queued during a running turn are delivered together (joined
    /// with `---`) as soon as the turn finishes.
    ///
    /// Boxed because the started task's teardown re-enters this function at
    /// turn end, which would otherwise recurse the future type.
    pub(crate) fn maybe_start_turn_for_channel_events(
        self: &Arc<Self>,
    ) -> futures::future::BoxFuture<'static, ()> {
        let session = Arc::clone(self);
        Box::pin(async move {
            session.maybe_start_turn_for_channel_events_inner().await;
        })
    }

    async fn maybe_start_turn_for_channel_events_inner(self: &Arc<Self>) {
        let hub = &self.services.channel_hub;
        if !hub.has_events() {
            return;
        }
        // Queued user/client work and Plan mode take priority; events stay
        // queued and the next turn end retries delivery.
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            return;
        }
        if self.collaboration_mode().await.mode == ModeKind::Plan {
            return;
        }

        {
            let mut active_turn = self.active_turn.lock().await;
            if active_turn.is_some() {
                return;
            }
            *active_turn = Some(ActiveTurn::default());
        }

        let turn_context = self
            .new_default_turn_with_sub_id(uuid::Uuid::new_v4().to_string())
            .await;
        if turn_context.mode == ModeKind::Plan {
            self.clear_reserved_idle_turn_for_channels().await;
            return;
        }
        let events = hub.drain_events();
        if events.is_empty() {
            self.clear_reserved_idle_turn_for_channels().await;
            return;
        }
        let text = events.join(CHANNEL_EVENT_SEPARATOR);
        let input = vec![TurnInput::UserInput {
            content: vec![UserInput::Text {
                text,
                text_elements: Vec::new(),
            }],
            client_id: None,
        }];
        self.maybe_emit_model_warnings_for_turn(turn_context.as_ref())
            .await;
        self.start_task(turn_context, input, RegularTask::new())
            .await;
    }

    async fn clear_reserved_idle_turn_for_channels(&self) {
        let mut active_turn = self.active_turn.lock().await;
        if let Some(reserved) = active_turn.as_ref()
            && reserved.task.is_none()
        {
            *active_turn = None;
        }
    }
}

/// Resolves channel opt-ins against the effective MCP servers.
fn resolve_session_channels(
    entries: &[String],
    policy: &codex_channels::ChannelsPolicy,
    tool_plugin_provenance: &ToolPluginProvenance,
    mcp_servers: &HashMap<String, EffectiveMcpServer>,
) -> ChannelSetup {
    let configured_servers: BTreeSet<String> = mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled())
        .map(|(name, _)| name.clone())
        .collect();
    let plugin_ids_by_server: BTreeMap<String, String> = configured_servers
        .iter()
        .filter_map(|name| {
            tool_plugin_provenance
                .plugin_id_for_mcp_server_name(name)
                .map(|plugin_id| (name.clone(), plugin_id.to_string()))
        })
        .collect();
    resolve_channels(entries, policy, &configured_servers, &plugin_ids_by_server)
}

/// Builds the MCP-layer wiring that forwards `notifications/codex/channel`
/// events from active servers into the hub.
pub(crate) fn channel_wiring_for_hub(
    hub: &Arc<ChannelHub>,
    setup: &ChannelSetup,
) -> Option<Arc<ChannelWiring>> {
    if setup.active_servers.is_empty() {
        return None;
    }
    let hub = Arc::clone(hub);
    let sink: codex_mcp::ChannelEventSink = Arc::new(move |server_name, event| {
        let hub = Arc::clone(&hub);
        async move {
            hub.on_channel_event(server_name, event).await;
        }
        .boxed()
    });
    Some(Arc::new(ChannelWiring::new(
        setup.active_servers.iter().cloned().collect(),
        sink,
    )))
}

/// Injects `~/.codex/channels/<server>/.env` credentials into the spawn
/// environment of servers opted in as channels this session. Explicit `env`
/// entries from the MCP server config win over `.env` values.
pub(crate) fn apply_channel_env_overlay(
    mcp_servers: &mut HashMap<String, EffectiveMcpServer>,
    active_servers: &BTreeSet<String>,
    codex_home: &Path,
) {
    for server_name in active_servers {
        let Some(server) = mcp_servers.get(server_name) else {
            continue;
        };
        let Some(config) = server.configured_config() else {
            continue;
        };
        let McpServerTransportConfig::Stdio {
            command,
            args,
            env,
            env_vars,
            cwd,
        } = &config.transport
        else {
            continue;
        };
        let env_path = codex_home.join("channels").join(server_name).join(".env");
        let Ok(contents) = std::fs::read_to_string(&env_path) else {
            continue;
        };
        let mut merged = codex_channels::parse_dotenv(&contents);
        if merged.is_empty() {
            continue;
        }
        if let Some(env) = env {
            merged.extend(env.clone());
        }
        let mut overlaid = config.clone();
        overlaid.transport = McpServerTransportConfig::Stdio {
            command: command.clone(),
            args: args.clone(),
            env: Some(merged),
            env_vars: env_vars.clone(),
            cwd: cwd.clone(),
        };
        mcp_servers.insert(
            server_name.clone(),
            EffectiveMcpServer::configured(overlaid),
        );
    }
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
