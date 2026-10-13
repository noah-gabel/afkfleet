// The invitation an Admin sends (Plan.md P8.8, ADR-0017): what "Copy invitation" copies, so
// the invitee has everything the app asks for: the server's address, the code (shown once)
// and when it ends, and with a custom CA the CA's fingerprints to check the file against.
// The CA file itself never reaches the UI, so the invitee asks for it.
import { formatTimeWithZone } from "../../lib/time";

/** What the invitation names. */
export interface Invitation {
  /** The server's address, as the app shows it. */
  readonly origin: string;
  readonly code: string;
  readonly expiresAt: string;
  /** The custom CA's SHA-256 fingerprints, when the server uses one. */
  readonly fingerprints?: readonly string[];
}

/** The text "Copy invitation" copies. */
export function invitationText(invitation: Invitation): string {
  const ca = invitation.fingerprints;
  const lines = [
    "You're invited to afkfleet.",
    `Server: ${invitation.origin}`,
    `Invite code: ${invitation.code}`,
    `The code works once, until ${formatTimeWithZone(invitation.expiresAt)}.`,
  ];
  if (ca === undefined) {
    lines.push("In the afkfleet app, enter the server, then register with the code.");
  } else {
    lines.push(
      `The server uses its own certificate authority (CA). Ask me for the CA file, and check that its SHA-256 ${ca.length === 1 ? "fingerprint is" : "fingerprints are"}:`,
      ...ca,
      "In the afkfleet app, enter the server and the CA, then register with the code.",
    );
  }
  return lines.join("\n");
}
