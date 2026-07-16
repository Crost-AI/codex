# Channels

Channels let an MCP server **push events into a running Codex session** — chat
messages, webhooks, CI alerts — instead of waiting for the agent to call a
tool. Two-way channels (like the bundled
[Discord channel](../codex-rs/channels/examples/discord/README.md)) also
expose ordinary MCP tools for replies; nothing about the reply path is
channel-specific.

## Usage

1. Configure the channel server as a normal MCP server in `config.toml`:

   ```toml
   [mcp_servers.discord]
   command = "node"
   args = ["/path/to/discord-channel.mjs"]
   ```

2. Opt the session in explicitly with `--channels` (repeatable or
   comma-separated):

   ```shell
   codex --channels server:discord
   codex exec --channels server:discord,server:ci-alerts "triage incoming reports"
   ```

   Being configured as an MCP server is **not** sufficient — a server only
   becomes a channel when the session opts it in. `plugin:<id>` entries opt in
   every MCP server contributed by that plugin.

3. Put the channel's credentials in `~/.codex/channels/<server-name>/.env`
   (`KEY=VALUE` lines, `#` comments, optional quotes, optional `export `).
   These are injected into that server's process environment **only when the
   server is opted in as a channel** for the session. Explicit `env` entries
   in the MCP server config win over `.env` values.

4. Run `/channels` in the TUI to see every requested entry and its state:
   active, blocked (channels disabled by config), blocked (not in the
   allowlist), invalid (parse error), or matched no configured MCP server.
   For active entries it also shows **which config source the matched server
   definition came from** (and which same-named definitions were overridden),
   plus whether the connected server actually declares the `codex/channel`
   capability — the most useful diagnostic when events don't arrive.

## Delivery semantics

- If the agent is idle, an event wakes it immediately as a new turn.
- If a turn is running, events queue and are delivered together when the turn
  ends, separated by `---`.
- The queue is bounded (64 events); when full the oldest event is dropped and
  a warning is logged.

Events are injected into the conversation as:

```
<channel source="discord" author="karl" channel_id="123">
message text, verbatim
</channel>
```

`source` is always the real MCP server name. Each meta entry from the event
becomes an attribute.

## `[channels]` config

```toml
[channels]
# Master switch, default true. Precedence, highest first: managed config,
# the CODEX_CHANNELS_ENABLED environment variable, then this value.
enabled = true

# Optional allowlist of entry strings. Unset = every --channels entry is
# allowed; empty list = none.
allowed = ["server:discord"]

# Persistent opt-ins, merged with --channels.
entries = ["server:discord"]
```

## Security model

- **Opt-in flag.** No delivery ever happens without the session's explicit
  `--channels` (or `[channels] entries`) opt-in. Non-opted-in servers get no
  notification listener at all, and delivery is gated a second time before an
  event is queued.
- **Allowlist policy.** `[channels] allowed` and `enabled = false` are
  enforced before a server is registered as a channel; managed config wins
  over the environment variable, which wins over user config.
- **Sender gating is the channel server's job.** Gate on *sender identity*,
  not the room: anyone who can post to a public Discord channel or open an
  issue can reach your agent otherwise. The bundled Discord channel refuses
  to forward anything until `DISCORD_ALLOWED_USER_IDS` is set.
- **Envelope hygiene.** Meta keys must match `[A-Za-z0-9_]+`; the reserved
  key `source` is dropped so an event can never spoof its origin; attribute
  values are entity-escaped so they cannot break out of the tag; bodies are
  truncated past 100k bytes.
- **Channel text is untrusted input.** Treat it like any other message from
  that outside sender — not as operator instructions.

## Build your own channel

A channel is a standard MCP server (any transport Codex supports; stdio is
simplest) with two small extensions:

1. Declare the experimental capability `codex/channel` in your `initialize`
   result. The value is a reserved empty object:

   ```json
   {
     "capabilities": {
       "experimental": { "codex/channel": {} },
       "tools": { "listChanged": false }
     }
   }
   ```

2. Push events as the JSON-RPC notification `notifications/codex/channel`:

   ```json
   {
     "jsonrpc": "2.0",
     "method": "notifications/codex/channel",
     "params": {
       "content": "the event text (required)",
       "meta": { "author": "karl", "channel_id": "123" }
     }
   }
   ```

   `content` must be a string; malformed events are dropped silently. `meta`
   is an optional object whose string values become envelope attributes.

Guidelines:

- Use the MCP `instructions` field to tell the agent how events look and how
  to reply (which tool, which id from `meta` to pass back).
- Gate inbound events on sender identity and log dropped senders with the
  exact id, so misconfigured allowlists are diagnosable.
- Expose reply actions as ordinary MCP tools; the host needs nothing special
  for them.
- Log to stderr only — stdout is the MCP transport. Codex forwards MCP server
  stderr into its log (`~/.codex/log/codex-tui.log` for the TUI) prefixed
  with `MCP server stderr (<program>)`.

The bundled Discord channel
([`codex-rs/channels/examples/discord/`](../codex-rs/channels/examples/discord/))
is the reference implementation, including a self-contained e2e test that
runs against a mock gateway with plain `node discord-channel.test.mjs`.
