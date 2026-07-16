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

mod dotenv;
mod envelope;
mod event;
mod policy;
mod queue;
mod resolve;
mod spec;

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
/// result to register as a channel. The value is a reserved empty object.
pub const CHANNEL_CAPABILITY: &str = "codex/channel";

/// JSON-RPC notification method channels use to push events to the host.
pub const CHANNEL_NOTIFICATION_METHOD: &str = "notifications/codex/channel";

/// Environment variable that overrides the user-level `[channels] enabled`
/// config while still being overridden by managed config.
pub const CHANNELS_ENABLED_ENV_VAR: &str = "CODEX_CHANNELS_ENABLED";

/// Separator inserted between channel events that are delivered together in a
/// single turn.
pub const CHANNEL_EVENT_SEPARATOR: &str = "\n---\n";
