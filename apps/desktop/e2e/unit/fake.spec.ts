// The fake backend against the command contract (Plan.md P8.9): it answers exactly the
// commands src-tauri/tests/ipc-contract.json lists, the ones the Rust side registers.
import { readFileSync } from "node:fs";

import type { BotPage } from "@afkfleet/ui/generated/BotPage";
import type { ModePage } from "@afkfleet/ui/generated/ModePage";
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
