// A bot's chat console (Plan.md P11.9; ADR-0020, U39, U43, U44, U49, S7, S9, S11, S70), the
// right column of its screen, filling its height.
//
// - **The history** (`useChat`, a cached query, U34) in the API's order, which is the
//   server's commit order (U5): never sorted by time. Newest at the bottom; "Show older" on
//   top reads the page before, keeping the reader's place; the list follows new lines while
//   it's scrolled to the bottom, and stays put while it's scrolled up.
// - **A line** is one block per message, in columns of time, sender and text (chat.ts says
//   what each reads). Its text is plain, as it came: a line break arrived as ` | ` (S11), and
//   nothing is pre-formatted; its block clips stacked combining marks. Every mark sits outside
//   the text: what was left out of it (S9), and, for a line someone sent, its outcome (S70).
// - **Live lines** reach the history through the live layer (S60), and outcomes through
//   their own key (chatOutcomes.ts); a pending mark lasts while no live update was missed.

import { cn } from "cn";
import { type ReactNode, useLayoutEffect, useRef } from "react";

import { toApiError } from "../../../api/ApiError";
import { type ChatMark, marks } from "../../../api/chatOutcomes";
import { describeError } from "../../../api/describeError";
import { useChat, useChatOutcomes } from "../../../api/queries";
import { useLive } from "../../../app/context";
import { Badge } from "../../../components/ui/badge";
import { Button } from "../../../components/ui/button";
import type { BotResponse } from "../../../generated/BotResponse";
import type { ChatLine } from "../../../generated/ChatLine";
import { chatTime, lineNotes, markText, type Sender, senderOf } from "../chat";
import { CHAT, chatFailed, olderFailed } from "../texts";

/** How far from the bottom, in pixels, the list still counts as scrolled to it. */
const FOLLOW_SLACK = 16;

/** `bot`'s chat; `children` go under the list (the send box). */
export function BotChat({ bot, children }: { bot: BotResponse; children?: ReactNode }) {
  const chat = useChat(bot.id);
  const outcomes = useChatOutcomes(bot.id);
  const losses = useLive((state) => state.losses);
  const box = useRef<HTMLDivElement>(null);
  /** Whether the list is scrolled to its bottom: new lines are followed. */
  const following = useRef(true);
  /** How far from the bottom the reader was when older lines were asked for. */
  const anchor = useRef<number | null>(null);
  // The API's order, newest first, turned round: newest at the bottom (U5, U43).
  const lines: ChatLine[] = (chat.data?.pages.flatMap((page) => page.items) ?? []).reverse();
  const marked = marks(outcomes, losses);
  const now = Date.now();
  const oldest = lines.at(0)?.id;
  const newest = lines.at(-1)?.id;
  /** The oldest line the last change showed. */
  const shownOldest = useRef(oldest);

  // When the first or the last line changes: older lines that came keep the reader's place;
  // otherwise new ones are followed while the list was at its bottom.
  useLayoutEffect(() => {
    const list = box.current;
    const olderCame = oldest !== shownOldest.current;
    shownOldest.current = oldest;
    if (list === null || newest === undefined) {
      return;
    }
    if (olderCame && anchor.current !== null) {
      list.scrollTop = list.scrollHeight - anchor.current;
      anchor.current = null;
    } else if (following.current) {
      list.scrollTop = list.scrollHeight;
    }
  }, [oldest, newest]);

  function older(): void {
    const list = box.current;
    if (list !== null) {
      anchor.current = list.scrollHeight - list.scrollTop;
    }
    void chat.fetchNextPage();
  }

  const failure = chat.error === null ? null : toApiError(chat.error).error;
  return (
    <section
      aria-label={CHAT.heading}
      className="flex min-h-96 flex-col gap-3 lg:h-[calc(100dvh-8rem)]"
    >
      <h2 className="text-lg font-semibold">{CHAT.heading}</h2>
      {chat.isPending ? <p className="text-sm text-muted-foreground">{CHAT.loading}</p> : null}
      {failure !== null && chat.data === undefined ? (
        <p role="alert" className="text-sm text-destructive">
          {chatFailed(describeError(failure))}
        </p>
      ) : null}
      {chat.data === undefined ? null : (
        <div
          ref={box}
          role="log"
          aria-label={CHAT.heading}
          className="min-h-0 flex-1 overflow-y-auto rounded-md border p-3"
          onScroll={(event) => {
            const list = event.currentTarget;
            following.current =
              list.scrollHeight - list.scrollTop - list.clientHeight <= FOLLOW_SLACK;
          }}
        >
          <div className="mb-3 flex flex-col items-start gap-1 text-sm">
            {chat.isFetchNextPageError && failure !== null ? (
              <p role="alert" className="text-destructive">
                {olderFailed(describeError(failure))}
              </p>
            ) : null}
            {chat.hasNextPage ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={chat.isFetchingNextPage}
                onClick={older}
              >
                {CHAT.older}
              </Button>
            ) : lines.length > 0 ? (
              <p className="text-muted-foreground">{CHAT.noOlder}</p>
            ) : (
              <p className="text-muted-foreground">{CHAT.empty}</p>
            )}
          </div>
          <ol className="flex flex-col gap-2">
            {lines.map((line) => (
              <Line
                key={line.id}
                line={line}
                bot={bot.account.name}
                mark={marked.get(line.id)}
                now={now}
              />
            ))}
          </ol>
        </div>
      )}
      {children}
    </section>
  );
}

/** One line: its time, its sender, its text with its marks after it (U43, U44). */
function Line({
  line,
  bot,
  mark,
  now,
}: {
  line: ChatLine;
  bot: string;
  mark: ChatMark | undefined;
  now: number;
}) {
  const time = chatTime(line.at, now);
  const sender = senderOf(line, bot);
  return (
    <li
      className={cn(
        "grid grid-cols-[auto_auto_minmax(0,1fr)] items-baseline gap-x-2 text-sm",
        line.mine && "border-l-2 border-primary pl-2",
      )}
    >
      <time dateTime={line.at} title={time.full} className="text-xs text-muted-foreground">
        {time.shown}
      </time>
      <SenderCell sender={sender} />
      <span className="min-w-0">
        <span className="inline-block max-w-full overflow-hidden align-top break-words">
          {line.text}
        </span>
        {lineNotes(line).map((note) => (
          <span key={note} className="ml-2 text-xs text-muted-foreground">
            {note}
          </span>
        ))}
        {mark === undefined ? null : (
          <span
            className={cn(
              "ml-2 text-xs",
              mark.kind === "failed" ? "text-destructive" : "text-muted-foreground",
            )}
          >
            {markText(mark)}
          </span>
        )}
      </span>
    </li>
  );
}

/** A line's sender, in its look (U44); a user's line read as asked of the bot (S7). */
function SenderCell({ sender }: { sender: Sender }) {
  switch (sender.look) {
    case "player":
      return <span className="font-medium text-primary">{sender.label}</span>;
    case "server":
      return <span className="italic text-muted-foreground">{sender.label}</span>;
    case "mode":
      return (
        <span className="flex items-baseline gap-1 font-medium">
          <Badge variant="outline" className="px-1.5 py-0 text-xs font-normal">
            {CHAT.modeTag}
          </Badge>
          {sender.label}
        </span>
      );
    case "user":
      return (
        <span className="font-medium">
          <span className="sr-only">{sender.asked}</span>
          <span aria-hidden="true">{sender.label}</span>
        </span>
      );
    default:
      return <span />;
  }
}
