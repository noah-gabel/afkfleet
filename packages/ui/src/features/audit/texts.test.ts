// The audit log's texts (ADR-0020's table): each name's word, and the raw name for anything
// only a newer server records.
import { describe, expect, it } from "vitest";
import type { AuditActionName } from "../../generated/AuditActionName";
import generated from "../../generated/AuditActionName.ts?raw";
import type { AuditOutcomeName } from "../../generated/AuditOutcomeName";
import type { AuditTargetKind } from "../../generated/AuditTargetKind";
import { actionLabel, connectFailureLabel, outcomeLabel, targetKindLabel } from "./texts";

/** Every action this version knows, as the generated union lists it. */
const ACTIONS = [...generated.matchAll(/"([a-z0-9_.]+)"/g)].map(
  (match) => match[1] as AuditActionName,
);

describe("the audit log's texts", () => {
  it("has a word of its own for every action", () => {
    expect(ACTIONS.length).toBeGreaterThan(50);
    for (const action of ACTIONS) {
      expect(actionLabel(action), action).not.toBe(action);
    }
  });

  it.each<[AuditActionName, string]>([
    ["auth.login", "Login"],
    ["agent.connect", "Agent connection"],
    ["grant.revoke", "Access ended"],
    ["retention.purge", "Old data deleted"],
  ])("words %s as %s", (action, words) => {
    expect(actionLabel(action)).toBe(words);
  });

  it.each(["agent.teleport", "constructor", "__proto__", "toString"])(
    "shows %s, which it doesn't know, as the server sent it",
    (name) => {
      expect(actionLabel(name as AuditActionName)).toBe(name);
      expect(targetKindLabel(name as AuditTargetKind)).toBe(name);
      expect(outcomeLabel(name as AuditOutcomeName)).toBe(name);
      expect(connectFailureLabel(name)).toBe(name);
    },
  );

  it.each<[AuditOutcomeName, string]>([
    ["success", "Done"],
    ["failure", "Failed"],
    ["denied", "Denied"],
  ])("words the outcome %s as %s", (outcome, words) => {
    expect(outcomeLabel(outcome)).toBe(words);
  });

  it.each<[AuditTargetKind, string]>([
    ["user", "User"],
    ["account", "Account"],
    ["bot", "Bot"],
    ["agent", "Agent"],
    ["mode", "Mode"],
    ["session", "Session"],
    ["invite", "Invite"],
    ["enrollment_token", "Enrollment token"],
    ["server_settings", "Server settings"],
  ])("words the target %s as %s", (kind, words) => {
    expect(targetKindLabel(kind)).toBe(words);
  });

  it.each([
    ["revoked", "Its certificate was revoked"],
    ["expired", "Its certificate expired"],
    ["disabled", "The agent is disabled"],
    ["san_mismatch", "Its certificate names a different agent"],
    ["name_mismatch", "It gave another name than it enrolled with"],
  ])("words the connect failure %s as %s", (reason, words) => {
    expect(connectFailureLabel(reason)).toBe(words);
  });
});
