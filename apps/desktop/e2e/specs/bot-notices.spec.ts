// A bot's death as the live events bring it (Plan.md P11.9; ADR-0020, U42, U46, S71): a toast,
// a line on Home until its bot's screen is opened, and the bot's timeline.
import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";

test("a death toasts, waits on Home until its bot's screen opens, and shows in its timeline", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  const id = backend.bot("AfkBot1").id;

  await push(page, {
    update: "event",
    event: { type: "bot_died", bot_id: id, at: new Date().toISOString() },
  });

  await expect(page.getByText("AfkBot1 died.")).toBeVisible();
  const line = page.locator("[data-slot=notice-line]").filter({ hasText: "AfkBot1 died at" });
  await expect(line).toBeVisible();
  await line.getByRole("link", { name: "Open" }).click();
  const main = page.getByRole("main");
  await expect(main.getByRole("region", { name: "Timeline" }).getByText("Died")).toBeVisible();
  await main.getByRole("link", { name: "Home" }).click();
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  await expect(line).toHaveCount(0);
});
