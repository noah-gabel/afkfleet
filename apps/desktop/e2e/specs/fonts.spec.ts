// The bundled fonts, in Edge under the release CSP (Plan.md P11.11; ADR-0020, U93, S111): each face
// loads from the bundle, and what's on screen is drawn in it. Computed `font-family` proves only
// what was asked for, so this asks the browser which fonts it actually used.

import type { Page } from "@playwright/test";

import { NEW_USER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { logIn } from "./steps";

/** What the browser drew the first `selector` with: each family, and whether it's a web font. */
async function drawnWith(page: Page, selector: string) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("DOM.enable");
  await cdp.send("CSS.enable");
  const { root } = await cdp.send("DOM.getDocument");
  const { nodeId } = await cdp.send("DOM.querySelector", { nodeId: root.nodeId, selector });
  const { fonts } = await cdp.send("CSS.getPlatformFontsForNode", { nodeId });
  await cdp.detach();
  return fonts.map((font) => ({ family: font.familyName, custom: font.isCustomFont }));
}

test("each face loads from the bundle", async ({ page, context }) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();

  for (const face of [
    '400 16px "Geist Variable"',
    '600 16px "Geist Variable"',
    '700 16px "Geist Variable"',
    '400 16px "IBM Plex Mono"',
    '600 16px "IBM Plex Mono"',
  ]) {
    const statuses = await page.evaluate(
      async (font) => (await document.fonts.load(font, "a")).map((loaded) => loaded.status),
      face,
    );
    expect(statuses, face).toEqual(["loaded"]);
  }
});

test("the page, a mono cell and Cyrillic are drawn in the bundled fonts", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);

  expect(await drawnWith(page, "h1")).toEqual([{ family: "Geist", custom: true }]);
  expect(await drawnWith(page, "button[type=submit]")).toEqual([{ family: "Geist", custom: true }]);

  await page.evaluate(async () => {
    const mono = document.createElement("div");
    mono.id = "planted-mono";
    mono.className = "font-mono";
    mono.textContent = "AfkBot1 192.0.2.10:25565";
    const cyrillic = document.createElement("div");
    cyrillic.id = "planted-cyrillic";
    cyrillic.textContent = "Привет";
    const monoCyrillic = document.createElement("div");
    monoCyrillic.id = "planted-mono-cyrillic";
    monoCyrillic.className = "font-mono";
    monoCyrillic.textContent = "Привет";
    document.body.append(mono, cyrillic, monoCyrillic);
    await document.fonts.ready;
    await document.fonts.load('400 16px "Geist Variable"', "Привет");
    await document.fonts.load('400 16px "IBM Plex Mono"', "Привет");
  });
  const plex = [{ family: "IBM Plex Mono", custom: true }];
  expect(await drawnWith(page, "#planted-mono")).toEqual(plex);
  expect(await drawnWith(page, "#planted-mono-cyrillic")).toEqual(plex);
  expect(await drawnWith(page, "#planted-cyrillic")).toEqual([{ family: "Geist", custom: true }]);
  await page.evaluate(() => {
    for (const id of ["planted-mono", "planted-cyrillic", "planted-mono-cyrillic"]) {
      document.getElementById(id)?.remove();
    }
  });
});

test("a toast is drawn in the bundled font, though sonner sets its own", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, NEW_USER);
  await expect(page.getByRole("img", { name: /QR code/ })).toBeVisible();
  await page.getByLabel("Code").fill(backend.setupCode());
  await page
    .getByRole("button", { name: "Confirm" })
    .click({ timeout: 1_000 })
    .catch(() => {});
  await page.getByRole("button", { name: "Copy all" }).click();
  const toast = page.locator("[data-sonner-toast] [data-title]");
  await expect(toast).toBeVisible();
  await toast.hover();
  await page.evaluate(() => document.fonts.ready);

  expect(await drawnWith(page, "[data-sonner-toast] [data-title]")).toEqual([
    { family: "Geist", custom: true },
  ]);
});
