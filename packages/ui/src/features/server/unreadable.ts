// Why a saved server profile can't be used (Plan.md P8.7, ADR-0017). The Rust side's
// `ProfileUnreadable` ships with this UI, so every reason has its text here (a new one
// doesn't compile); none repeats the file's content.
import type { ProfileUnreadable } from "../../generated/ProfileUnreadable";

const REASONS: Record<ProfileUnreadable, string> = {
  io: "Saved settings unreadable.",
  json: "Saved settings damaged.",
  version: "Saved settings from another app version.",
  url: "Saved address invalid.",
  ca: "Saved custom CA invalid.",
  client: "Can't connect with the saved settings.",
};

/** What server setup says about an unreadable profile. */
export function unreadableText(reason: ProfileUnreadable): string {
  return `${REASONS[reason]} Enter the server again.`;
}
