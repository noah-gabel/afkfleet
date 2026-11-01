// Dark mode as a story (Plan.md P11.10; ADR-0020, U84, S92, S94): the app follows Windows' setting
// live, without a restart; a choice on the Profile screen overrides it and survives a reload, the
// sign-in screens following it; and going back to Windows follows it again. Toasts follow too.

import type { Page } from "@playwright/test";

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";
import { inScheme } from "./theme";

/** Windows' setting moves to `scheme`; returns two frames after the page heard the change. */
async function windowsSays(page: Page, scheme: "light" | "dark"): Promise<void> {
  const heard = await page.evaluateHandle(() => ({
    done: new Promise<void>((resolve) => {
      matchMedia("(prefers-color-scheme: dark)").addEventListener(
        "change",
        () => {
          requestAnimationFrame(() => {
            requestAnimationFrame(() => {
              resolve();
            });
          });
        },
        { once: true },
      );
    }),
  }));
  await page.emulateMedia({ colorScheme: scheme });
  await heard.evaluate((waiting) => waiting.done);
}

test("the app follows Windows' setting live, its toasts too, without a restart", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  const html = page.locator("html");
  const background = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();

  await expect(html).toHaveClass(/\bdark\b/);
  expect(await background()).toBe("rgb(10, 10, 10)");

  await page.emulateMedia({ colorScheme: "light" });

  await expect(html).toHaveClass(/\blight\b/);
  await expect(html).not.toHaveClass(/\bdark\b/);
  expect(await background()).toBe("rgb(255, 255, 255)");

  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  await push(page, {
    update: "event",
    event: { type: "bot_died", bot_id: backend.bot("AfkBot1").id, at: new Date().toISOString() },
  });
  const toaster = page.locator("[data-sonner-toaster]");
  await page.getByText("AfkBot1 died.").hover();
  await expect(toaster).toHaveAttribute("data-sonner-theme", "light");

  await page.emulateMedia({ colorScheme: "dark" });

  await expect(html).toHaveClass(/\bdark\b/);
  await expect(toaster).toHaveAttribute("data-sonner-theme", "dark");
  await expectHome(page, OWNER);
});

test("a choice overrides Windows, survives a reload, and Windows takes over again", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const html = page.locator("html");
  const nav = page.getByRole("navigation", { name: "Main" });

  await nav.getByRole("link", { name: "Profile" }).click();
  const theme = page.getByRole("group", { name: "Theme" });
  await expect(theme.getByRole("radio", { name: "Windows" })).toBeChecked();
  await theme.getByRole("radio", { name: "Dark" }).check();
  await expect(html).toHaveClass(/\bdark\b/);

  // Windows' setting moves under the choice, which holds, once the page has heard each move.
  await windowsSays(page, "dark");
  await windowsSays(page, "light");
  await expect(html).toHaveClass(/\bdark\b/);

  // It survives a reload, and it's this PC's: the sign-in screens hold it too.
  await page.reload();
  await expectHome(page, OWNER);
  await expect(html).toHaveClass(/\bdark\b/);
  await page.getByRole("button", { name: "Log out" }).click();
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await expect(html).toHaveClass(/\bdark\b/);
  await expect(
    page.evaluate(() => getComputedStyle(document.documentElement).colorScheme),
  ).resolves.toBe("dark");

  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await nav.getByRole("link", { name: "Profile" }).click();
  await expect(theme.getByRole("radio", { name: "Dark" })).toBeChecked();
  await theme.getByRole("radio", { name: "Windows" }).check();

  await expect(html).toHaveClass(/\blight\b/);
  await inScheme(page, "dark");
});
