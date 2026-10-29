// A parsed sender as one story (Plan.md P11.9; ADR-0020, U55, U58, S79, R147, R148): the Owner
// adds a server with a chat format and no conflict text, tries a line on it, and saves after
// "Confirm it's you"; a bot on that server shows its server's line under the name the format
// read, with the mark, in the normal style. Deleting the settings shows the line as the
// server's again, muted, at the bot's next read: nothing parsed is stored.

import { OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

const SERVER = "mc.example.com:25565";
const MARK = "Read from the server's text by a chat format. Minecraft doesn't check this name.";

test("a format read from the server's line marks its sender until the format is deleted", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", {
    serverAddress: "mc.example.com",
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  backend.addChat("AfkBot1", {
    at: new Date().toISOString(),
    direction: "in",
    kind: "system",
    text: "[Clan] Notch : hello",
    truncated: false,
    arguments_hidden: false,
    mine: false,
  });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");
  const nav = page.getByRole("navigation", { name: "Main" });
  const formats = main.getByRole("group", { name: "Chat formats" });
  const line = main
    .getByRole("region", { name: "Chat" })
    .getByRole("listitem")
    .filter({ hasText: "[Clan] Notch : hello" });

  await nav.getByRole("link", { name: "Minecraft servers" }).click();
  await main.getByRole("link", { name: "Add a server" }).click();
  await page.getByLabel("Server address").fill("mc.example.com");
  await main
    .getByRole("group", { name: "Conflict texts" })
    .getByRole("button", { name: "Remove" })
    .click();
  await formats.getByRole("button", { name: "Add format" }).click();
  await page.getByLabel("Chat format 1").fill("[{any}] {sender} : ");
  await page.getByLabel("Try a line").fill("[Clan] Notch : hello");
  await formats.getByRole("button", { name: "Try" }).click();
  await expect(formats.getByRole("status")).toHaveText("Format 1 reads the sender as Notch.");
  await main.getByRole("button", { name: "Save" }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText(`Added settings for ${SERVER}.`)).toBeVisible();

  await nav.getByRole("link", { name: "Home" }).click();
  await main.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(line.getByText("Notch", { exact: true })).toBeVisible();
  await expect(line.getByTitle(MARK)).toBeVisible();
  await expect(line).not.toHaveClass(/italic/);

  await nav.getByRole("link", { name: "Minecraft servers" }).click();
  await main.getByRole("link", { name: `Open ${SERVER}` }).click();
  await main.getByRole("button", { name: "Delete", exact: true }).click();
  await main.getByRole("button", { name: "Delete settings" }).click();
  await expect(page.getByText(`Deleted the settings of ${SERVER}.`)).toBeVisible();
  await nav.getByRole("link", { name: "Home" }).click();
  await main.getByRole("link", { name: "Open AfkBot1" }).click();

  await expect(line.getByText("Server", { exact: true })).toBeVisible();
  await expect(line.getByTitle(MARK)).toHaveCount(0);
  await expect(line).toHaveClass(/italic/);
});
