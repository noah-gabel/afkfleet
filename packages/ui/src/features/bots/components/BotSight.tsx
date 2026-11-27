// What a bot sees, on its screen (Plan.md P16.20; ADR-0036, M14): its agent is asked only when Look
// is pressed, since the server allows each viewer a few looks a minute. It shows the screen the bot
// has open, or its own inventory when none is, each filled slot as a script names it (its number,
// its row and column, and in a screen's own grid the `menu.at` that names it) with its item's every
// field, and the empty slots as ranges of numbers; or why it can't be said, in plain words. Every
// name and line is the Minecraft server's, cleaned by the server and shown as plain text, which the
// viewer can select and copy into a script.
import { useId } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useBotSight } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
import type { BotResponse } from "../../../generated/BotResponse";
import type { BotScreenItem } from "../../../generated/BotScreenItem";
import type { BotScreenResponse } from "../../../generated/BotScreenResponse";
import type { BotScreenSlot } from "../../../generated/BotScreenSlot";
import { ranges } from "../sight";
import {
  ALL_EMPTY,
  emptySlots,
  enchantmentsLine,
  itemLine,
  potionLine,
  SIGHT_ASKING,
  SIGHT_CURSOR,
  SIGHT_HEADING,
  SIGHT_INVENTORY,
  SIGHT_LOOK,
  SIGHT_LOOK_AGAIN,
  SIGHT_NOTE,
  SIGHT_OWN,
  SIGHT_PLAYER,
  SIGHT_UNSEEN,
  screenKindLine,
  sightSign,
  sightUnknown,
  slotPlace,
  usesLeft,
} from "../texts";

/** An item's every field, one line each. */
function ItemFields({ item }: { item: BotScreenItem }) {
  return (
    <>
      <p className="font-mono text-sm break-all">{itemLine(item.id, item.count)}</p>
      <p className="text-sm break-words">{item.name}</p>
      {item.lore.length === 0 ? null : (
        <p className="text-sm break-words whitespace-pre-line text-muted-foreground">
          {item.lore.join("\n")}
        </p>
      )}
      {item.potion === undefined ? null : (
        <p className="font-mono text-xs break-all">{potionLine(item.potion)}</p>
      )}
      {item.enchantments.length === 0 ? null : (
        <p className="font-mono text-xs break-all">{enchantmentsLine(false, item.enchantments)}</p>
      )}
      {item.stored_enchantments.length === 0 ? null : (
        <p className="font-mono text-xs break-all">
          {enchantmentsLine(true, item.stored_enchantments)}
        </p>
      )}
      {item.durability === undefined ? null : (
        <p className="text-xs text-muted-foreground">{usesLeft(item.durability)}</p>
      )}
    </>
  );
}

/** A list of slots: each filled one with its item, the empty ones as ranges after it. */
function Slots({
  label,
  slots,
  grid,
}: {
  label: string;
  slots: readonly BotScreenSlot[];
  /** Whether `menu.at(row, column)` names these slots: a screen's own grid. */
  grid: boolean;
}) {
  const labelId = useId();
  const filled = slots.filter((slot) => slot.item !== undefined);
  const empty = slots.filter((slot) => slot.item === undefined).map((slot) => slot.index);
  return (
    <div className="flex flex-col gap-1">
      <h3 id={labelId} className="text-sm font-semibold">
        {label}
      </h3>
      {filled.length === 0 ? (
        <p className="text-sm text-muted-foreground">{ALL_EMPTY}</p>
      ) : (
        <ol aria-labelledby={labelId} className="flex flex-col gap-2">
          {filled.map((slot) => (
            <li key={slot.index} className="flex flex-col gap-0.5 border-t pt-2">
              <p className="font-mono text-xs text-muted-foreground">
                {slotPlace(slot.index, slot.row, slot.column, grid)}
              </p>
              {slot.item === undefined ? null : <ItemFields item={slot.item} />}
            </li>
          ))}
        </ol>
      )}
      {empty.length === 0 || filled.length === 0 ? null : (
        <p className="font-mono text-xs text-muted-foreground">{emptySlots(ranges(empty))}</p>
      )}
    </div>
  );
}

/** The agent's answer: what the bot sees, or why that can't be said. */
function Answer({ answer }: { answer: BotScreenResponse }) {
  const sign =
    answer.sign === undefined ? null : (
      <p className="text-sm">
        {sightSign(answer.sign.x, answer.sign.y, answer.sign.z, answer.sign.front)}
      </p>
    );
  switch (answer.kind) {
    case "screen": {
      const view = answer.screen;
      if (view === undefined) {
        return <p className="text-sm">{sightUnknown(answer.kind)}</p>;
      }
      return (
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-0.5">
            <p className="text-sm font-semibold break-words">{view.title}</p>
            <p className="font-mono text-xs text-muted-foreground">
              {screenKindLine(view.kind, view.rows, view.columns)}
            </p>
          </div>
          {sign}
          {view.cursor === undefined ? null : (
            <div className="flex flex-col gap-0.5">
              <h3 className="text-sm font-semibold">{SIGHT_CURSOR}</h3>
              <ItemFields item={view.cursor} />
            </div>
          )}
          <Slots label={SIGHT_OWN} slots={view.slots} grid />
          <Slots label={SIGHT_PLAYER} slots={view.player} grid={false} />
        </div>
      );
    }
    case "inventory":
      return (
        <div className="flex flex-col gap-3">
          {sign}
          <Slots label={SIGHT_INVENTORY} slots={answer.inventory ?? []} grid={false} />
        </div>
      );
    case "offline":
    case "agent_too_old":
    case "no_answer":
      return <p className="text-sm">{SIGHT_UNSEEN[answer.kind]}</p>;
    default:
      // A kind a newer server may send (an open enum).
      return <p className="text-sm">{sightUnknown(answer.kind)}</p>;
  }
}

/** `bot`'s "What it sees" card. */
export function BotSight({ bot }: { bot: BotResponse }) {
  const look = useBotSight(bot.id);
  const headingId = useId();
  const failure = look.error === null ? null : toApiError(look.error).error;
  return (
    <section aria-labelledby={headingId}>
      <Card className="gap-3 p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id={headingId} className="text-base font-semibold">
            {SIGHT_HEADING}
          </h2>
          <Button
            variant="outline"
            size="sm"
            disabled={look.isFetching}
            onClick={() => {
              void look.refetch();
            }}
          >
            {look.data === undefined && failure === null ? SIGHT_LOOK : SIGHT_LOOK_AGAIN}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">{SIGHT_NOTE}</p>
        <div role="status" className="flex flex-col gap-3">
          {look.isFetching ? <p className="text-sm text-muted-foreground">{SIGHT_ASKING}</p> : null}
          {failure !== null && !look.isFetching ? (
            <p className="text-sm text-destructive">{describeError(failure)}</p>
          ) : null}
        </div>
        {look.data === undefined || failure !== null ? null : <Answer answer={look.data} />}
      </Card>
    </section>
  );
}
