import { describe, expect, it } from "vitest";

import type { ModeOwnership } from "../../generated/ModeOwnership";
import type { ModeVisibility } from "../../generated/ModeVisibility";
import { CAP_REACHED, copyName, visibilityText, whoseText } from "./texts";

describe("whoseText", () => {
  const OWNER = { id: "01900000-0000-7000-8000-000000000001", username: "afkadmin" };

  it.each<[ModeOwnership, string]>([
    ["built_in", "Built in"],
    ["mine", "Yours"],
    ["someone_else", "Someone else's"],
    ["a_new_kind" as ModeOwnership, "Someone else's"],
  ])("reads %s without an owner as %s", (ownership, text) => {
    expect(whoseText(ownership, undefined)).toBe(text);
  });

  it("names the owner only where the server sent one, and never on a viewer's own", () => {
    expect(whoseText("someone_else", OWNER)).toBe("afkadmin");
    expect(whoseText("mine", OWNER)).toBe("Yours");
    expect(whoseText("built_in", OWNER)).toBe("Built in");
  });
});

describe("visibilityText", () => {
  it.each<[ModeVisibility, string]>([
    ["private", "Private"],
    ["shared", "Shared"],
    ["secret" as ModeVisibility, "secret"],
  ])("reads %s as %s", (visibility, text) => {
    expect(visibilityText(visibility)).toBe(text);
  });
});

describe("copyName", () => {
  it("adds the suffix", () => {
    expect(copyName("afk")).toBe("afk copy");
  });

  it("shortens a long name to fit 32 characters, counting characters", () => {
    expect(copyName("x".repeat(32))).toBe(`${"x".repeat(27)} copy`);
    expect([...copyName("日".repeat(32))]).toHaveLength(32);
    expect(copyName("日".repeat(32))).toBe(`${"日".repeat(27)} copy`);
  });

  it("leaves no double space where it cut after a space", () => {
    expect(copyName(`${"x".repeat(26)} yyyyy`)).toBe(`${"x".repeat(26)} copy`);
  });
});

describe("the cap's text", () => {
  it("names S19's limit", () => {
    expect(CAP_REACHED).toBe("Mode limit reached (50). Delete one to add another.");
  });
});
