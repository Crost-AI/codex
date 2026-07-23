#!/usr/bin/env node
// Self-contained e2e test for discord-channel.mjs.
//
// Runs the REAL bridge as a child process over stdio against a mock Discord
// gateway (a minimal RFC 6455 websocket server) and a mock REST server, so it
// never touches the network. Run with plain `node discord-channel.test.mjs`.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer as createHttpServer } from "node:http";
import { createServer as createNetServer } from "node:net";
import { createInterface } from "node:readline";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const BRIDGE = join(dirname(fileURLToPath(import.meta.url)), "discord-channel.mjs");
const OVERALL_TIMEOUT_MS = 30_000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

// ---------------------------------------------------------------------------
// Minimal RFC 6455 websocket server (handshake + masked text frames + close)
// ---------------------------------------------------------------------------

class MockGateway {
  constructor() {
    this.socket = null;
    this.frames = [];
    this.frameWaiters = [];
    this.connWaiters = [];
    this.server = createNetServer((socket) => this.#handleSocket(socket));
  }

  listen() {
    return new Promise((resolve) => {
      this.server.listen(0, "127.0.0.1", () => resolve(this.server.address().port));
    });
  }

  #handleSocket(socket) {
    let buffer = Buffer.alloc(0);
    let upgraded = false;
    socket.on("data", (data) => {
      buffer = Buffer.concat([buffer, data]);
      if (!upgraded) {
        const headerEnd = buffer.indexOf("\r\n\r\n");
        if (headerEnd === -1) return;
        const header = buffer.subarray(0, headerEnd).toString("utf8");
        buffer = buffer.subarray(headerEnd + 4);
        const keyMatch = /Sec-WebSocket-Key: (.+)\r\n/i.exec(`${header}\r\n`);
        const accept = createHash("sha1")
          .update(`${keyMatch[1].trim()}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
          .digest("base64");
        socket.write(
          "HTTP/1.1 101 Switching Protocols\r\n" +
            "Upgrade: websocket\r\n" +
            "Connection: Upgrade\r\n" +
            `Sec-WebSocket-Accept: ${accept}\r\n\r\n`,
        );
        upgraded = true;
        this.socket = socket;
        for (const waiter of this.connWaiters.splice(0)) waiter(socket);
      }
      buffer = this.#drainFrames(socket, buffer);
    });
    socket.on("error", () => {});
  }

  #drainFrames(socket, buffer) {
    for (;;) {
      if (buffer.length < 2) return buffer;
      const opcode = buffer[0] & 0x0f;
      const masked = (buffer[1] & 0x80) !== 0;
      let len = buffer[1] & 0x7f;
      let offset = 2;
      if (len === 126) {
        if (buffer.length < offset + 2) return buffer;
        len = buffer.readUInt16BE(offset);
        offset += 2;
      } else if (len === 127) {
        if (buffer.length < offset + 8) return buffer;
        len = Number(buffer.readBigUInt64BE(offset));
        offset += 8;
      }
      const maskLen = masked ? 4 : 0;
      if (buffer.length < offset + maskLen + len) return buffer;
      const mask = masked ? buffer.subarray(offset, offset + 4) : null;
      let payload = buffer.subarray(offset + maskLen, offset + maskLen + len);
      if (mask) {
        payload = Buffer.from(payload);
        for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i % 4];
      }
      buffer = buffer.subarray(offset + maskLen + len);
      if (opcode === 0x1) {
        const frame = JSON.parse(payload.toString("utf8"));
        this.frames.push(frame);
        // ACK heartbeats like the real gateway so the bridge's zombie
        // detection never trips mid-test.
        if (frame.op === 1 && this.socket === socket) {
          this.send({ op: 11 });
        }
        for (const waiter of this.frameWaiters.splice(0)) waiter();
      } else if (opcode === 0x8) {
        // Echo the close and end the TCP stream.
        socket.write(Buffer.from([0x88, 0x00]));
        socket.end();
      } else if (opcode === 0x9) {
        socket.write(this.#encodeFrame(payload, 0xa));
      }
    }
  }

  #encodeFrame(payload, opcode = 0x1) {
    const len = payload.length;
    let header;
    if (len < 126) {
      header = Buffer.from([0x80 | opcode, len]);
    } else if (len < 65536) {
      header = Buffer.alloc(4);
      header[0] = 0x80 | opcode;
      header[1] = 126;
      header.writeUInt16BE(len, 2);
    } else {
      header = Buffer.alloc(10);
      header[0] = 0x80 | opcode;
      header[1] = 127;
      header.writeBigUInt64BE(BigInt(len), 2);
    }
    return Buffer.concat([header, payload]);
  }

  send(payload) {
    assert.ok(this.socket, "gateway has no connected client");
    this.socket.write(this.#encodeFrame(Buffer.from(JSON.stringify(payload), "utf8")));
  }

  async waitForConnection(timeoutMs = 5000) {
    if (this.socket) return;
    await this.#wait(this.connWaiters, timeoutMs, "gateway connection");
  }

  async waitForFrame(predicate, timeoutMs = 5000, label = "gateway frame") {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const found = this.frames.find(predicate);
      if (found) return found;
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new Error(`timed out waiting for ${label}`);
      await this.#wait(this.frameWaiters, remaining, label).catch(() => {});
    }
  }

  #wait(list, timeoutMs, label) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), timeoutMs);
      list.push((value) => {
        clearTimeout(timer);
        resolve(value);
      });
    });
  }

  close() {
    try {
      this.socket?.destroy();
    } catch {}
    this.server.close();
  }
}

// ---------------------------------------------------------------------------
// Mock Discord REST server
// ---------------------------------------------------------------------------

class MockRest {
  constructor() {
    this.requests = [];
    let counter = 0;
    this.server = createHttpServer((req, res) => {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => {
        const url = new URL(req.url, "http://127.0.0.1");
        let parsedBody = null;
        try {
          parsedBody = body.length > 0 ? JSON.parse(body) : null;
        } catch {
          // multipart or raw upload — keep raw only
        }
        const record = {
          method: req.method,
          path: url.pathname,
          query: Object.fromEntries(url.searchParams),
          contentType: req.headers["content-type"] ?? "",
          body: parsedBody,
          raw: body,
        };
        this.requests.push(record);
        if (req.method === "GET" && /^\/channels\/[^/]+\/messages\/PM1$/.test(url.pathname)) {
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(
            JSON.stringify({
              id: "PM1",
              poll: {
                question: { text: "Ship it?" },
                allow_multiselect: false,
                expiry: "2026-07-23T00:00:00Z",
                answers: [
                  { answer_id: 1, poll_media: { text: "Yes" } },
                  { answer_id: 2, poll_media: { text: "No" } },
                ],
                results: {
                  is_finalized: false,
                  answer_counts: [{ id: 1, count: 2, me_voted: false }],
                },
              },
            }),
          );
          return;
        }
        if (req.method === "GET" && url.pathname.includes("/polls/PM1/answers/")) {
          const answerId = url.pathname.split("/answers/")[1];
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(
            JSON.stringify({ users: answerId === "1" ? [{ id: "111", username: "karl" }] : [] }),
          );
          return;
        }
        if (req.method === "POST" && url.pathname.includes("/polls/PM1/expire")) {
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ id: "PM1" }));
          return;
        }
        if (req.method === "GET" && url.pathname.startsWith("/cdn/")) {
          res.writeHead(200, { "Content-Type": "text/plain" });
          res.end("cdn-attachment-bytes");
        } else if (req.method === "POST" && /^\/channels\/[^/]+\/threads$/.test(url.pathname)) {
          const parent = url.pathname.split("/")[2];
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ id: "T9", parent_id: parent, type: 11 }));
        } else if (req.method === "GET" && /^\/channels\/[^/]+$/.test(url.pathname)) {
          // Channel/thread lookup: T* ids are threads, everything else is a
          // plain guild text channel (type 0).
          const id = url.pathname.split("/")[2];
          const parents = { T1: "G1", T2: "GX" };
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(
            id.startsWith("T")
              ? JSON.stringify({ id, parent_id: parents[id] ?? null, type: 11 })
              : JSON.stringify({ id, parent_id: null, type: 0 }),
          );
        } else if (req.method === "PATCH" && /^\/channels\/[^/]+$/.test(url.pathname)) {
          // Thread modify (rename / archive).
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ id: url.pathname.split("/")[2], ...(parsedBody ?? {}) }));
        } else if (req.method === "POST" && /^\/channels\/[^/]+\/messages$/.test(url.pathname)) {
          counter += 1;
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ id: `m${counter}`, channel_id: url.pathname.split("/")[2] }));
        } else if (req.method === "PUT" && url.pathname.includes("/reactions/")) {
          res.writeHead(204);
          res.end();
        } else if (req.method === "GET" && /^\/channels\/[^/]+\/messages$/.test(url.pathname)) {
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(
            JSON.stringify([
              {
                id: "h2",
                author: { id: "111", username: "karl" },
                content: "newest",
                timestamp: "2026-07-16T12:00:00Z",
                attachments: [{ url: "http://x/a.png" }],
              },
              {
                id: "h1",
                author: { id: "999", username: "testbot" },
                content: "older",
                timestamp: "2026-07-16T11:00:00Z",
                attachments: [],
              },
            ]),
          );
        } else {
          res.writeHead(404, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ message: "not found" }));
        }
      });
    });
  }

  listen() {
    return new Promise((resolve) => {
      this.server.listen(0, "127.0.0.1", () => resolve(this.server.address().port));
    });
  }

  close() {
    this.server.close();
  }
}

// ---------------------------------------------------------------------------
// MCP stdio client for the bridge child process
// ---------------------------------------------------------------------------

class BridgeClient {
  constructor(child) {
    this.child = child;
    this.nextId = 1;
    this.pending = new Map();
    this.notifications = [];
    this.notificationWaiters = [];
    this.stderr = "";
    createInterface({ input: child.stdout, crlfDelay: Infinity }).on("line", (line) => {
      if (line.trim().length === 0) return;
      const msg = JSON.parse(line);
      if (msg.id !== undefined && this.pending.has(msg.id)) {
        const { resolve } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        resolve(msg);
      } else if (msg.method) {
        this.notifications.push(msg);
        for (const waiter of this.notificationWaiters.splice(0)) waiter();
      }
    });
    child.stderr.on("data", (data) => {
      this.stderr += data.toString();
      for (const line of data.toString().split("\n")) {
        if (line.trim().length > 0) console.log(`  [bridge stderr] ${line}`);
      }
    });
  }

  request(method, params) {
    const id = this.nextId++;
    const { promise, resolve } = deferred();
    this.pending.set(id, { resolve });
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return promise;
  }

  notify(method, params) {
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  async waitForNotification(timeoutMs = 5000) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (this.notifications.length > 0) return this.notifications.shift();
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new Error("timed out waiting for a channel notification");
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("timeout")), remaining);
        this.notificationWaiters.push(() => {
          clearTimeout(timer);
          resolve();
        });
      }).catch(() => {});
    }
  }

  async assertNoNotification(waitMs = 300) {
    await sleep(waitMs);
    assert.equal(
      this.notifications.length,
      0,
      `expected no notification, got: ${JSON.stringify(this.notifications)}`,
    );
  }

  async waitForStderr(needle, timeoutMs = 3000) {
    const deadline = Date.now() + timeoutMs;
    while (!this.stderr.includes(needle)) {
      if (Date.now() > deadline) {
        throw new Error(`timed out waiting for stderr to contain: ${needle}`);
      }
      await sleep(25);
    }
  }
}

// ---------------------------------------------------------------------------
// The test sequence
// ---------------------------------------------------------------------------

let passed = 0;
function pass(label) {
  passed += 1;
  console.log(`PASS: ${label}`);
}

async function main() {
  const gateway = new MockGateway();
  const rest = new MockRest();
  const gwPort = await gateway.listen();
  const restPort = await rest.listen();

  const child = spawn(process.execPath, [BRIDGE], {
    env: {
      ...process.env,
      DISCORD_BOT_TOKEN: "test-token",
      DISCORD_ALLOWED_USER_IDS: "111",
      DISCORD_ALLOWED_BOT_IDS: "666",
      DISCORD_GATEWAY_URL: `ws://127.0.0.1:${gwPort}`,
      DISCORD_API_BASE: `http://127.0.0.1:${restPort}`,
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const client = new BridgeClient(child);
  const cleanup = () => {
    try {
      child.kill("SIGKILL");
    } catch {}
    gateway.close();
    rest.close();
  };

  try {
    // 1. initialize declares the channel capability and instructions.
    const init = await client.request("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "e2e-test", version: "0.0.0" },
    });
    assert.ok(init.result.capabilities.experimental["codex/channel"], "codex/channel capability");
    assert.deepEqual(
      init.result.capabilities.experimental["codex/channel"].commands,
      {
        reply_tool: "send_message",
        target_meta: "channel_id",
        target_arg: "channel_id",
        content_arg: "content",
      },
      "host slash-command reply descriptor",
    );
    assert.equal(init.result.serverInfo.version, "1.6.0");
    assert.ok(init.result.instructions.includes("send_message"));
    assert.ok(init.result.instructions.includes("loop forever"));
    await client.waitForStderr("discord-channel bridge v1.6.0 starting");
    pass("initialize declares codex/channel, commands descriptor, version 1.6.0, and instructions");

    // 2. tools/list returns exactly the three tools.
    client.notify("notifications/initialized", {});
    const tools = await client.request("tools/list", {});
    assert.deepEqual(
      tools.result.tools.map((t) => t.name).sort(),
      [
        "add_reaction",
        "close_thread",
        "create_poll",
        "create_thread",
        "end_poll",
        "read_attachment",
        "read_messages",
        "read_poll",
        "rename_thread",
        "send_file",
        "send_message",
      ],
    );
    for (const tool of tools.result.tools) assert.ok(tool.inputSchema);
    pass("tools/list returns all eleven tools");

    // 3. HELLO -> IDENTIFY with the token and intents.
    await gateway.waitForConnection();
    gateway.send({ op: 10, d: { heartbeat_interval: 200 } });
    const identify = await gateway.waitForFrame((f) => f.op === 2, 5000, "IDENTIFY");
    assert.equal(identify.d.token, "test-token");
    assert.equal(identify.d.intents, 37377);
    gateway.send({
      op: 0,
      s: 1,
      t: "READY",
      d: {
        session_id: "sess1",
        resume_gateway_url: `ws://127.0.0.1:${gwPort}`,
        user: { id: "999", username: "testbot" },
      },
    });
    await client.waitForStderr("logged in as testbot");
    pass("HELLO triggers IDENTIFY with token and intents 37377");

    // 4. Heartbeats are sent on the HELLO interval (the mock ACKs them).
    await gateway.waitForFrame((f) => f.op === 1, 1500, "heartbeat");
    pass("heartbeat observed on the HELLO interval");

    // 5. Allowed DM is forwarded with meta.
    gateway.send({
      op: 0,
      s: 2,
      t: "MESSAGE_CREATE",
      d: {
        id: "M1",
        channel_id: "C1",
        content: "hello codex",
        author: { id: "111", username: "karl", bot: false },
        attachments: [{ url: "http://x/y.png" }],
      },
    });
    const dm = await client.waitForNotification();
    assert.equal(dm.method, "notifications/codex/channel");
    assert.equal(dm.params.content, "hello codex\n[attachment: http://x/y.png]");
    assert.deepEqual(dm.params.meta, {
      channel_id: "C1",
      message_id: "M1",
      author: "karl",
      author_id: "111",
      dm: "true",
    });
    pass("allowed DM forwarded with content and meta");

    // 6. Disallowed DM sender is dropped with a loud log naming the id.
    gateway.send({
      op: 0,
      s: 3,
      t: "MESSAGE_CREATE",
      d: {
        id: "M2",
        channel_id: "C1",
        content: "let me in",
        author: { id: "222", username: "mallory", bot: false },
      },
    });
    await client.waitForStderr(
      "dropping message M2: sender mallory (id 222) is not in DISCORD_ALLOWED_USER_IDS",
    );
    await client.assertNoNotification();
    pass("disallowed sender dropped with the exact id in the log");

    // 7. Unmentioned guild message is dropped silently.
    gateway.send({
      op: 0,
      s: 4,
      t: "MESSAGE_CREATE",
      d: {
        id: "M3",
        channel_id: "C2",
        guild_id: "G1",
        content: "ambient chatter",
        author: { id: "111", username: "karl", bot: false },
        mentions: [],
        mention_roles: [],
      },
    });
    await client.assertNoNotification();
    assert.ok(!client.stderr.includes("dropping message M3"), "room-gate drops stay silent");
    pass("unmentioned guild message dropped silently");

    // 8. User-mentioned guild message forwarded with the mention stripped.
    gateway.send({
      op: 0,
      s: 5,
      t: "MESSAGE_CREATE",
      d: {
        id: "M4",
        channel_id: "C2",
        guild_id: "G1",
        content: "<@999> do the thing",
        author: { id: "111", username: "karl", bot: false },
        mentions: [{ id: "999" }],
        mention_roles: [],
      },
    });
    const mentioned = await client.waitForNotification();
    assert.equal(mentioned.params.content, "do the thing");
    assert.equal(mentioned.params.meta.guild_id, "G1");
    pass("mentioned guild message forwarded with mention stripped");

    // 9. Role mentions of the bot's managed role count and are stripped.
    gateway.send({
      op: 0,
      s: 6,
      t: "GUILD_CREATE",
      d: {
        id: "G1",
        roles: [
          { id: "R1", tags: { bot_id: "999" } },
          { id: "R2", name: "unrelated" },
        ],
      },
    });
    await sleep(50);
    gateway.send({
      op: 0,
      s: 7,
      t: "MESSAGE_CREATE",
      d: {
        id: "M5",
        channel_id: "C2",
        guild_id: "G1",
        content: "<@&R1> via role ping",
        author: { id: "111", username: "karl", bot: false },
        mentions: [],
        mention_roles: ["R1"],
      },
    });
    const rolePing = await client.waitForNotification();
    assert.equal(rolePing.params.content, "via role ping");
    assert.equal(rolePing.params.meta.guild_id, "G1");
    pass("role mention of the bot's managed role accepted and stripped");

    // 10. Bot-authored messages are dropped by default.
    gateway.send({
      op: 0,
      s: 8,
      t: "MESSAGE_CREATE",
      d: {
        id: "M6",
        channel_id: "C2",
        guild_id: "G1",
        content: "<@999> beep",
        author: { id: "555", username: "otherbot", bot: true },
        mentions: [{ id: "999" }],
        mention_roles: [],
      },
    });
    await client.assertNoNotification();
    pass("bot-authored message dropped by default");

    // 11. Allowlisted bots are forwarded with bot=\"true\".
    gateway.send({
      op: 0,
      s: 9,
      t: "MESSAGE_CREATE",
      d: {
        id: "M7",
        channel_id: "C2",
        guild_id: "G1",
        content: "<@999> status?",
        author: { id: "666", username: "friendbot", bot: true },
        mentions: [{ id: "999" }],
        mention_roles: [],
      },
    });
    const botMsg = await client.waitForNotification();
    assert.equal(botMsg.params.content, "status?");
    assert.equal(botMsg.params.meta.bot, "true");
    assert.equal(botMsg.params.meta.author_id, "666");
    pass("allowlisted bot forwarded with bot=\"true\" meta");

    // 12. send_message chunks long content; only the first chunk carries the
    //     reply reference.
    const line = "x".repeat(999);
    const longContent = [line, line, line, line, line].join("\n"); // 4999 chars
    const sendResult = await client.request("tools/call", {
      name: "send_message",
      arguments: { channel_id: "C1", content: longContent, reply_to_message_id: "M1" },
    });
    assert.ok(!sendResult.result.isError, JSON.stringify(sendResult.result));
    const posts = rest.requests.filter(
      (r) => r.method === "POST" && r.path === "/channels/C1/messages",
    );
    assert.ok(posts.length >= 3, `expected >= 3 chunks, got ${posts.length}`);
    for (const post of posts) assert.ok(post.body.content.length <= 2000);
    assert.deepEqual(posts[0].body.message_reference, { message_id: "M1" });
    for (const post of posts.slice(1)) assert.equal(post.body.message_reference, undefined);
    assert.equal(posts.map((p) => p.body.content).join("\n"), longContent);
    pass("send_message splits content at newlines with reply reference on the first chunk only");

    // 13. add_reaction hits the URL-encoded reaction endpoint.
    const reaction = await client.request("tools/call", {
      name: "add_reaction",
      arguments: { channel_id: "C1", message_id: "M1", emoji: "👍" },
    });
    assert.ok(!reaction.result.isError, JSON.stringify(reaction.result));
    const put = rest.requests.find((r) => r.method === "PUT");
    assert.equal(put.path, "/channels/C1/messages/M1/reactions/%F0%9F%91%8D/@me");
    pass("add_reaction PUTs the encoded emoji reaction");

    // 14. read_messages fetches with the limit and returns simplified JSON.
    const read = await client.request("tools/call", {
      name: "read_messages",
      arguments: { channel_id: "C1", limit: 2 },
    });
    assert.ok(!read.result.isError, JSON.stringify(read.result));
    const get = rest.requests.find((r) => r.method === "GET");
    assert.equal(get.path, "/channels/C1/messages");
    assert.equal(get.query.limit, "2");
    const simplified = JSON.parse(read.result.content[0].text);
    assert.equal(simplified.length, 2);
    assert.deepEqual(simplified[0], {
      id: "h2",
      author: "karl",
      author_id: "111",
      content: "newest",
      timestamp: "2026-07-16T12:00:00Z",
      attachments: ["http://x/a.png"],
    });
    pass("read_messages GETs with limit and returns simplified JSON");

    // ── Phase 2: a second bridge with a channel allowlist, a short
    // mention window, and test attachment hosts — covers the ported
    // features: continuation window, reply-to-bot, thread allowlist
    // inheritance, create_thread/create_poll, send_file/read_attachment.
    const gateway2 = new MockGateway();
    const rest2 = new MockRest();
    const gwPort2 = await gateway2.listen();
    const restPort2 = await rest2.listen();
    const child2 = spawn(process.execPath, [BRIDGE], {
      env: {
        ...process.env,
        DISCORD_BOT_TOKEN: "test-token",
        DISCORD_ALLOWED_USER_IDS: "111",
        DISCORD_CHANNEL_IDS: "G1",
        DISCORD_MENTION_WINDOW_SECONDS: "2",
        DISCORD_ATTACHMENT_HOSTS: "127.0.0.1",
        DISCORD_GATEWAY_URL: `ws://127.0.0.1:${gwPort2}`,
        DISCORD_API_BASE: `http://127.0.0.1:${restPort2}`,
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    const client2 = new BridgeClient(child2);
    try {
      await client2.request("initialize", {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "e2e-test-2", version: "0.0.0" },
      });
      client2.notify("notifications/initialized", {});
      await gateway2.waitForConnection();
      gateway2.send({ op: 10, d: { heartbeat_interval: 200 } });
      await gateway2.waitForFrame((f) => f.op === 2, 5000, "IDENTIFY (bridge2)");
      gateway2.send({
        op: 0,
        s: 1,
        t: "READY",
        d: { session_id: "sess2", resume_gateway_url: "", user: { id: "999", username: "testbot" } },
      });
      await client2.waitForStderr("logged in as testbot");

      // 16. Continuation window: a mentioned message opens the floor; the
      // unmentioned follow-up from the same sender+channel flows.
      const g1 = { guild_id: "GUILD", channel_id: "G1" };
      gateway2.send({
        op: 0, s: 2, t: "MESSAGE_CREATE",
        d: { id: "W1", ...g1, content: "<@999> part one", author: { id: "111", username: "karl" }, mentions: [{ id: "999" }] },
      });
      const w1 = await client2.waitForNotification();
      assert.equal(w1.params.content, "part one");
      gateway2.send({
        op: 0, s: 3, t: "MESSAGE_CREATE",
        d: { id: "W2", ...g1, content: "part two, no mention", author: { id: "111", username: "karl" }, mentions: [] },
      });
      const w2 = await client2.waitForNotification();
      assert.equal(w2.params.content, "part two, no mention");
      pass("continuation window forwards the unmentioned follow-up chunk");

      // 17. After the window expires, unmentioned drops — but a Discord
      // reply to one of the bot's messages still counts as addressed.
      await sleep(2300);
      gateway2.send({
        op: 0, s: 4, t: "MESSAGE_CREATE",
        d: { id: "W3", ...g1, content: "late, no mention", author: { id: "111", username: "karl" }, mentions: [] },
      });
      await client2.assertNoNotification();
      gateway2.send({
        op: 0, s: 5, t: "MESSAGE_CREATE",
        d: {
          id: "W4", ...g1, content: "replying to you", author: { id: "111", username: "karl" },
          mentions: [], referenced_message: { id: "old", author: { id: "999" } },
        },
      });
      const w4 = await client2.waitForNotification();
      assert.equal(w4.params.content, "replying to you");
      pass("expired window drops; reply-to-bot still counts as addressed");

      // 18. Threads inherit the parent's channel allowlist (REST fallback
      // when THREAD_CREATE was missed): T1->G1 allowed, T2->GX dropped.
      await sleep(2300); // let W4's continuation window lapse
      gateway2.send({
        op: 0, s: 6, t: "MESSAGE_CREATE",
        d: { id: "W5", guild_id: "GUILD", channel_id: "T1", content: "<@999> in thread", author: { id: "111", username: "karl" }, mentions: [{ id: "999" }] },
      });
      const w5 = await client2.waitForNotification();
      assert.equal(w5.params.meta.channel_id, "T1");
      assert.equal(w5.params.meta.parent_channel_id, "G1");
      gateway2.send({
        op: 0, s: 7, t: "MESSAGE_CREATE",
        d: { id: "W6", guild_id: "GUILD", channel_id: "T2", content: "<@999> wrong thread", author: { id: "111", username: "karl" }, mentions: [{ id: "999" }] },
      });
      await client2.assertNoNotification();
      pass("threads inherit the parent allowlist (T1->G1 allowed, T2->GX dropped)");

      // 19. create_thread enforces the parent allowlist and remembers the
      // new thread's parent.
      const badThread = await client2.request("tools/call", {
        name: "create_thread",
        arguments: { parent_channel_id: "G9", name: "nope" },
      });
      assert.ok(badThread.result.isError, "create_thread under non-allowlisted parent must fail");
      const goodThread = await client2.request("tools/call", {
        name: "create_thread",
        arguments: { parent_channel_id: "G1", name: "release @everyone <@&5> work", message: "kickoff" },
      });
      assert.ok(!goodThread.result.isError, JSON.stringify(goodThread.result));
      assert.ok(goodThread.result.content[0].text.includes("T9"));
      assert.ok(goodThread.result.content[0].text.includes('"release work"'), goodThread.result.content[0].text);
      gateway2.send({
        op: 0, s: 8, t: "MESSAGE_CREATE",
        d: { id: "W7", guild_id: "GUILD", channel_id: "T9", content: "<@999> in new thread", author: { id: "111", username: "karl" }, mentions: [{ id: "999" }] },
      });
      const w7 = await client2.waitForNotification();
      assert.equal(w7.params.meta.parent_channel_id, "G1");
      pass("create_thread sanitizes the name, gates the parent, and the thread inherits inbound");

      // 19b. rename_thread / close_thread: threads only, allowlist-gated.
      const rename = await client2.request("tools/call", {
        name: "rename_thread",
        arguments: { thread_id: "T9", name: "release @everyone done" },
      });
      assert.ok(!rename.result.isError, JSON.stringify(rename.result));
      assert.ok(rename.result.content[0].text.includes('"release done"'), rename.result.content[0].text);
      const renamePatch = rest2.requests.findLast((r) => r.method === "PATCH" && r.path === "/channels/T9");
      assert.equal(renamePatch?.body?.name, "release done");
      // G1 is a plain channel (mock GET type 0) — must be refused, otherwise
      // thread tools could rename real channels.
      const renameChan = await client2.request("tools/call", {
        name: "rename_thread",
        arguments: { thread_id: "G1", name: "evil rename" },
      });
      assert.ok(renameChan.result.isError, "rename_thread must refuse non-thread channels");
      assert.ok(renameChan.result.content[0].text.includes("not a thread"), renameChan.result.content[0].text);
      assert.ok(
        !rest2.requests.some((r) => r.method === "PATCH" && r.path === "/channels/G1"),
        "no PATCH was issued against the regular channel",
      );
      // T2's parent (GX) is outside the channel allowlist.
      const renameDenied = await client2.request("tools/call", {
        name: "rename_thread",
        arguments: { thread_id: "T2", name: "nope" },
      });
      assert.ok(renameDenied.result.isError, "rename_thread must gate on the parent allowlist");
      assert.ok(renameDenied.result.content[0].text.includes("DISCORD_CHANNEL_IDS"), renameDenied.result.content[0].text);
      const close = await client2.request("tools/call", {
        name: "close_thread",
        arguments: { thread_id: "T9", lock: true },
      });
      assert.ok(!close.result.isError, JSON.stringify(close.result));
      assert.ok(close.result.content[0].text.includes("archived + locked"), close.result.content[0].text);
      const closePatch = rest2.requests.findLast((r) => r.method === "PATCH" && r.path === "/channels/T9");
      assert.equal(closePatch?.body?.archived, true);
      assert.equal(closePatch?.body?.locked, true);
      pass("rename_thread/close_thread modify threads only, with allowlist gating");

      // 20. create_poll posts a native poll body.
      const poll = await client2.request("tools/call", {
        name: "create_poll",
        arguments: {
          channel_id: "G1",
          question: "Ship it?",
          answers: ["Yes", { text: "No", emoji: "👎" }],
          duration: 48,
        },
      });
      assert.ok(!poll.result.isError, JSON.stringify(poll.result));
      const pollReq = rest2.requests.find((r) => r.body?.poll);
      assert.equal(pollReq.body.poll.question.text, "Ship it?");
      assert.equal(pollReq.body.poll.answers.length, 2);
      assert.equal(pollReq.body.poll.answers[1].poll_media.emoji.name, "👎");
      assert.equal(pollReq.body.poll.duration, 48);
      pass("create_poll posts the native poll body");

      // 20b. read_poll returns standings + voters; end_poll expires.
      const pollRead = await client2.request("tools/call", {
        name: "read_poll",
        arguments: { channel_id: "G1", message_id: "PM1" },
      });
      assert.ok(!pollRead.result.isError, JSON.stringify(pollRead.result));
      const pollData = JSON.parse(pollRead.result.content[0].text);
      assert.equal(pollData.question, "Ship it?");
      assert.equal(pollData.answers[0].count, 2);
      assert.deepEqual(pollData.answers[0].voters, ["karl"]);
      assert.deepEqual(pollData.answers[1].voters, []);
      const pollEnd = await client2.request("tools/call", {
        name: "end_poll",
        arguments: { channel_id: "G1", message_id: "PM1" },
      });
      assert.ok(!pollEnd.result.isError, JSON.stringify(pollEnd.result));
      assert.ok(rest2.requests.some((r) => r.path.includes("/polls/PM1/expire")));
      pass("read_poll returns standings and voters; end_poll expires the poll");

      // 21. send_file uploads multipart; read_attachment round-trips.
      const fsPromises = await import("node:fs/promises");
      const osModule = await import("node:os");
      const pathModule = await import("node:path");
      const tmpFile = pathModule.join(osModule.tmpdir(), `codex-sendfile-${process.pid}.txt`);
      await fsPromises.writeFile(tmpFile, "upload payload");
      const sent = await client2.request("tools/call", {
        name: "send_file",
        arguments: { channel_id: "G1", file_path: tmpFile, caption: "log attached" },
      });
      assert.ok(!sent.result.isError, JSON.stringify(sent.result));
      const upload = rest2.requests.find((r) => r.contentType.startsWith("multipart/form-data"));
      assert.ok(upload, "multipart upload recorded");
      assert.ok(upload.raw.includes("payload_json") && upload.raw.includes("log attached"));
      const dl = await client2.request("tools/call", {
        name: "read_attachment",
        arguments: { url: `http://127.0.0.1:${restPort2}/cdn/notes.txt` },
      });
      assert.ok(!dl.result.isError, JSON.stringify(dl.result));
      const saved = JSON.parse(dl.result.content[0].text);
      assert.equal(await fsPromises.readFile(saved.path, "utf8"), "cdn-attachment-bytes");
      const refused = await client2.request("tools/call", {
        name: "read_attachment",
        arguments: { url: "http://evil.example/x" },
      });
      assert.ok(refused.result.isError);
      assert.ok(refused.result.content[0].text.includes("only fetches Discord CDN"));
      pass("send_file multipart upload and read_attachment round-trip with host allowlist");
    } finally {
      child2.kill("SIGKILL");
      gateway2.close();
      rest2.close();
    }

    // ── Phase 3: mention requirement OFF — addressed-awareness. ──
    const gateway3 = new MockGateway();
    const rest3 = new MockRest();
    const gwPort3 = await gateway3.listen();
    const restPort3 = await rest3.listen();
    const child3 = spawn(process.execPath, [BRIDGE], {
      env: {
        ...process.env,
        DISCORD_BOT_TOKEN: "test-token",
        DISCORD_ALLOWED_USER_IDS: "111",
        DISCORD_REQUIRE_MENTION: "false",
        DISCORD_MENTION_WINDOW_SECONDS: "0",
        DISCORD_GATEWAY_URL: `ws://127.0.0.1:${gwPort3}`,
        DISCORD_API_BASE: `http://127.0.0.1:${restPort3}`,
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    const client3 = new BridgeClient(child3);
    try {
      await client3.request("initialize", {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "e2e-test-3", version: "0.0.0" },
      });
      client3.notify("notifications/initialized", {});
      await gateway3.waitForConnection();
      gateway3.send({ op: 10, d: { heartbeat_interval: 200 } });
      await gateway3.waitForFrame((f) => f.op === 2, 5000, "IDENTIFY (bridge3)");
      gateway3.send({
        op: 0, s: 1, t: "READY",
        d: { session_id: "sess3", resume_gateway_url: "", user: { id: "999", username: "testbot" } },
      });
      await client3.waitForStderr("logged in as testbot");

      // 22. Open chatter flows with addressed="none"; @someone-else flows
      // with addressed="other"; @bot carries no addressed attribute.
      const room = { guild_id: "GUILD", channel_id: "G5" };
      gateway3.send({
        op: 0, s: 2, t: "MESSAGE_CREATE",
        d: { id: "N1", ...room, content: "thinking out loud", author: { id: "111", username: "karl" }, mentions: [] },
      });
      const n1 = await client3.waitForNotification();
      assert.equal(n1.params.meta.addressed, "none");
      gateway3.send({
        op: 0, s: 3, t: "MESSAGE_CREATE",
        d: { id: "N2", ...room, content: "<@555> your turn", author: { id: "111", username: "karl" }, mentions: [{ id: "555" }] },
      });
      const n2 = await client3.waitForNotification();
      assert.equal(n2.params.meta.addressed, "other");
      gateway3.send({
        op: 0, s: 4, t: "MESSAGE_CREATE",
        d: { id: "N3", ...room, content: "<@999> and you?", author: { id: "111", username: "karl" }, mentions: [{ id: "999" }] },
      });
      const n3 = await client3.waitForNotification();
      assert.equal(n3.params.meta.addressed, undefined);
      pass("mention-free mode marks addressed=none/other and leaves bot-directed unmarked");
    } finally {
      child3.kill("SIGKILL");
      gateway3.close();
      rest3.close();
    }

    // 15. Closing stdin shuts the bridge down cleanly.
    const exit = deferred();
    child.on("exit", (code) => exit.resolve(code));
    child.stdin.end();
    const code = await Promise.race([
      exit.promise,
      sleep(2000).then(() => {
        throw new Error("bridge did not exit within 2s of stdin closing");
      }),
    ]);
    assert.equal(code, 0);
    pass("bridge exits 0 within 2s when stdin closes");

    console.log(`\n${passed} steps passed.\nALL TESTS PASSED`);
  } finally {
    cleanup();
  }
}

const watchdog = setTimeout(() => {
  console.error("FAIL: overall test timeout reached");
  process.exit(1);
}, OVERALL_TIMEOUT_MS);
watchdog.unref();

main()
  .then(() => process.exit(0))
  .catch((err) => {
    console.error(`FAIL: ${err.stack || err.message}`);
    process.exit(1);
  });
