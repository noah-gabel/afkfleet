import { describe, expect, it } from "vitest";

import {
  AT_MOST_TEXTS,
  added,
  CAP_REACHED,
  chatFormatCount,
  conflictTextCount,
  conflictTextLabel,
  deleted,
  deleteQuestion,
  openServer,
  serversFailed,
  settingsCount,
} from "./texts";

describe("the Minecraft servers' texts", () => {
  it.each([
    [1, "1 conflict text"],
    [2, "2 conflict texts"],
    [16, "16 conflict texts"],
  ])("counts %i text(s) as %s", (count, words) => {
    expect(conflictTextCount(count)).toBe(words);
  });

  it.each([
    [1, "1 chat format"],
    [2, "2 chat formats"],
    [8, "8 chat formats"],
  ])("counts %i format(s) as %s", (count, words) => {
    expect(chatFormatCount(count)).toBe(words);
  });

  it.each([
    [3, 0, "3 conflict texts"],
    [0, 2, "2 chat formats"],
    [1, 1, "1 conflict text · 1 chat format"],
    [16, 8, "16 conflict texts · 8 chat formats"],
  ])(
    "shows an entry of %i text(s) and %i format(s) as %s, only the parts it has",
    (texts, formats, words) => {
      expect(settingsCount(texts, formats)).toBe(words);
    },
  );

  it("names each text from 1 and each entry by its server", () => {
    expect(conflictTextLabel(1)).toBe("Conflict text 1");
    expect(openServer("mc.example.com:25565")).toBe("Open mc.example.com:25565");
    expect(added("mc.example.com:25565")).toBe("Added settings for mc.example.com:25565.");
    expect(deleted("mc.example.com:25565")).toBe("Deleted the settings of mc.example.com:25565.");
    expect(deleteQuestion("mc.example.com:25565")).toBe(
      "Delete the settings of mc.example.com:25565? Its bots then treat no kick message as someone logging in.",
    );
    expect(serversFailed("why")).toBe("Couldn't load the servers: why");
  });

  it("says the server's caps as numbers", () => {
    expect(AT_MOST_TEXTS).toBe("At most 16 texts.");
    expect(CAP_REACHED).toBe(
      "There are settings for 100 servers, the most there can be. Delete a server's settings to add another.",
    );
  });
});
