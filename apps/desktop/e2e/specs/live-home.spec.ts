// The bots on Home and on a bot's screen follow the live events (Plan.md P11.9; ADR-0020,
// U35, R98): the fake changes the bot first, so REST answers what each pushed frame says.
import type { LiveUpdateMessage } from "@afkfleet/ui/generated/LiveUpdateMessage";

import { type FakeBackend, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";

const SINCE = "2026-10-26T12:00:00.000Z";

/** The `bot` frame of the bot of `name`, as the fake shows it now. */
function frame(backend: FakeBackend, name: string): LiveUpdateMessage {
  const bot = backend.bot(name);
  return { update: "event", event: { type: "bot", bot_id: bot.id, bot } };
}

test("a bot's card and its screen follow its frames, and a removed bot leaves Home", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const link = page.getByRole("link", { name: "Open AfkBot1" });
  const card = page.getByRole("listitem").filter({ has: link });
  await expect(card.getByText("Stopped")).toBeVisible();

  backend.setBot("AfkBot1", {
    desiredState: "running",
    serverAddress: "127.0.0.1:25565",
    status: { kind: "reported", state: "online", since: SINCE, online_since: SINCE },
  });
  await push(page, frame(backend, "AfkBot1"));
  await expect(card.getByText("Online")).toBeVisible();
  await expect(card.getByText("127.0.0.1:25565")).toBeVisible();

  await link.click();
  const main = page.getByRole("main");
  await expect(main.getByRole("heading", { name: "AfkBot1" })).toBeVisible();
  await expect(main.getByText("Online")).toBeVisible();
  backend.setBot("AfkBot1", { status: { kind: "reported", state: "stopping", since: SINCE } });
  await push(page, frame(backend, "AfkBot1"));
  await expect(main.getByText("Stopping")).toBeVisible();

  await main.getByRole("link", { name: "Home" }).click();
  await expect(card.getByText("Stopping")).toBeVisible();
  const id = backend.bot("AfkBot1").id;
  backend.removeAccount("AfkBot1");
  await push(page, { update: "event", event: { type: "bot_removed", bot_id: id } });
  await expect(link).toHaveCount(0);
});
