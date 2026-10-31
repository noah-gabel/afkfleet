// P11.9's admin screens as stories (ADR-0020, U76-U81, S49-S52, S85-S89): the Owner disables an
// agent for good, releases its bots on their word and reads both in the audit log, narrowed to
// that agent; the log shows the server's own entry and an unreadable row; the Owner promotes a
// Member and reviews their grants, revoking one; and a Member leaves a shared account.

import { MEMBER, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

test("the Owner disables an agent, releases its bots and reads it in the audit log", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const agent = backend.addAgent("agent-throwaway", { bots: 2 });
  backend.addAgent("agent-main");
  backend.addAuditEntry({
    action: "retention.purge",
    metadata: [
      { key: "since", text: "2026-10-13T07:00:00.000Z" },
      { key: "chat_by_age", int: 12 },
    ],
  });
  const unreadable = backend.addUnreadableEntry();
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");

  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Agents" })
    .click();
  const row = main.getByRole("row", { name: /agent-throwaway/ }).first();
  await expect(row.getByText("Online")).toBeVisible();
  await expect(main.getByText(/create-enrollment-token/)).toBeVisible();

  await row.getByRole("button", { name: "Disable" }).click();
  await expect(main.getByText(/Disabling agent-throwaway is final/)).toBeVisible();
  await main.getByRole("button", { name: "Disable for good" }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText("agent-throwaway is disabled.")).toBeVisible();
  await expect(row.getByText("2 bots wait here until released.")).toBeVisible();

  await row.getByRole("button", { name: "Release bots" }).click();
  await expect(main.getByText(/you are vouching for that/)).toBeVisible();
  await main
    .getByText(/you are vouching for that/)
    .locator("..")
    .getByRole("button", {
      name: "Release bots",
    })
    .click();
  await expect(page.getByText("agent-throwaway's bots are released.")).toBeVisible();
  await expect(row.getByRole("button", { name: "Release bots" })).toBeHidden();

  await row.getByRole("link", { name: "Audit of agent-throwaway" }).click();
  await expect(main.getByText(`Showing only agent ${agent}.`)).toBeVisible();
  await expect(main.getByRole("row", { name: /Agent disabled/ })).toBeVisible();
  await expect(main.getByRole("row", { name: /Agent's bots released/ })).toBeVisible();
  await expect(main.getByText("The server", { exact: true })).toBeHidden();

  await main.getByRole("link", { name: "Show everything" }).click();
  await expect(
    main.getByRole("row", { name: /Old data deleted/ }).getByText("The server"),
  ).toBeVisible();
  await expect(
    main.getByText(`Entry ${unreadable} can't be read. The server's log says why.`),
  ).toBeVisible();
});

test("the Owner promotes a Member and reviews their grants, revoking one", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.share(backend.addAccount(OWNER), MEMBER, "view");
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");

  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Users" }).click();
  const row = main.getByRole("row", { name: new RegExp(`^${MEMBER}`) });
  await row.getByRole("button", { name: "Make admin" }).click();
  await main
    .getByText(/logs them out everywhere/)
    .locator("..")
    .getByRole("button", { name: "Make admin" })
    .click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();

  const offer = main.getByText(`${MEMBER} is now an Admin. Grants made before this stay.`);
  await expect(offer).toBeVisible();
  await offer.locator("..").getByRole("link", { name: "Review grants" }).click();

  await expect(main.getByRole("heading", { name: `Grants of ${MEMBER}`, level: 1 })).toBeVisible();
  const held = main.getByRole("region", { name: "Grants they hold" });
  await held.getByRole("button", { name: `Revoke ${MEMBER}'s access to AfkBot1` }).click();
  await held.getByRole("button", { name: "Revoke", exact: true }).click();
  await expect(held.getByText("They hold no grants.")).toBeVisible();
  await main.getByRole("link", { name: "Back to users" }).click();
  await expect(main.getByRole("heading", { name: "Users", level: 1 })).toBeVisible();
});

test("a Member leaves a shared account after the password", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.share(backend.addAccount(OWNER), MEMBER, "view");
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  const main = page.getByRole("main");

  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Accounts" })
    .click();
  await main.getByRole("link", { name: "Open AfkBot1" }).click();
  await main.getByRole("button", { name: "Leave", exact: true }).click();
  await expect(main.getByText(/You lose access to it and its bot/)).toBeVisible();
  await main.getByRole("button", { name: "Leave account" }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();

  await expect(page.getByText("You left AfkBot1.")).toBeVisible();
  await expect(main.getByRole("link", { name: "Open AfkBot1" })).toBeHidden();
  await expect(main.getByRole("heading", { name: "Accounts", level: 1 })).toBeVisible();
});
