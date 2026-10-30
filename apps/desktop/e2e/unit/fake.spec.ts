// The fake backend against the command contract (Plan.md P8.9): it answers exactly the
// commands src-tauri/tests/ipc-contract.json lists, the ones the Rust side registers.
import { readFileSync } from "node:fs";

import type { BotPage } from "@afkfleet/ui/generated/BotPage";
import type { ChatPage } from "@afkfleet/ui/generated/ChatPage";
import type { ModePage } from "@afkfleet/ui/generated/ModePage";
import type { ServerSettingsResponse } from "@afkfleet/ui/generated/ServerSettingsResponse";
import { expect, test } from "@playwright/test";

import { COMMANDS, FakeBackend, MEMBER, OWNER, PASSWORD } from "../fake/backend";

/** The built-in `farm` mode, as the server seeds it. */
const FARM = "01a14202-2800-7000-8000-000000000002";

const CONTRACT: { commands: { command: string }[] } = JSON.parse(
  readFileSync(new URL("../../src-tauri/tests/ipc-contract.json", import.meta.url), "utf8"),
);

test("answers exactly the contract's commands", () => {
  expect([...COMMANDS]).toEqual(CONTRACT.commands.map((entry) => entry.command));
});

test("answers every one of them", () => {
  const backend = new FakeBackend();

  for (const command of COMMANDS) {
    expect(() => backend.handle(command, {}), command).not.toThrow();
  }
});

test("lists each account's bot, newest first, and the built-in modes", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot2", { status: { kind: "reported", state: "online" } });

  const bots = (await backend.handle("bots", {})) as { ok: true; value: BotPage };
  const second = bots.value.items[0];
  const one = await backend.handle("bot", { id: second?.id });
  const missing = await backend.handle("bot", { id: "01900000-0000-7000-8000-0000000000ff" });
  const modes = (await backend.handle("modes", {})) as { ok: true; value: ModePage };

  expect(bots.value.items.map((bot) => bot.account.name)).toEqual(["AfkBot2", "AfkBot1"]);
  expect(second?.status).toEqual({ kind: "reported", state: "online" });
  expect(one).toEqual({ ok: true, value: second });
  expect(missing).toMatchObject({ ok: false, error: { api: { status: 404 } } });
  expect(modes.value.items.map((mode) => mode.name).sort()).toEqual(["afk", "farm", "warp"]);
});

test("drives a bot: its server after a step-up, its mode, and each lifecycle call", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.addAccount(OWNER);
  const id = backend.bot("AfkBot1").id;
  const address = { server_address: "127.0.0.1:25565" };

  const unstepped = await backend.handle("patch_bot", { id, request: address });
  backend.handle("step_up", { request: { password: PASSWORD } });
  const set = await backend.handle("patch_bot", { id, request: address });
  const farm = await backend.handle("set_bot_mode", { id, request: { mode_id: FARM } });
  const started = await backend.handle("start_bot", { id });
  backend.setBot("AfkBot1", { status: { kind: "reported", state: "online" } });
  const restarted = await backend.handle("restart_bot", { id });
  const notPaused = await backend.handle("resume_bot", { id });
  const stopped = await backend.handle("stop_bot", { id });
  backend.setBot("AfkBot1", {
    status: { kind: "unplaced", why: "stopped", state: "failed", reason: "crash_loop" },
  });
  const reset = await backend.handle("reset_bot", { id });

  expect(unstepped).toMatchObject({ ok: false, error: { api: { status: 403 } } });
  expect(set).toMatchObject({ ok: true, value: { server_address: "127.0.0.1:25565" } });
  expect(farm).toMatchObject({ ok: true, value: { mode_id: FARM } });
  expect(started).toMatchObject({
    ok: true,
    value: { desired_state: "running", status: { kind: "awaiting_report" } },
  });
  expect(restarted).toMatchObject({ ok: true, value: { effect: "sent" } });
  expect(notPaused).toMatchObject({
    ok: false,
    error: { api: { status: 409, code: "bot_not_paused" } },
  });
  expect(stopped).toMatchObject({ ok: true, value: { desired_state: "stopped" } });
  expect(reset).toMatchObject({
    ok: true,
    value: { effect: "cleared", bot: { status: { kind: "unplaced", why: "stopped" } } },
  });
});

test("lets only a manager choose a mode that sends a command off the allowlist", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.addAccount(OWNER);
  const id = backend.bot("AfkBot1").id;
  const modes = (await backend.handle("modes", {})) as { ok: true; value: ModePage };
  const unlisted = modes.value.items.find((mode) => mode.unlisted_command);

  const chosen = await backend.handle("set_bot_mode", { id, request: { mode_id: unlisted?.id } });

  expect(unlisted?.name).toBe("warp");
  expect(chosen).toMatchObject({ ok: true, value: { mode_id: unlisted?.id } });
});

test("refuses a Control holder that mode, and lets them choose a listed one", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.addAccount(OWNER);
  const shared = backend.bot("AfkBot1");
  backend.handle("step_up", { request: { password: PASSWORD } });
  const granted = await backend.handle("create_grant", {
    id: shared.account.id,
    request: { username: MEMBER, level: "control" },
  });
  backend.handle("logout", {});
  backend.handle("login", { request: { username: MEMBER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(MEMBER) });
  const modes = (await backend.handle("modes", {})) as { ok: true; value: ModePage };
  const unlisted = modes.value.items.find((mode) => mode.unlisted_command);

  const refused = await backend.handle("set_bot_mode", {
    id: shared.id,
    request: { mode_id: unlisted?.id },
  });
  const listed = await backend.handle("set_bot_mode", {
    id: shared.id,
    request: { mode_id: FARM },
  });

  expect(granted).toMatchObject({ ok: true, value: { level: "control" } });
  expect(refused).toMatchObject({ ok: false, error: { api: { status: 403, code: "forbidden" } } });
  expect(listed).toMatchObject({ ok: true, value: { mode_id: FARM } });
});

test("keeps each bot's chat: lines a page at a time, a line sent online, a command after a step-up", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.addAccount(OWNER);
  const id = backend.bot("AfkBot1").id;
  const said = (text: string) =>
    backend.addChat("AfkBot1", {
      at: "2026-10-27T12:00:00.000Z",
      direction: "in",
      kind: "system",
      text,
      truncated: false,
      arguments_hidden: false,
      mine: false,
    });
  const lines = [said("one"), said("two"), said("three")];

  const first = (await backend.handle("bot_chat", { id, limit: 2 })) as {
    ok: true;
    value: ChatPage;
  };
  const rest = await backend.handle("bot_chat", { id, before: first.value.next, limit: 2 });
  const offline = await backend.handle("send_chat", { id, request: { message: "hello" } });
  backend.setBot("AfkBot1", { status: { kind: "reported", state: "online" } });
  backend.chatOutcome = "pending";
  const sent = await backend.handle("send_chat", { id, request: { message: " hello " } });
  const pointed = await backend.handle("send_chat", { id, request: { message: "/op AfkBot2" } });
  const asked = await backend.handle("send_command", { id, request: { message: "/op AfkBot2" } });
  backend.handle("step_up", { request: { password: PASSWORD } });
  const command = await backend.handle("send_command", { id, request: { message: "/op AfkBot2" } });

  expect(first.value.items.map((line) => line.text)).toEqual(["three", "two"]);
  expect(first.value.next).toBe(lines[1]?.id);
  expect(rest).toEqual({ ok: true, value: { items: [lines[0]] } });
  expect(offline).toMatchObject({
    ok: false,
    error: { api: { status: 409, code: "bot_not_online" } },
  });
  expect(sent).toMatchObject({ ok: true, value: { outcome: "pending" } });
  expect(backend.lastSend?.line).toMatchObject({ kind: "user", text: "/op AfkBot2", mine: true });
  expect(pointed).toMatchObject({
    ok: false,
    error: { api: { status: 403, code: "command_needs_step_up" } },
  });
  expect(asked).toMatchObject({
    ok: false,
    error: { api: { status: 403, code: "step_up_required" } },
  });
  expect(command).toMatchObject({ ok: true, value: { request_id: backend.lastSend?.request } });
});

test("keeps the servers' settings: writes after a step-up, the key, the version, and 404", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  const request = {
    server: "MC.example.com",
    conflict_texts: ["b", "a\nlines", "b"],
    chat_formats: ["{sender} : ", "[{any}] {sender} : ", "{sender} : "],
  };

  const unstepped = await backend.handle("create_server_settings", { request });
  backend.handle("step_up", { request: { password: PASSWORD } });
  const created = (await backend.handle("create_server_settings", { request })) as {
    ok: true;
    value: ServerSettingsResponse;
  };
  const taken = await backend.handle("create_server_settings", { request });
  const id = created.value.id;
  const edit = {
    server: "mc.example.com:25565",
    conflict_texts: [],
    chat_formats: ["<{sender}> "],
  };
  const stale = await backend.handle("update_server_settings", {
    id,
    request: { version: 2, ...edit },
  });
  const edited = await backend.handle("update_server_settings", {
    id,
    request: { version: 1, ...edit },
  });
  const listed = await backend.handle("server_settings", {});
  const removed = await backend.handle("delete_server_settings", { id });
  const gone = await backend.handle("delete_server_settings", { id });

  expect(unstepped).toMatchObject({
    ok: false,
    error: { api: { status: 403, code: "step_up_required" } },
  });
  expect(created.value).toMatchObject({
    server: "mc.example.com:25565",
    conflict_texts: [{ lines: ["a", "lines"] }, { lines: ["b"] }],
    chat_formats: ["{sender} : ", "[{any}] {sender} : ", "{sender} : "],
    version: 1,
  });
  expect(taken).toMatchObject({
    ok: false,
    error: { api: { status: 422, fields: [{ path: "server", message: "already has settings" }] } },
  });
  expect(stale).toMatchObject({
    ok: false,
    error: { api: { status: 409, code: "server_settings_changed" } },
  });
  expect(edited).toMatchObject({
    ok: true,
    value: { version: 2, conflict_texts: [], chat_formats: ["<{sender}> "] },
  });
  expect(listed).toMatchObject({ ok: true, value: { items: [{ id, version: 2 }] } });
  expect(removed).toEqual({ ok: true });
  expect(gone).toMatchObject({ ok: false, error: { api: { status: 404 } } });
});

test("checks the cap before a taken server, as the server does", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.handle("step_up", { request: { password: PASSWORD } });
  for (let n = 0; n < 100; n += 1) {
    await backend.handle("create_server_settings", {
      request: { server: `mc${n}.example.com`, conflict_texts: ["a"], chat_formats: [] },
    });
  }

  const taken = await backend.handle("create_server_settings", {
    request: { server: "mc0.example.com", conflict_texts: ["a"], chat_formats: [] },
  });

  expect(taken).toMatchObject({ ok: false, error: { api: { status: 409, code: "conflict" } } });
});

test("refuses settings that hold neither a text nor a chat format", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.handle("step_up", { request: { password: PASSWORD } });
  const empty = { server: "mc.example.com", conflict_texts: [], chat_formats: [] };

  const refused = await backend.handle("create_server_settings", { request: empty });

  expect(refused).toMatchObject({
    ok: false,
    error: {
      api: {
        status: 422,
        fields: [
          {
            path: "conflict_texts",
            message: "must hold at least one text, unless a chat format is set",
          },
        ],
      },
    },
  });
});

test("checks an edit's cross-field rule before its lookup and version, as the server's extractor does", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.handle("step_up", { request: { password: PASSWORD } });
  const created = (await backend.handle("create_server_settings", {
    request: { server: "mc.example.com", conflict_texts: ["a"], chat_formats: [] },
  })) as { ok: true; value: ServerSettingsResponse };
  const empty = { server: "mc.example.com", conflict_texts: [], chat_formats: [] };

  const stale = await backend.handle("update_server_settings", {
    id: created.value.id,
    request: { version: 2, ...empty },
  });
  const unknown = await backend.handle("update_server_settings", {
    id: "01900000-0000-7000-8000-000000000999",
    request: { version: 1, ...empty },
  });

  for (const refused of [stale, unknown]) {
    expect(refused).toMatchObject({
      ok: false,
      error: { api: { status: 422, fields: [{ path: "conflict_texts" }] } },
    });
  }
});

test("tries chat formats on a line with a matcher of its own, for Admins and the Owner", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  const formats = ["<{sender}> ", "[{any}] {sender} : ", "{{{sender}}} "];
  const tried = (line: string) =>
    backend.handle("preview_chat_formats", { request: { chat_formats: formats, line } });

  expect(await tried("[Clan] Notch : hello")).toEqual({
    ok: true,
    value: { sender: "Notch", format: 1 },
  });
  expect(await tried("{Notch} hi")).toEqual({ ok: true, value: { sender: "Notch", format: 2 } });
  expect(await tried("[x] Admin : hi")).toEqual({
    ok: true,
    value: { sender: "Admin", format: 1 },
  });
  for (const line of [
    "Server restarting soon",
    "[Clan] No : hello",
    "[Clan] Seventeen_letters : hello",
    "[Clan]Notch : hello",
    `[${"x".repeat(33)}] Notch : hello`,
    "[Cl\nan] Notch : hello",
  ])
    expect(await tried(line), line).toEqual({ ok: true, value: {} });
  expect(await tried(`[${"x".repeat(32)}] Notch : hello`)).toEqual({
    ok: true,
    value: { sender: "Notch", format: 1 },
  });
});

test("reads a bot's system lines with its server's formats at each read, so a deleted one reads none", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  backend.handle("step_up", { request: { password: PASSWORD } });
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", { serverAddress: "MC.example.com:25565" });
  const id = backend.bot("AfkBot1").id;
  const said = (kind: "system" | "chat", text: string) =>
    backend.addChat("AfkBot1", {
      at: "2026-10-27T12:00:00.000Z",
      direction: "in",
      kind,
      text,
      truncated: false,
      arguments_hidden: false,
      mine: false,
      ...(kind === "chat" ? { sender: { name: "Steve" } } : {}),
    });
  said("system", "[Clan] Notch : hello");
  said("chat", "[Clan] Notch : hello");
  said("system", "Server restarting soon");
  const read = async () =>
    ((await backend.handle("bot_chat", { id })) as { ok: true; value: ChatPage }).value.items.map(
      (line) => line.parsed_sender,
    );
  const created = (await backend.handle("create_server_settings", {
    request: {
      server: "mc.example.com",
      conflict_texts: [],
      chat_formats: ["[{any}] {sender} : "],
    },
  })) as { ok: true; value: ServerSettingsResponse };

  const formatted = await read();
  await backend.handle("delete_server_settings", { id: created.value.id });
  const unformatted = await read();

  expect(formatted).toEqual([undefined, undefined, "Notch"]);
  expect(unformatted).toEqual([undefined, undefined, undefined]);
});

test("lets only Admins and the Owner try chat formats", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: MEMBER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(MEMBER) });

  const tried = await backend.handle("preview_chat_formats", {
    request: { chat_formats: ["{sender} : "], line: "Notch : hi" },
  });

  expect(tried).toMatchObject({ ok: false, error: { api: { status: 403, code: "forbidden" } } });
});

test("refuses a preview's format as the server does, at the format it names", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  const refused = (chat_formats: readonly string[]) =>
    backend.handle("preview_chat_formats", { request: { chat_formats, line: "Notch : hi" } });

  for (const [format, message] of [
    ["", "must not be empty"],
    ["x".repeat(65), "must be at most 64 characters"],
    [
      "{sender}\n: ",
      "must be one line, without control characters, formatting codes, text-direction marks or invisible characters",
    ],
    ["{Sender} : ", "can hold only {sender} and {any}; write a brace as {{ or }}"],
    ["no placeholder", "must hold {sender} exactly once"],
    ["{sender} {sender} : ", "must hold {sender} exactly once"],
    ["{sender} : {any}", "can hold {any} only before {sender}"],
    ["{any}a{any}b{any}c{sender} : ", "can hold {any} at most twice"],
    ["{any}{sender} : ", "must have text right after each {any}"],
    ["{sender}", "must have text after {sender} that doesn't start with a letter, a digit or _"],
    ["{sender}x", "must have text after {sender} that doesn't start with a letter, a digit or _"],
  ] as const) {
    expect(await refused(["{sender} : ", format])).toMatchObject({
      ok: false,
      error: {
        api: {
          status: 422,
          code: "validation_failed",
          fields: [{ path: "chat_formats[1]", message }],
        },
      },
    });
  }
  expect(await refused(Array.from({ length: 9 }, () => "{sender} : "))).toMatchObject({
    ok: false,
    error: { api: { fields: [{ path: "chat_formats", message: "must hold at most 8 formats" }] } },
  });
  expect(await refused(["{sender} : "])).toEqual({
    ok: true,
    value: { sender: "Notch", format: 0 },
  });
});

test("lets only Admins and the Owner read the servers' settings", async () => {
  const backend = new FakeBackend();
  backend.handle("login", { request: { username: MEMBER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(MEMBER) });

  const read = await backend.handle("server_settings", {});

  expect(read).toMatchObject({ ok: false, error: { api: { status: 403, code: "forbidden" } } });
});

test("refuses a command it doesn't know", () => {
  expect(() => new FakeBackend().handle("drop_tables", {})).toThrow();
});

test("holds a watch until a newer one starts or the session ends", async () => {
  const backend = new FakeBackend();
  const signedOut = await backend.handle("watch_events", {});
  backend.handle("login", { request: { username: OWNER, password: PASSWORD } });
  backend.handle("login_mfa", { code: backend.code(OWNER) });
  let settled = 0;
  const first = Promise.resolve(backend.handle("watch_events", {})).then((outcome) => {
    settled += 1;
    return outcome;
  });
  await Promise.resolve();

  expect(settled).toBe(0);
  const second = backend.handle("watch_events", {});
  expect(await first).toEqual({ ok: true });
  backend.handle("logout", {});
  expect(await second).toMatchObject({ ok: false, error: { code: "no_session" } });
  expect(signedOut).toMatchObject({ ok: false, error: { code: "no_session" } });
});
