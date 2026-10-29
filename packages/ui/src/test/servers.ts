// Answers the Minecraft servers' screens' tests script their FakeApiClient with (Plan.md
// P11.9). Placeholders only (CLAUDE.md).
import type { ServerSettingsResponse } from "../generated/ServerSettingsResponse";

/** Settings `n`'s ID. */
export function settingsId(n: number): string {
  return `01900000-0000-7000-8000-0000000002${String(n).padStart(2, "0")}`;
}

/**
 * Settings `n` for `server` (its key), each text given as its lines, at `version`, with the
 * chat formats `formats`.
 */
export function settings(
  n: number,
  server: string,
  texts: readonly (readonly string[])[],
  version = 1,
  formats: readonly string[] = [],
): ServerSettingsResponse {
  return {
    id: settingsId(n),
    server,
    conflict_texts: texts.map((lines) => ({ lines: [...lines] })),
    chat_formats: [...formats],
    version,
    created_at: "2026-10-28T08:00:00.000Z",
    updated_at: "2026-10-28T08:00:00.000Z",
  };
}
