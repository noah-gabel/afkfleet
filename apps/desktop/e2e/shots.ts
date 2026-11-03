// The screenshot baselines' settings (Plan.md P11.11; ADR-0020, U97, U125, S130, S131, R226), shared
// by playwright.shots.config.ts, the specs and their unit tests. The baselines come only from
// Playwright's pinned Linux image (deploy/docker/shots.Dockerfile), so a Windows run can never write
// Windows-rendered ones: the config refuses to load anywhere else.

/** The Playwright project that shoots; every other run measures. */
export const SHOTS_PROJECT = "shots";

/** What the image sets, and nothing else does. */
export const IMAGE_MARKER = "AFKFLEET_SHOTS_IMAGE";

/** What scripts/shots.mjs sets for a before or an after: every shot written to target/design. */
export const DESIGN_MARKER = "AFKFLEET_SHOTS_DESIGN";

/**
 * The time the page and the fake read in the shots: after every fixed time the fake shows, but the
 * link code's 12:15, so "Works until" reads naturally (S131).
 */
export const NOW = "2026-10-13T12:10:00.000Z";

/** The sweep's own window, and the smallest the app allows (S101). */
export const WIDE = { width: 1280, height: 720 } as const;
export const SMALL = { width: 800, height: 600 } as const;

/**
 * How a shot is compared: not one pixel's colour may move. Playwright's default threshold, 0.2,
 * would pass `--muted-foreground` 40 units lighter (S131). pixelmatch still skips what it judges
 * anti-aliasing, which Playwright gives no way to turn off.
 */
export const COMPARE = {
  threshold: 0,
  animations: "disabled",
  caret: "hide",
  scale: "css",
} as const;

/** Why the shots can't run here, or undefined when this is the pinned image. */
export function refusal(
  platform: string,
  env: Readonly<Record<string, string | undefined>>,
): string | undefined {
  if (platform !== "linux" || env[IMAGE_MARKER] !== "1") {
    return "The screenshot baselines run only in Playwright's pinned Linux image: run `just shots`, or `just shots-update` to write them.";
  }
  return undefined;
}

/** A row's name as a file name: lowercase words joined by dashes. */
export function slug(row: string): string {
  return row
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}
