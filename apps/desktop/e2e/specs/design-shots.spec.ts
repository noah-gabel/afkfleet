// Screenshots of L2's screens in both themes, from the fake's placeholder data, for the user to
// look at (Plan.md P11.11; ADR-0020, group L2). They're no test: the spec runs only when
// AFK_DESIGN_SHOTS names a folder, and L4 replaces it with the baselines (U97).
//
//   AFK_DESIGN_SHOTS=target/design-l2/after pnpm --filter @afkfleet/desktop exec playwright test design-shots

import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import type { Page } from "@playwright/test";

import { MEMBER, NEW_USER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { inScheme } from "./theme";

const FOLDER = process.env.AFK_DESIGN_SHOTS;
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..");

test.skip(FOLDER === undefined, "only when AFK_DESIGN_SHOTS names a folder");
test.setTimeout(120_000);

/** What's on the page in both themes, as `name-light.png` and `name-dark.png`. */
async function shoot(page: Page, name: string): Promise<void> {
  for (const scheme of ["light", "dark"] as const) {
    await inScheme(page, scheme);
    await page.screenshot({
      path: path.join(ROOT, FOLDER ?? "", `${name}-${scheme}.png`),
      fullPage: true,
    });
  }
}

test("before signing in", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.setViewportSize({ width: 1200, height: 800 });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await shoot(page, "login");

  await page.getByRole("link", { name: "Register with an invite" }).click();
  await expect(page.getByRole("heading", { name: "Register" })).toBeVisible();
  await shoot(page, "register");
  await page.goto("/");

  await page.getByRole("link", { name: "Server settings" }).click();
  await expect(page.getByLabel(/address/i).first()).toBeVisible();
  await shoot(page, "server-settings");
  await page.goto("/");

  await logIn(page, OWNER);
  await enterCode(page, "000000");
  await expect(page.getByText("Wrong code. 2 tries left.")).toBeVisible();
  await shoot(page, "code-step");
  await page.goto("/");

  await logIn(page, NEW_USER);
  await expect(page.getByRole("img", { name: /QR code/ })).toBeVisible();
  await shoot(page, "2fa-setup");
  await enterCode(page, backend.setupCode());
  await expect(page.getByRole("list", { name: "Recovery codes" })).toBeVisible();
  await shoot(page, "recovery-codes");

  await page.setViewportSize({ width: 800, height: 600 });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await shoot(page, "login-small");
});

test("signed in", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  backend.setBot("AfkBot2", {
    desiredState: "running",
    status: { kind: "reported", state: "failed" },
  });
  await page.setViewportSize({ width: 1200, height: 800 });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await shoot(page, "frame-home");

  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Profile" })
    .click();
  await expect(page.getByRole("group", { name: "Theme" })).toBeVisible();
  await shoot(page, "profile");

  await page.setViewportSize({ width: 900, height: 600 });
  await shoot(page, "profile-small");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Home" }).click();
  await expect(page.getByRole("heading", { name: "Bots" })).toBeVisible();
  await shoot(page, "frame-home-small");

  backend.refuseAll("unreachable");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await expect(
    page
      .getByRole("status")
      .getByText(/server/i)
      .first(),
  ).toBeVisible();
  await shoot(page, "banner-small");
  backend.refuseAll(null);
});

test("the member's frame and the start and error screens", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.setViewportSize({ width: 1200, height: 800 });
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  await shoot(page, "frame-member");

  backend.refuseAll("unreachable");
  await page.reload();
  await expect(page.getByText("Waiting for the server…")).toBeVisible();
  await shoot(page, "start-waiting");
  backend.refuseAll(null);

  // The panel renders no App, so nothing sets the theme's class but the media query at its load.
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/?panel=error");
    await expect(page.getByRole("button", { name: "Restart the app" })).toBeVisible();
    await page.screenshot({
      path: path.join(ROOT, FOLDER ?? "", `error-panel-${scheme}.png`),
      fullPage: true,
    });
  }
});
