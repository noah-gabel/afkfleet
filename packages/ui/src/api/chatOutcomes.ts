// What came of the chat lines users sent (Plan.md P11.9; ADR-0020, S70, R116, R121): per chat
// request, the line it made, its final outcome, and the loss count its send began at. Pure:
// the live patches and the send box write it into its own query key, never into the cached
// lines, whose routine refetches would wipe it. It never holds a line's text.
//
// The merge is order-free, since a line's `chat` frame, its `chat_sent` or `chat_failed`, and
// the send's own answer arrive in any order: the first link wins, a final is never replaced,
// and pending counts only without a final. A pending mark lasts while the live updates lost
// nothing since the send began (`stores.live`'s count, R115); after that the app can't know,
// so the line shows no mark, never "sent". Only the viewer's own sends are ever pending:
// another user's line is marked from its frames alone.
import type { ChatFailure } from "../generated/ChatFailure";
import type { ChatSendResponse } from "../generated/ChatSendResponse";

/** The most requests kept per bot. */
export const OUTCOMES_MAX = 1024;

/** How a line ended: sent, or failed with why when the viewer may see it. */
export type ChatFinal =
  | { readonly kind: "sent" }
  | { readonly kind: "failed"; readonly failure?: ChatFailure };

/** What's known of one request. */
export interface ChatOutcome {
  readonly line?: number;
  readonly final?: ChatFinal;
  readonly pendingSince?: number;
}

/** A bot's requests, oldest first. */
export type ChatOutcomes = Readonly<Record<string, ChatOutcome>>;

/** A line's mark. */
export type ChatMark = ChatFinal | { readonly kind: "pending" };

/** `outcomes` with `request` linked to `line`, unless it's linked already. */
export function linked(outcomes: ChatOutcomes, request: string, line: number): ChatOutcomes {
  const kept = outcomes[request];
  return kept?.line === undefined ? withOutcome(outcomes, request, { ...kept, line }) : outcomes;
}

/**
 * `outcomes` with `request`'s final, unless it has one already; a final ends pending, so the
 * record is the same whichever came first.
 */
export function finished(outcomes: ChatOutcomes, request: string, final: ChatFinal): ChatOutcomes {
  const kept = outcomes[request];
  if (kept?.final !== undefined) {
    return outcomes;
  }
  const { pendingSince: _ended, ...rest } = kept ?? {};
  return withOutcome(outcomes, request, { ...rest, final });
}

/** `outcomes` with a send's `answer`, the send begun at loss count `since`. */
export function answered(
  outcomes: ChatOutcomes,
  answer: ChatSendResponse,
  since: number,
): ChatOutcomes {
  const request = answer.request_id;
  const link = linked(outcomes, request, answer.message_id);
  switch (answer.outcome) {
    case "sent":
      return finished(link, request, { kind: "sent" });
    case "pending": {
      const kept = link[request];
      return kept?.final === undefined && kept?.pendingSince === undefined
        ? withOutcome(link, request, { ...kept, pendingSince: since })
        : link;
    }
    default:
      // An outcome this version doesn't know: the line is linked, and nothing more.
      return link;
  }
}

/** Each linked line's mark, at loss count `losses`. */
export function marks(outcomes: ChatOutcomes, losses: number): ReadonlyMap<number, ChatMark> {
  const marked = new Map<number, ChatMark>();
  for (const outcome of Object.values(outcomes)) {
    if (outcome.line === undefined) {
      continue;
    }
    if (outcome.final !== undefined) {
      marked.set(outcome.line, outcome.final);
    } else if (outcome.pendingSince === losses) {
      marked.set(outcome.line, { kind: "pending" });
    }
  }
  return marked;
}

/** Whether `data` holds a bot's outcomes, as their query keeps them. */
export function isOutcomes(data: unknown): data is ChatOutcomes {
  return typeof data === "object" && data !== null && !Array.isArray(data);
}

/** `outcomes` with `request`'s `outcome`, a new request last, the oldest dropped past the cap. */
function withOutcome(outcomes: ChatOutcomes, request: string, outcome: ChatOutcome): ChatOutcomes {
  const next: Record<string, ChatOutcome> = { ...outcomes, [request]: outcome };
  const requests = Object.keys(next);
  for (const old of requests.slice(0, Math.max(0, requests.length - OUTCOMES_MAX))) {
    delete next[old];
  }
  return next;
}
