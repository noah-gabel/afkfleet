import { describe, expect, it } from "vitest";

import type { AccountKind } from "../../generated/AccountKind";
import type { AccountStatus } from "../../generated/AccountStatus";
import type { GrantLevel } from "../../generated/GrantLevel";
import type { RefusalReason } from "../../generated/RefusalReason";
import {
  accessLabel,
  badgeNumber,
  kindLabel,
  LINK_WARNING,
  reasonText,
  statusLabel,
} from "./texts";

const REASONS: RefusalReason[] = [
  "not_owned",
  "no_profile",
  "xbox_banned",
  "xbox_no_xbox_profile",
  "xbox_country_unavailable",
  "xbox_adult_verification",
  "xbox_child_account",
  "xbox_other",
  "profile_mismatch",
];

describe("the account texts", () => {
  it("show a value a newer server sends as it came", () => {
    expect(kindLabel("bedrock" as AccountKind)).toBe("bedrock");
    expect(statusLabel("suspended" as AccountStatus)).toBe("suspended");
    expect(accessLabel("own" as GrantLevel)).toBe("own");
    expect(reasonText("xbox_new" as RefusalReason, "AfkBot1")).toBe("Refused (xbox_new).");
  });

  it("give every refusal reason a text of its own", () => {
    const texts = new Set(REASONS.map((reason) => reasonText(reason, "AfkBot1")));

    expect(texts.size).toBe(REASONS.length);
    expect(reasonText("profile_mismatch", "AfkBot1")).toContain("AfkBot1");
  });

  it("stop the badge at 99+", () => {
    expect(badgeNumber(1)).toBe("1");
    expect(badgeNumber(99)).toBe("99");
    expect(badgeNumber(100)).toBe("99+");
  });

  it("warn on the link screen in the user's own words (ADR-0018)", () => {
    expect(LINK_WARNING).toBe(
      "Only enter a code you started yourself here. Never enter a code someone sent you, it would hand them your Minecraft account.",
    );
  });
});
