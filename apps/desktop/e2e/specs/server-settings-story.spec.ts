// The Minecraft servers' settings as one story (Plan.md P11.9; ADR-0020, U57, U61, R136): the
// Owner adds a server with a two-line conflict text after "Confirm it's you" and lands on its
// screen, finds it listed, adds a second text and saves without being asked again, then
// deletes it, answering the question, and the list is empty again.

import { OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

const SERVER = "mc.example.com:25565";

test("the Owner adds a server's conflict texts, edits them and deletes them", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");
  const entry = main.getByRole("listitem").filter({ hasText: SERVER });

  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Minecraft servers" })
    .click();
  await expect(main.getByText("No server has settings yet.")).toBeVisible();
  await main.getByRole("link", { name: "Add a server" }).click();
  await page.getByLabel("Server address").fill("MC.example.com");
  await page.getByLabel("Conflict text 1").fill("You logged in\nfrom another location");
  await main.getByRole("button", { name: "Save" }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText(`Added settings for ${SERVER}.`)).toBeVisible();
  await expect(main.getByRole("heading", { name: SERVER, level: 1 })).toBeVisible();
  await expect(page.getByLabel("Conflict text 1")).toHaveValue(
    "You logged in\nfrom another location",
  );

  await main.getByRole("link", { name: "Minecraft servers" }).click();
  await expect(entry.getByText("1 conflict text")).toBeVisible();
  await entry.getByRole("link", { name: `Open ${SERVER}` }).click();
  await main.getByRole("button", { name: "Add text" }).click();
  await page.getByLabel("Conflict text 2").fill("Already connected");
  await main.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Saved. Bots on this server use the change at once.")).toBeVisible();
  // The step-up still holds, and the server answers its texts in its own order.
  await expect(page.getByLabel("Current password")).toHaveCount(0);
  await expect(page.getByLabel("Conflict text 1")).toHaveValue("Already connected");
  await expect(page.getByLabel("Conflict text 2")).toHaveValue(
    "You logged in\nfrom another location",
  );

  await main.getByRole("link", { name: "Minecraft servers" }).click();
  await expect(entry.getByText("2 conflict texts")).toBeVisible();
  await entry.getByRole("link", { name: `Open ${SERVER}` }).click();
  await main.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(
    main.getByText(
      `Delete the settings of ${SERVER}? Its bots then treat no kick message as someone logging in.`,
    ),
  ).toBeVisible();
  await main.getByRole("button", { name: "Delete settings" }).click();

  await expect(page.getByText(`Deleted the settings of ${SERVER}.`)).toBeVisible();
  await expect(main.getByText("No server has settings yet.")).toBeVisible();
});
