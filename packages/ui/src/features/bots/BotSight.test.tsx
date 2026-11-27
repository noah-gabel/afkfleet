// What a bot sees on its screen (Plan.md P16.20): asked of its agent only when Look is pressed,
// each filled slot with its number, its row and column and, in a screen's own grid, the
// `menu.at` that names it, its item's every field as plain text, the empty slots as ranges; the
// bot's own inventory when it has no screen open; and why it can't be said, in plain words.
import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { BotScreenItem } from "../../generated/BotScreenItem";
import type { BotScreenResponse } from "../../generated/BotScreenResponse";
import type { BotScreenSlot } from "../../generated/BotScreenSlot";
import { bot, botId } from "../../test/live";
import { atBot } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ID = botId(1);

const POTION: BotScreenItem = {
  id: "minecraft:potion",
  count: 1,
  name: "Potion of the Turtle Master",
  lore: ["Price: 1,250", "Seller: AfkBot2"],
  potion: "minecraft:turtle_master",
  enchantments: [],
  stored_enchantments: [],
};

const SWORD: BotScreenItem = {
  id: "minecraft:diamond_sword",
  count: 1,
  name: "Diamond Sword",
  lore: [],
  enchantments: [
    { id: "minecraft:mending", level: 1 },
    { id: "minecraft:sharpness", level: 5 },
  ],
  stored_enchantments: [],
  durability: 1500,
};

const BOOK: BotScreenItem = {
  id: "minecraft:enchanted_book",
  count: 1,
  name: "<b>Enchanted Book</b>",
  lore: [],
  enchantments: [],
  stored_enchantments: [{ id: "minecraft:efficiency", level: 4 }],
};

/** A slot of a nine-column grid numbered from `first`. */
function slot(index: number, first: number, item?: BotScreenItem): BotScreenSlot {
  const place = index - first;
  return {
    index,
    row: Math.floor(place / 9),
    column: place % 9,
    ...(item === undefined ? {} : { item }),
  };
}

/** An auction house of one row, the potion in slot 4, the sword in the bot's first hotbar slot. */
const AUCTION: BotScreenResponse = {
  kind: "screen",
  screen: {
    kind: "generic_9x1",
    title: "Auction House",
    rows: 1,
    columns: 9,
    slots: Array.from({ length: 9 }, (_, n) => slot(n, 0, n === 4 ? POTION : undefined)),
    player: Array.from({ length: 36 }, (_, n) => slot(9 + n, 9, n === 27 ? SWORD : undefined)),
    cursor: BOOK,
  },
};

/** Bot 1's screen, its agent answering `answer`. */
async function sight(answer: () => Promise<BotScreenResponse>) {
  const botScreen = vi.fn(answer);
  const app = await atBot(ID, { bot: async () => bot(1), botScreen });
  await screen.findByRole("heading", { name: bot(1).account.name });
  const card = screen.getByRole("region", { name: "What it sees" });
  return { app, botScreen, card };
}

describe("what a bot sees (P16.20)", () => {
  it("asks its agent only when Look is pressed, and again when asked again", async () => {
    const { app, botScreen, card } = await sight(async () => AUCTION);
    expect(botScreen).not.toHaveBeenCalled();

    await app.user.click(within(card).getByRole("button", { name: "Look" }));
    expect(await within(card).findByText("Auction House")).toBeTruthy();
    await app.user.click(within(card).getByRole("button", { name: "Look again" }));

    await waitFor(() => expect(botScreen).toHaveBeenCalledTimes(2));
    expect(botScreen).toHaveBeenCalledWith(ID);
  });

  it("shows each filled slot as a script names it, its item's every field, the empty ones as ranges", async () => {
    const { app, card } = await sight(async () => AUCTION);

    await app.user.click(within(card).getByRole("button", { name: "Look" }));

    const own = await within(card).findByRole("list", { name: "Its slots (menu.slots)" });
    expect(within(own).getByText("Slot 4 · row 0, column 4 · menu.at(0, 4)")).toBeTruthy();
    expect(within(own).getByText("minecraft:potion × 1")).toBeTruthy();
    expect(within(own).getByText("Potion of the Turtle Master")).toBeTruthy();
    expect(
      within(own).getByText("Price: 1,250 Seller: AfkBot2", {
        normalizer: (text) => text.replace(/\s+/g, " "),
      }),
    ).toBeTruthy();
    expect(within(own).getByText("Potion: minecraft:turtle_master")).toBeTruthy();
    expect(within(card).getByText("Empty: 0–3, 5–8")).toBeTruthy();

    const player = within(card).getByRole("list", { name: "The bot's slots in it (menu.player)" });
    expect(within(player).getByText("Slot 36 · row 3, column 0")).toBeTruthy();
    expect(
      within(player).getByText("Enchantments: minecraft:mending 1, minecraft:sharpness 5"),
    ).toBeTruthy();
    expect(within(player).getByText("1500 uses left")).toBeTruthy();
    expect(within(card).getByText("Empty: 9–35, 37–44")).toBeTruthy();

    expect(within(card).getByText("menu.kind generic_9x1 · 1 row of 9")).toBeTruthy();
    expect(within(card).getByText("Stored enchantments: minecraft:efficiency 4")).toBeTruthy();
  });

  it("shows a server's text as plain text, never as markup", async () => {
    const { app, card } = await sight(async () => AUCTION);

    await app.user.click(within(card).getByRole("button", { name: "Look" }));

    expect(await within(card).findByText("<b>Enchanted Book</b>")).toBeTruthy();
    expect(card.querySelector("b")).toBeNull();
  });

  it("shows the bot's own inventory when no screen is open, and a sign editor that waits", async () => {
    const { app, card } = await sight(async () => ({
      kind: "inventory",
      inventory: [
        { index: 5 },
        ...Array.from({ length: 36 }, (_, n) => slot(9 + n, 9, n === 0 ? SWORD : undefined)),
      ],
      sign: { x: -12, y: 64, z: 300, front: true },
    }));

    await app.user.click(within(card).getByRole("button", { name: "Look" }));

    const own = await within(card).findByRole("list", {
      name: "No screen open: its own inventory (inventory.slots)",
    });
    expect(within(own).getByText("Slot 9 · row 0, column 0")).toBeTruthy();
    expect(within(card).getByText("Empty: 5, 10–44")).toBeTruthy();
    expect(
      within(card).getByText(
        "A sign editor waits for its answer: the sign at -12 64 300, its front (on_sign_open).",
      ),
    ).toBeTruthy();
  });

  it.each([
    ["offline", "It isn't in a world, so there's nothing to see."],
    [
      "agent_too_old",
      "Its agent runs a version from before this release, which can't show what a bot sees. Update the agent to this release.",
    ],
    ["no_answer", "Its agent didn't answer within 5 seconds. Try again."],
    ["blind", "The server answered with something this app doesn't know (blind). Update the app."],
  ] as const)("says plainly why it can't see: %s", async (kind, text) => {
    const { app, card } = await sight(async () => ({ kind }) as BotScreenResponse);

    await app.user.click(within(card).getByRole("button", { name: "Look" }));

    expect(await within(card).findByText(text)).toBeTruthy();
  });

  it("shows the limit's refusal with its wait, and nothing it saw before", async () => {
    let calls = 0;
    const { app, card } = await sight(async () => {
      calls += 1;
      if (calls === 1) {
        return AUCTION;
      }
      throw refusal(429, "rate_limited", { retry_after_secs: 20 });
    });
    await app.user.click(within(card).getByRole("button", { name: "Look" }));
    await within(card).findByText("Auction House");

    await app.user.click(within(card).getByRole("button", { name: "Look again" }));

    expect(await within(card).findByText(/Try again in 20 s\.$/)).toBeTruthy();
    expect(within(card).queryByText("Auction House")).toBeNull();
  });
});
