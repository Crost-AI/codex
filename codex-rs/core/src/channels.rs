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
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;

use codex_channels::BoundedEventQueue;
use codex_channels::CHANNEL_EVENT_MARKER;
use codex_channels::CHANNEL_EVENT_PREAMBLE;
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
use crate::session::session::SessionSettingsUpdate;
use crate::session::turn_context::NewTurnContextOptions;
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
    /// Set once the full preamble has been delivered in this session; later
    /// batches carry only [`CHANNEL_EVENT_MARKER`].
    preamble_sent: AtomicBool,
}

impl ChannelHub {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            policy: Mutex::new(codex_channels::ChannelsPolicy::default()),
            setup: Mutex::new(ChannelSetup::default()),
            queue: Mutex::new(BoundedEventQueue::default()),
            session: OnceLock::new(),
            preamble_sent: AtomicBool::new(false),
        }
    }

    /// Records the session's requested entries and effective policy so MCP
    /// runtime rebuilds can re-resolve against a fresh server set.
    pub(crate) fn configure(&self, config: &Config) {
        *self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config.channels_entries.clone();
        *self
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config.channels_policy.clone();
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let policy = self
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let setup =
            resolve_session_channels(&entries, &policy, tool_plugin_provenance, mcp_servers);
        *self
            .setup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = setup.clone();
        setup
    }

    #[cfg(test)]
    fn set_setup_for_tests(&self, setup: ChannelSetup) {
        *self
            .setup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = setup;
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_server_active(&server_name)
        {
            debug!("dropping channel event from non-active MCP server `{server_name}`");
            return;
        }
        // Host-executed slash commands (`/status`, `/channels`, `/help`)
        // short-circuit injection: the host answers through the channel
        // itself and the model never sees the event.
        if self.try_handle_command(&server_name, &event).await {
            return;
        }
        let rendered = render_channel_event(&server_name, &event.content, &event.meta);
        {
            let mut queue = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    /// Tries to execute an inbound event as a host-side slash command.
    /// Returns `true` when the event was consumed — command recognized,
    /// executed, and answered through the originating server's declared
    /// reply tool — so it must NOT inject into the model. Returns `false`
    /// for everything else (not a command, bot-authored, server declared no
    /// `commands` descriptor, unknown command name, or no reply target on
    /// the event); those events flow to the model unchanged.
    async fn try_handle_command(&self, server_name: &str, event: &ChannelEvent) -> bool {
        let Some(command) = codex_channels::parse_channel_command(&event.content) else {
            return false;
        };
        // Commands come from humans on the bridge's sender allowlist. A
        // bot-authored `/status` is not a host command — it injects like
        // any other message, so the agent can decide what to do.
        if event.meta.contains_key("bot") {
            return false;
        }
        let Some(session) = self.session.get().and_then(Weak::upgrade) else {
            return false;
        };
        let mcp_runtime = Arc::clone(&session.services.mcp_runtime);
        // Opt-in: only servers that told the host how to route replies get
        // command handling at all.
        let Some(descriptor) = mcp_runtime
            .latest_channel_commands_descriptor(server_name)
            .await
        else {
            return false;
        };
        let output = match command.as_str() {
            "help" => codex_channels::channel_command_help(),
            "status" | "session" => session.channel_status_text().await,
            "channels" => {
                let setup = self
                    .setup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                codex_channels::channel_setup_status_text(&setup)
            }
            _ => return false,
        };
        let Some(target) = event.meta.get(&descriptor.target_meta).cloned() else {
            warn!(
                "channel command `/{command}` from `{server_name}` has no `{}` meta to reply \
                 to; injecting as a normal event",
                descriptor.target_meta
            );
            return false;
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert(descriptor.target_arg.clone(), target.into());
        arguments.insert(descriptor.content_arg.clone(), output.into());
        for (arg, meta_key) in &descriptor.extra_args {
            if let Some(value) = event.meta.get(meta_key) {
                arguments.insert(arg.clone(), value.clone().into());
            }
        }
        debug!("executing channel slash command `/{command}` from `{server_name}` host-side");
        if let Err(error) = mcp_runtime
            .latest_call_tool(
                server_name,
                &descriptor.reply_tool,
                /*environment_id*/ None,
                Some(serde_json::Value::Object(arguments)),
                /*meta*/ None,
                /*requested_timeout*/ None,
                /*wait_for_server*/ false,
            )
            .await
        {
            warn!("channel command `/{command}` reply via `{server_name}` failed: {error:#}");
        }
        // Consumed either way: a command whose reply failed must not fall
        // through to the model as if it were conversation.
        true
    }

    fn has_events(&self) -> bool {
        !self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    fn drain_events(&self) -> Vec<String> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain_all()
    }

    /// Renders a batch of drained events as turn input. The first batch of a
    /// session carries the full [`CHANNEL_EVENT_PREAMBLE`]; every later batch
    /// carries the one-line [`CHANNEL_EVENT_MARKER`] instead, so standing
    /// instructions are not repeated on every delivery.
    fn render_batch(&self, events: &[String]) -> String {
        let header = if self.preamble_sent.swap(true, Ordering::AcqRel) {
            CHANNEL_EVENT_MARKER
        } else {
            CHANNEL_EVENT_PREAMBLE
        };
        format!("{header}\n\n{}", events.join(CHANNEL_EVENT_SEPARATOR))
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

        // A running Regular turn takes the queued events immediately — the
        // same steer path as input typed mid-turn — so a channel message
        // reaches the agent while it is working instead of waiting for the
        // turn to end. Review/compact turns are not steerable; their events
        // stay queued for turn-end delivery below.
        if self.try_steer_channel_events().await {
            return;
        }

        {
            let mut active_turn = self.active_turn.lock().await;
            if active_turn.is_some() {
                return;
            }
            *active_turn = Some(ActiveTurn::default());
        }

        let Ok((turn_context, _)) = self
            .new_turn_with_sub_id(
                uuid::Uuid::new_v4().to_string(),
                SessionSettingsUpdate::default(),
                NewTurnContextOptions::default(),
            )
            .await
        else {
            self.clear_reserved_idle_turn_for_channels().await;
            return;
        };
        if self.collaboration_mode().await.mode == ModeKind::Plan {
            self.clear_reserved_idle_turn_for_channels().await;
            return;
        }
        let events = hub.drain_events();
        if events.is_empty() {
            self.clear_reserved_idle_turn_for_channels().await;
            return;
        }
        let text = hub.render_batch(&events);
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

    /// Injects queued channel events into the currently running turn via the
    /// steer path (the one input typed mid-turn uses). Returns `true` when
    /// the events were handed to the active task — or the queue was already
    /// empty — and `false` when there is no steerable Regular task, in which
    /// case the caller falls back to idle/turn-end delivery. Drain and
    /// inject happen under the active-turn lock so a turn ending mid-call
    /// cannot strand drained events.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    async fn try_steer_channel_events(&self) -> bool {
        let mut active = self.active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return false;
        };
        let Some(active_task) = active_turn.task.as_ref() else {
            // Reserved-but-taskless window (a delivery turn is being set
            // up); leave events queued rather than racing it.
            return false;
        };
        if !matches!(active_task.kind, crate::state::TaskKind::Regular) {
            return false;
        }
        let events = self.services.channel_hub.drain_events();
        if events.is_empty() {
            return true;
        }
        let text = self.services.channel_hub.render_batch(&events);
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                active_turn.turn_state.as_ref(),
                vec![TurnInput::UserInput {
                    content: vec![UserInput::Text {
                        text,
                        text_elements: Vec::new(),
                    }],
                    client_id: None,
                }],
            )
            .await;
        true
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
        let config = server.config();
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
        let mut merged = std::fs::read_to_string(&env_path)
            .map(|contents| codex_channels::parse_dotenv(&contents))
            .unwrap_or_default();
        let config_had_namespaces = env
            .as_ref()
            .is_some_and(|variables| variables.contains_key("CHANNEL_NAMESPACES"));
        let config_had_env_file = env
            .as_ref()
            .is_some_and(|variables| variables.contains_key("CHANNEL_ENV_FILE"));
        if let Some(env) = env {
            merged.extend(env.clone());
        }
        if !config_had_namespaces {
            merged.insert("CHANNEL_NAMESPACES".to_string(), "codex".to_string());
        }
        if !config_had_env_file {
            merged.insert(
                "CHANNEL_ENV_FILE".to_string(),
                env_path.to_string_lossy().into_owned(),
            );
        }
        if merged.is_empty() {
            continue;
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
