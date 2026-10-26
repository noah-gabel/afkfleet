// The fake backend against the command contract (Plan.md P8.9): it answers exactly the
// commands src-tauri/tests/ipc-contract.json lists, the ones the Rust side registers.
import { readFileSync } from "node:fs";

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
