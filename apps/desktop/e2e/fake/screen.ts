// What an online bot sees in the e2e fake (Plan.md P16.20): an auction house's chest of one row,
// a listing of a Turtle Master potion in slot 4, the bot's sword in its first hotbar slot and
// paper in the rest of its inventory, as `GET /bots/{id}/screen` answers it.
import type { BotScreenItem } from "@afkfleet/ui/generated/BotScreenItem";
import type { BotScreenResponse } from "@afkfleet/ui/generated/BotScreenResponse";
import type { BotScreenSlot } from "@afkfleet/ui/generated/BotScreenSlot";

const POTION: BotScreenItem = {
  id: "minecraft:potion",
  count: 1,
  name: "Potion of the Turtle Master",
  lore: ["Price: 1,250", "Seller: AfkBot2", "Expires in 2 h"],
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

const PAPER: BotScreenItem = {
  id: "minecraft:paper",
  count: 12,
  name: "Paper",
  lore: [],
  enchantments: [],
  stored_enchantments: [],
};

/** A slot of a grid of nine columns, numbered from `first`, holding `item` or nothing. */
function slot(index: number, first: number, item?: BotScreenItem): BotScreenSlot {
  const place = index - first;
  return {
    index,
    row: Math.floor(place / 9),
    column: place % 9,
    ...(item === undefined ? {} : { item }),
  };
}

/** The auction house the fake's online bots see. */
export const AUCTION_HOUSE: BotScreenResponse = {
  kind: "screen",
  screen: {
    kind: "generic_9x1",
    title: "Auction House",
    rows: 1,
    columns: 9,
    slots: Array.from({ length: 9 }, (_, n) => slot(n, 0, n === 4 ? POTION : undefined)),
    player: Array.from({ length: 36 }, (_, n) =>
      slot(9 + n, 9, n === 27 ? SWORD : n === 0 ? PAPER : undefined),
    ),
  },
};
