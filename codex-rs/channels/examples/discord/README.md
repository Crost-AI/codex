# Discord channel for Codex

A bundled [Codex channel](../../../../docs/channels.md) that bridges a Discord
bot into a running Codex session. It is a single-file, zero-dependency MCP
stdio server for Node >= 22 (`discord-channel.mjs`):

- **Inbound**: Discord messages that pass its gates are pushed into the
  session as `notifications/codex/channel` events, rendered to the agent as
  `<channel source="discord" channel_id="..." author="...">…</channel>`.
- **Outbound**: the agent replies with ordinary MCP tools — `send_message`
  (auto-splits past Discord's 2000-character limit), `add_reaction`,
  `read_messages`, `create_poll` (native Discord polls; `read_poll` shows standings
  and voters, `end_poll` closes one of the bot's polls — bots cannot cast native
  votes, so agents vote by replying with their choice), `create_thread`
  (public workstream threads under an allowlisted parent, 24h auto-archive),
  `rename_thread` / `close_thread` (retitle a thread as the work evolves, or
  archive — optionally lock — it when the workstream wraps up; both verify
  the target is a thread and can never touch a regular channel), and
  `send_file` (upload a local file as an attachment, 10 MB bot limit — needs
  the **Attach Files** permission on the bot invite; threads need **Create
  Public Threads**, and renaming/closing threads the bot didn't create — or
  locking any thread — needs **Manage Threads**).
- **Files inbound**: incoming attachments arrive as `[attachment "name": url]`
  lines; the `read_attachment` tool downloads that URL (Discord CDN hosts
  only, 25 MB cap) to a temp file the agent can read with its normal file
  tools. Stray trailing punctuation (like a copied `]`) is trimmed from the
  URL, and expired signed CDN links are automatically re-signed through the
  bot token (`/attachments/refresh-urls`) and retried (v1.7.0+).

## Setup

### 1. Create the bot

1. Open the [Discord developer portal](https://discord.com/developers/applications)
   and create a **New Application**.
2. In **Bot**, add a bot and copy its **token** (you will only see it once).
3. Still in **Bot**, under *Privileged Gateway Intents*, enable
   **MESSAGE CONTENT INTENT**. Without it the gateway closes with code 4014
   and the bridge cannot read message text.

### 2. Invite it to your server

Use the OAuth2 URL (replace `YOUR_APP_ID`); the permissions integer grants
View Channels, Send Messages, Read Message History, and Add Reactions:

```
https://discord.com/oauth2/authorize?client_id=YOUR_APP_ID&scope=bot&permissions=68672
```

### 3. Find your Discord user id

Discord → *User Settings* → *Advanced* → enable **Developer Mode**, then
right-click your own name anywhere and pick **Copy User ID**. This id — not
your username, and not a server or channel id — goes in the allowlist below.

### 4. Configure credentials

Create `~/.codex/channels/discord/.env`:

```
DISCORD_BOT_TOKEN=your-bot-token
DISCORD_ALLOWED_USER_IDS=123456789012345678
```

Codex injects this file into the bridge's environment only when the session
opts the server in as a channel.

### 5. Configure the MCP server and launch

`~/.codex/config.toml`:

```toml
[mcp_servers.discord]
command = "node"
args = ["/path/to/codex/codex-rs/channels/examples/discord/discord-channel.mjs"]
```

```shell
codex --channels server:discord
```

Run `/channels` inside the session to confirm the entry is active and that
the connected server declares the `codex/channel` capability.

## Configuration reference

All configuration is environment variables (usually via the `.env` file):

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `DISCORD_BOT_TOKEN` | yes (for connectivity) | — | Bot token. When missing the bridge still serves MCP (tools return an error hint) but makes no gateway connection. |
| `DISCORD_ALLOWED_USER_IDS` | yes (to receive anything) | unset | Comma-separated Discord **user ids** allowed to reach the session. Unset drops every inbound message (with a one-time loud warning). `*` allows every human user. |
| `DISCORD_ALLOWED_BOT_IDS` | no | empty | Comma-separated **bot** user ids allowed through the bot filter (see bot-to-bot below). `*` in the user allowlist does not affect bots. |
| `DISCORD_ALLOW_DMS` | no | `true` | Set to `false` to ignore direct messages. |
| `DISCORD_CHANNEL_IDS` | no | all channels | Comma-separated channel ids; guild messages outside them are ignored. Threads inherit their **parent** channel's allowlist (tracked from gateway events, with a REST fallback). |
| `DISCORD_REQUIRE_MENTION` | no | `true` | Guild messages must be addressed to the bot: an @mention (user or its managed role), a Discord **reply** to one of the bot's messages, or a message inside the continuation window below. Set to `false` to forward all allowed guild messages. |
| — listening mode | | | With `DISCORD_REQUIRE_MENTION=false`, messages not directed at the bot carry `addressed="other"` (someone else was mentioned/replied to) or `addressed="none"` (open chatter); the agent is instructed to read them for context and stay silent unless correcting a clear factual error or something urgent. Pair with `DISCORD_CHANNEL_IDS`. |
| `DISCORD_MENTION_WINDOW_SECONDS` | no | `60` | Sliding continuation window: after a sender's message is forwarded, their follow-ups in the same channel pass the mention gate for this long. Covers content split by the 2000-char limit (only the first chunk carries the mention). `0` disables. |
| `DISCORD_ATTACHMENT_HOSTS` | no | Discord CDN hosts | Hosts `read_attachment` may fetch from (used by the e2e test). |
| `DISCORD_GATEWAY_URL` | no | `wss://gateway.discord.gg` | Gateway override (used by the e2e test). |
| `DISCORD_API_BASE` | no | `https://discord.com/api/v10` | REST base override (used by the e2e test). |

## Slash commands from Discord

A few session commands work straight from Discord — the **host** executes them and replies in the channel; the agent never sees them (and they don't interrupt whatever it's doing):

| You type | You get back |
| --- | --- |
| `/status` (or `/session`) | Session/thread id, model, working directory, approval + sandbox policy |
| `/channels` | The session's channel entries and their resolution state |
| `/help` | The list above |

With the mention requirement on, address the bot as usual: `@codex /status` (the mention is stripped before the command is parsed). Command messages must *start* with the `/`; anything else — including `/commands` the host doesn't recognize — forwards to the agent as a normal message. Bot-authored messages are never treated as commands, so another agent can't drive your session's host commands.

## Security model

The allowlist gates on **sender identity, not the room**: anyone who can post
in a public channel the bot can read would otherwise be able to steer your
agent. Nothing is forwarded until `DISCORD_ALLOWED_USER_IDS` is set, and
messages from unlisted senders are dropped (loudly, with the id you need).
Text arriving from Discord is untrusted input to the agent — the server's MCP
instructions tell it never to treat channel content as operator instructions.

## Bot-to-bot bridging

Bot-authored messages are dropped by default. To let another agent's bot
through, list its **bot user id** in `DISCORD_ALLOWED_BOT_IDS`; its messages
are forwarded with `bot="true"` in the meta.

- **Loop hazard.** Two agents that @mention each other in acknowledgements
  will ping-pong forever. The bridge's instructions tell the agent to reply
  to `bot="true"` messages tersely, only when it moves work forward, and
  never to mention a bot back in a mere acknowledgement. Prefer
  **one-directional listening**: only one side lists the other in
  `DISCORD_ALLOWED_BOT_IDS`.
- **Mentions from bots are easy to get wrong.** A bot that writes the plain
  text `@name` has NOT mentioned anyone — Discord mentions are the raw
  `<@userId>` (or `<@&roleId>`) syntax. For bot-to-bot channels either
  instruct the other agent to emit raw mention syntax, or restrict with
  `DISCORD_CHANNEL_IDS` and set `DISCORD_REQUIRE_MENTION=false`.

## Troubleshooting

Codex forwards the bridge's stderr into its own log as lines prefixed
`MCP server stderr (node): …`. For the interactive TUI that log is
`$CODEX_HOME/log/codex-tui.log` (usually `~/.codex/log/codex-tui.log`);
`tail -f` it while testing. Each log line maps to one cause and one fix:

| Log line (stderr) | Cause → fix |
| --- | --- |
| `discord-channel bridge v… starting (MCP over stdio)` missing entirely | The bridge never launched. Check the `[mcp_servers.discord]` `command`/`args` paths and that `node` >= 22 is on PATH. |
| `DISCORD_BOT_TOKEN is missing — Discord connectivity is disabled.` | No token in the environment. Add it to `~/.codex/channels/discord/.env` and restart the session. |
| `WARNING: DISCORD_ALLOWED_USER_IDS is not set — ALL inbound Discord messages will be dropped.` | The sender allowlist is required. Set it to your user id (or `*`). |
| `logged in as <name>` present, then `dropping message <id>: sender <name> (id <id>) is not in DISCORD_ALLOWED_USER_IDS` | The most common misconfiguration: the allowlist holds the wrong id (a server/channel/app id). Copy the **exact id from that log line** into `DISCORD_ALLOWED_USER_IDS`. |
| `logged in as <name>` present, but no new lines when you message | Either no message arrived since this session started, or your "@mention" was typed as plain text (pick the bot from Discord's mention picker instead), or the message is in a channel filtered out by `DISCORD_CHANNEL_IDS`. |
| `gateway closed with fatal code 4014: disallowed intents — …` | Enable the **MESSAGE CONTENT INTENT** in the developer portal (Bot settings page), then restart. |
| `gateway closed with fatal code 4004: authentication failed — …` | The bot token is invalid. Re-copy it into `~/.codex/channels/discord/.env`. |
| `gateway heartbeat was not acknowledged; treating connection as a zombie and reconnecting` | Transient network trouble; the bridge resumes automatically. Only investigate if it repeats constantly. |

The bridge logs its version at startup and the version is bumped on every
bridge change — check that line first when verifying a fix landed in the
running process.

## Testing

A self-contained e2e test drives the real bridge over stdio against a mock
gateway and mock REST server (no network, no token needed):

```shell
node discord-channel.test.mjs
```
