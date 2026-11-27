// What a bot sees (Plan.md P16.20), in Edge: Look asks its agent once, each filled slot shows as a
// script names it with its item's every field as text a person can select and copy, and a bot that
// isn't in a world says so in plain words.
import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

test("an online bot's screen shows slot by slot, its texts selectable, and an offline one says why", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const sight = page.getByRole("main").getByRole("region", { name: "What it sees" });

  await sight.getByRole("button", { name: "Look", exact: true }).click();

  await expect(sight.getByText("Auction House", { exact: true })).toBeVisible();
  const own = sight.getByRole("list", { name: "Its slots (menu.slots)" });
  await expect(own.getByText("Slot 4 · row 0, column 4 · menu.at(0, 4)")).toBeVisible();
  const name = own.getByText("Potion of the Turtle Master");
  await expect(name).toBeVisible();
  expect(await name.evaluate((element) => getComputedStyle(element).userSelect)).not.toBe("none");
  await name.selectText();
  expect(await page.evaluate(() => window.getSelection()?.toString())).toBe(
    "Potion of the Turtle Master",
  );
  await expect(sight.getByText("Empty: 0–3, 5–8")).toBeVisible();
  await expect(sight.getByRole("button", { name: "Look again" })).toBeEnabled();

  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Home" }).click();
  await page.getByRole("link", { name: "Open AfkBot2" }).click();
  await page
    .getByRole("main")
    .getByRole("region", { name: "What it sees" })
    .getByRole("button", { name: "Look", exact: true })
    .click();
  await expect(page.getByText("It isn't in a world, so there's nothing to see.")).toBeVisible();
});
