// Why a saved server profile can't be used (Plan.md P8.7, ADR-0017). The Rust side's
// `ProfileUnreadable` ships with this UI, so every reason has its text here (a new one
// doesn't compile); none repeats the file's content.
import type { ProfileUnreadable } from "../../generated/ProfileUnreadable";

const REASONS: Record<ProfileUnreadable, string> = {
  io: "The saved server settings couldn't be read.",
  json: "The saved server settings are damaged.",
  version: "The saved server settings are from another version of the app.",
  url: "The saved server address is no longer valid.",
  ca: "The saved custom CA is no longer valid.",
  client: "The app can't connect with the saved settings.",
};

/** What server setup says about an unreadable profile. */
export function unreadableText(reason: ProfileUnreadable): string {
  return `${REASONS[reason]} Enter the server again.`;
}
