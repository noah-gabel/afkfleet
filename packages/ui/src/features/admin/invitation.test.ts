import { describe, expect, it } from "vitest";

import { formatTimeWithZone } from "../../lib/time";
import { invitationText } from "./invitation";

const CODE = "afk_iv_placeholderplaceholderplaceholder00";
const EXPIRES = "2026-10-16T08:00:00.000Z";

describe("invitationText", () => {
  it("names the server, the code and when it ends", () => {
    const text = invitationText({ origin: "https://example.com", code: CODE, expiresAt: EXPIRES });

    expect(text).toBe(
      [
        "You're invited to afkfleet.",
        "Server: https://example.com",
        `Invite code: ${CODE}`,
        `The code works once, until ${formatTimeWithZone(EXPIRES)}.`,
        "In the afkfleet app, enter the server, then register with the code.",
      ].join("\n"),
    );
  });

  it("asks for the CA file and names its fingerprints when the server uses its own CA", () => {
    const text = invitationText({
      origin: "https://example.com",
      code: CODE,
      expiresAt: EXPIRES,
      fingerprints: ["AB:CD", "EF:01"],
    });

    expect(text).toContain(
      [
        "The server uses its own certificate authority (CA). Ask me for the CA file, and check that its SHA-256 fingerprints are:",
        "AB:CD",
        "EF:01",
      ].join("\n"),
    );
    expect(
      text.endsWith(
        "In the afkfleet app, enter the server and the CA, then register with the code.",
      ),
    ).toBe(true);
  });
});
