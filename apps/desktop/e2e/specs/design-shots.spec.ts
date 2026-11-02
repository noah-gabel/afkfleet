// Screenshots of L3's screens in both themes, from the fake's placeholder data, for the user to
// look at (Plan.md P11.11; ADR-0020, group L3), run on the unchanged screens first and at the end. They're no test: the spec runs only when
// AFK_DESIGN_SHOTS names a folder, and L4 replaces it with the baselines (U97).
//
//   AFK_DESIGN_SHOTS=target/design-l3/after pnpm --filter @afkfleet/desktop exec playwright test design-shots

import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import type { Page } from "@playwright/test";

import { type FakeBackend, MEMBER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { inScheme } from "./theme";

const FOLDER = process.env.AFK_DESIGN_SHOTS;
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..");

test.skip(FOLDER === undefined, "only when AFK_DESIGN_SHOTS names a folder");
test.setTimeout(180_000);

const SINCE = "2026-10-26T12:00:00.000Z";

/**
 * What's on the page in both themes, as `name-light.png` and `name-dark.png`. The content scrolls
 * in its own column, so `fullPage` alone would stop at the window: the window grows to hold it.
 */
async function shoot(page: Page, name: string): Promise<void> {
  const before = page.viewportSize();
  if (before === null) {
    throw new Error("no viewport");
  }
  const tallest = await page.evaluate(() =>
    Math.max(
      document.documentElement.scrollHeight,
      ...[...document.querySelectorAll("main, main *")].map((element) =>
        element.parentElement === null ? 0 : element.parentElement.scrollHeight,
      ),
    ),
  );
  await page.setViewportSize({ width: before.width, height: Math.max(before.height, tallest) });
  for (const scheme of ["light", "dark"] as const) {
    await inScheme(page, scheme);
    await page.screenshot({
      path: path.join(ROOT, FOLDER ?? "", `${name}-${scheme}.png`),
      fullPage: true,
    });
  }
  await page.setViewportSize(before);
}

/** Signed in as `username`, on Home. */
async function signIn(page: Page, backend: FakeBackend, username: string): Promise<void> {
  await page.goto("/");
  await logIn(page, username);
  await enterCode(page, backend.code(username));
  await expectHome(page, username);
}

/** Opens `name` in the app's nav. */
async function nav(page: Page, name: string): Promise<void> {
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name }).click();
}

/** The Owner's fleet: a bot in each state a card shows. */
function fleet(backend: FakeBackend): void {
  for (let bots = 0; bots < 7; bots += 1) {
    backend.addAccount(OWNER);
  }
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "reported", state: "online", since: SINCE, online_since: SINCE, attempt: 1 },
  });
  backend.setBot("AfkBot2", {
    desiredState: "running",
    status: { kind: "reported", state: "connecting", since: SINCE, attempt: 1 },
  });
  backend.setBot("AfkBot3", {
    desiredState: "running",
    status: { kind: "reported", state: "paused", since: SINCE, reason: "duplicate_login" },
  });
  backend.setBot("AfkBot4", {
    desiredState: "running",
    status: { kind: "reported", state: "failed", since: SINCE, reason: "crash_loop" },
  });
  backend.setBot("AfkBot5", {
    desiredState: "stopped",
    status: { kind: "unknown", since: SINCE, agent_last_seen_at: SINCE },
  });
  backend.setBot("AfkBot6", { status: { kind: "unplaced", why: "no_server" } });
}

test("Home and a bot", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  fleet(backend);
  backend.addChat("AfkBot1", {
    at: new Date().toISOString(),
    direction: "in",
    kind: "system",
    text: "AfkBot1 joined the game",
    truncated: false,
    arguments_hidden: false,
    mine: false,
  });
  await page.setViewportSize({ width: 1200, height: 800 });
  await signIn(page, backend, OWNER);
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  await shoot(page, "home");
  await page.setViewportSize({ width: 800, height: 600 });
  await shoot(page, "home-small");

  await page.setViewportSize({ width: 1200, height: 800 });
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(page.getByText("AfkBot1 joined the game")).toBeVisible();
  await shoot(page, "bot");
  await page.setViewportSize({ width: 800, height: 600 });
  await shoot(page, "bot-small");
  await page.setViewportSize({ width: 1200, height: 800 });

  await nav(page, "Home");
  await page.getByRole("link", { name: "Open AfkBot5" }).click();
  await expect(page.getByRole("heading", { name: "AfkBot5", level: 1 })).toBeVisible();
  await shoot(page, "bot-waiting");
});

test("someone with nothing yet", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.setViewportSize({ width: 1200, height: 800 });
  await signIn(page, backend, MEMBER);
  await shoot(page, "home-empty");
  await nav(page, "Accounts");
  await expect(page.getByRole("main").getByRole("heading", { name: "Accounts" })).toBeVisible();
  await shoot(page, "accounts-empty");
  await page.setViewportSize({ width: 800, height: 600 });
  await shoot(page, "accounts-empty-small");
  await page.setViewportSize({ width: 1200, height: 800 });
  await page.getByRole("link", { name: /Link Microsoft account|Microsoft account/ }).click();
  await page.getByRole("button", { name: "Get a code" }).click();
  await expect(page.getByText("B7X4-KDLQ", { exact: true })).toBeVisible();
  await shoot(page, "link-code");
});

test("Accounts and an account", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "view");
  backend.addAccount(OWNER, "reauth_required");
  backend.addAccount(MEMBER, "pending");
  await page.setViewportSize({ width: 1200, height: 800 });
  await signIn(page, backend, OWNER);
  await nav(page, "Accounts");
  const open = page.getByRole("main").getByRole("link", { name: /^(Open )?AfkBot1$/ });
  await expect(open).toBeVisible();
  await shoot(page, "accounts");
  await page.setViewportSize({ width: 800, height: 600 });
  await shoot(page, "accounts-small");
  await page.setViewportSize({ width: 1200, height: 800 });

  await open.click();
  await expect(page.getByRole("table", { name: "Who has access" })).toBeVisible();
  await shoot(page, "account");
  await page.setViewportSize({ width: 800, height: 600 });
  await shoot(page, "account-small");
  await page.setViewportSize({ width: 1200, height: 800 });

  await page.getByRole("button", { name: "Log out" }).click();
  await signIn(page, backend, MEMBER);
  await nav(page, "Accounts");
  await page
    .getByRole("main")
    .getByRole("link", { name: /^(Open )?AfkBot1$/ })
    .click();
  await expect(page.getByRole("button", { name: "Leave" })).toBeVisible();
  await shoot(page, "account-view");
});
