// The fake backend against the command contract (Plan.md P8.9): it answers exactly the
// commands src-tauri/tests/ipc-contract.json lists, the ones the Rust side registers.
import { readFileSync } from "node:fs";

import type { BotPage } from "@afkfleet/ui/generated/BotPage";
import type { ModePage } from "@afkfleet/ui/generated/ModePage";
import { expect, test } from "@playwright/test";

import { COMMANDS, FakeBackend, OWNER, PASSWORD } from "../fake/backend";

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
  expect(modes.value.items.map((mode) => mode.name).sort()).toEqual(["afk", "farm"]);
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
