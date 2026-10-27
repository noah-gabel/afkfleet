// The bot screens' mirror of `authorize()` (ADR-0020, S67) against fleet-core's own
// authorization matrix, read through test/authzMatrix.ts: every cell of the bot tables, so a
// rule change on the server fails this test until permissions.ts follows.
import { describe, expect, it } from "vitest";

import type { GrantLevel } from "../../generated/GrantLevel";
import { checkGrantTable } from "../../test/authzMatrix";
import { mode } from "../../test/live";
import { botActions, mayChoose } from "./permissions";

describe("bot permissions", () => {
  it("leave every level the server reports able to see the bot, and View nothing more", () => {
    checkGrantTable("### ViewBot", () => true);
    expect(botActions("view")).toEqual({
      control: false,
      chooseMode: false,
      chooseAnyMode: false,
      configure: false,
      chat: false,
      command: false,
    });
  });

  it("offer the lifecycle controls exactly where the server lets the viewer control the bot", () => {
    checkGrantTable("### ControlBot", (level) => botActions(level).control);
  });

  it("offer a mode exactly where the server lets the viewer choose it, by its flag", () => {
    checkGrantTable(
      "### SetBotMode: no command off the allowlist",
      (level) => botActions(level).chooseMode,
    );
    checkGrantTable(
      "### SetBotMode: a command off the allowlist",
      (level) => botActions(level).chooseAnyMode,
    );
  });

  it("offer the server address exactly where the server lets the viewer configure the bot", () => {
    checkGrantTable("### ConfigureBot", (level) => botActions(level).configure);
  });

  it("offer chat and commands exactly where the server takes them", () => {
    checkGrantTable(
      "### SendChat: plain chat or an allowlisted command",
      (level) => botActions(level).chat,
    );
    checkGrantTable(
      "### SendChat: a command off the allowlist",
      (level) => botActions(level).command,
    );
  });

  it("let a mode be chosen by its flag: an unlisted command needs the right to choose any", () => {
    const listed = mode(1);
    const unlisted = mode(2, { unlisted_command: true });

    expect([
      mayChoose(listed, botActions("control")),
      mayChoose(unlisted, botActions("control")),
    ]).toEqual([true, false]);
    expect([
      mayChoose(listed, botActions("manage")),
      mayChoose(unlisted, botActions("manage")),
    ]).toEqual([true, true]);
    expect(mayChoose(listed, botActions("view"))).toBe(false);
  });

  it("give an unknown level nothing", () => {
    expect(Object.values(botActions("owner" as GrantLevel)).some(Boolean)).toBe(false);
  });
});
