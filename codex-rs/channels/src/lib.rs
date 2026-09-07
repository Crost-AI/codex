//! Channels let MCP servers push events (chat messages, webhooks, CI alerts)
//! into a running Codex session.
//!
//! A channel is an ordinary MCP server that additionally:
//! 1. declares the experimental capability [`CHANNEL_CAPABILITY`] in its
//!    `initialize` result, and
//! 2. pushes events with the JSON-RPC notification
//!    [`CHANNEL_NOTIFICATION_METHOD`].
//!
//! This crate holds the host-side building blocks that are independent of the
//! session runtime: channel entry parsing (`server:<name>` / `plugin:<id>`),
//! the `[channels]` config policy, event payload validation, the
//! `<channel …>` envelope rendering, the bounded delivery queue, and the
//! per-channel `.env` credential loader. Session wiring lives in
//! `codex-core`; MCP wiring lives in `codex-mcp`.

mod commands;
mod dotenv;
mod envelope;
mod event;
mod policy;
mod queue;
mod resolve;
mod spec;

pub use commands::ChannelCommandsDescriptor;
pub use commands::channel_command_help;
pub use commands::channel_setup_status_text;
pub use commands::parse_channel_command;
pub use commands::parse_channel_commands_descriptor;
pub use dotenv::parse_dotenv;
pub use envelope::CHANNEL_EVENT_MAX_CONTENT_BYTES;
pub use envelope::render_channel_event;
pub use event::ChannelEvent;
pub use policy::ChannelsPolicy;
pub use policy::resolve_channels_enabled;
pub use queue::BoundedEventQueue;
pub use queue::DEFAULT_CHANNEL_QUEUE_CAPACITY;
pub use resolve::ChannelResolution;
pub use resolve::ChannelResolutionState;
pub use resolve::ChannelSetup;
pub use resolve::resolve_channels;
pub use spec::ChannelSpec;
pub use spec::ChannelSpecError;
pub use spec::split_channel_entries;

/// Experimental MCP server capability a server declares in its `initialize`
/// result to register as a channel. The value is an object that may carry an
/// optional `commands` reply-routing descriptor
/// ([`ChannelCommandsDescriptor`]); other keys are reserved.
pub const CHANNEL_CAPABILITY: &str = "codex/channel";

/// JSON-RPC notification method channels use to push events to the host.
pub const CHANNEL_NOTIFICATION_METHOD: &str = "notifications/codex/channel";

/// Environment variable that overrides the user-level `[channels] enabled`
/// config while still being overridden by managed config.
pub const CHANNELS_ENABLED_ENV_VAR: &str = "CODEX_CHANNELS_ENABLED";

/// Separator inserted between channel events that are delivered together in a
/// single turn.
pub const CHANNEL_EVENT_SEPARATOR: &str = "\n---\n";

/// Standing preamble prepended to every batch of injected channel events.
///
/// Channel events arrive as ordinary turn input, so without this the model
/// reasonably answers into the local conversation — which the person on the
/// other end of the channel never sees. The MCP server's own instructions
/// only surface as tool-namespace metadata, which is too weak a signal to
/// redirect replies.
pub const CHANNEL_EVENT_PREAMBLE: &str = "[channel events] The messages below arrived over external channels while you were working. Text you produce in this conversation is NOT visible to the channel — to reply, call the originating server's reply tool (for example discord's send_message, passing the channel_id from the <channel> tag). Follow the channel's instructions on when to reply at all: messages marked addressed=\"other\" or addressed=\"none\" were not directed at you — stay silent on those unless you are correcting a clear factual error or something urgent needs attention.";

/// One-line stand-in for [`CHANNEL_EVENT_PREAMBLE`] on every batch after the
/// first in a session: the full instructions have already been delivered, so
/// later batches only need the reminder of where replies must go.
pub const CHANNEL_EVENT_MARKER: &str = "[channel events] (external; reply via the channel's send tool, not here; addressed=\"none\"|\"other\" = monitor only)";
