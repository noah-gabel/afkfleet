import { describe, expect, it } from "vitest";

import {
  AT_MOST_TEXTS,
  added,
  CAP_REACHED,
  conflictTextLabel,
  deleted,
  deleteQuestion,
  openServer,
  serversFailed,
  settingsFacts,
} from "./texts";

describe("the Minecraft servers' texts", () => {
  it.each<[number, number, string[][]]>([
    [3, 0, [["Conflict texts", "3"]]],
    [0, 2, [["Chat formats", "2"]]],
    [
      1,
      1,
      [
        ["Conflict texts", "1"],
        ["Chat formats", "1"],
      ],
    ],
    [
      16,
      8,
      [
        ["Conflict texts", "16"],
        ["Chat formats", "8"],
      ],
    ],
  ])(
    "labels an entry of %i text(s) and %i format(s), only the parts it has",
    (texts, formats, facts) => {
      expect(settingsFacts(texts, formats).map((fact) => [fact.term, fact.value])).toEqual(facts);
    },
  );

  it("names each text from 1 and each entry by its server", () => {
    expect(conflictTextLabel(1)).toBe("Conflict text 1");
    expect(openServer("mc.example.com:25565")).toBe("Open mc.example.com:25565");
    expect(added("mc.example.com:25565")).toBe("Added mc.example.com:25565.");
    expect(deleted("mc.example.com:25565")).toBe("Deleted mc.example.com:25565's settings.");
    expect(deleteQuestion("mc.example.com:25565")).toBe(
      "Delete mc.example.com:25565's settings? Its bots then won't recognise any kick message as someone logging in.",
    );
    expect(serversFailed("why")).toBe("Couldn't load servers: why");
  });

  it("says the server's caps as numbers", () => {
    expect(AT_MOST_TEXTS).toBe("At most 16 texts.");
    expect(CAP_REACHED).toBe("Server limit reached (100). Delete one to add another.");
  });
});
