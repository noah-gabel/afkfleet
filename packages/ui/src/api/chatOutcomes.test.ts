import { describe, expect, it } from "vitest";

import type { ChatSendResponse } from "../generated/ChatSendResponse";
import { answered, type ChatOutcomes, finished, linked, marks, OUTCOMES_MAX } from "./chatOutcomes";

const REQUEST = "01900000-0000-7000-8000-0000000004a1";

/** The answer to a send of line `line`, `sent` or `pending`. */
function answer(outcome: ChatSendResponse["outcome"], line = 7): ChatSendResponse {
  return { request_id: REQUEST, message_id: line, outcome };
}

/** What `steps` leave, from nothing. */
function outcomesAfter(...steps: ((outcomes: ChatOutcomes) => ChatOutcomes)[]): ChatOutcomes {
  return steps.reduce((outcomes, step) => step(outcomes), {} as ChatOutcomes);
}

describe("the chat's outcomes (S70)", () => {
  it("merge in any order: the line's link and its final come out the same", () => {
    const link = (outcomes: ChatOutcomes) => linked(outcomes, REQUEST, 7);
    const sent = (outcomes: ChatOutcomes) => finished(outcomes, REQUEST, { kind: "sent" });

    expect(outcomesAfter(link, sent)).toEqual(outcomesAfter(sent, link));
    expect(outcomesAfter(link, sent)).toEqual({ [REQUEST]: { line: 7, final: { kind: "sent" } } });
  });

  it("keep the first link and the first final", () => {
    const outcomes = outcomesAfter(
      (now) => linked(now, REQUEST, 7),
      (now) => linked(now, REQUEST, 8),
      (now) => finished(now, REQUEST, { kind: "failed", failure: "rate_limited" }),
      (now) => finished(now, REQUEST, { kind: "sent" }),
    );

    expect(outcomes[REQUEST]).toEqual({
      line: 7,
      final: { kind: "failed", failure: "rate_limited" },
    });
  });

  it("link a send's answer: sent as final, pending from the count its send began at", () => {
    expect(answered({}, answer("sent"), 3)[REQUEST]).toEqual({
      line: 7,
      final: { kind: "sent" },
    });
    expect(answered({}, answer("pending"), 3)[REQUEST]).toEqual({ line: 7, pendingSince: 3 });
  });

  it("never put pending over a final, whichever came first", () => {
    const sentFirst = outcomesAfter(
      (now) => finished(now, REQUEST, { kind: "sent" }),
      (now) => answered(now, answer("pending"), 3),
    );
    const pendingFirst = outcomesAfter(
      (now) => answered(now, answer("pending"), 3),
      (now) => finished(now, REQUEST, { kind: "sent" }),
    );

    expect(marks(sentFirst, 3).get(7)).toEqual({ kind: "sent" });
    expect(marks(pendingFirst, 3).get(7)).toEqual({ kind: "sent" });
  });

  it("link an answer whose outcome this version doesn't know, and mark nothing more", () => {
    const unknown = { ...answer("sent"), outcome: "queued" } as unknown as ChatSendResponse;

    expect(answered({}, unknown, 3)[REQUEST]).toEqual({ line: 7 });
  });

  it("keep at most 1024 requests per bot, dropping the oldest", () => {
    let outcomes: ChatOutcomes = {};
    for (let n = 0; n <= OUTCOMES_MAX; n += 1) {
      outcomes = linked(outcomes, `request-${n}`, n);
    }

    const kept = Object.keys(outcomes);

    expect(kept).toHaveLength(OUTCOMES_MAX);
    expect(kept[0]).toBe("request-1");
    expect(kept.at(-1)).toBe(`request-${OUTCOMES_MAX}`);
  });
});

describe("the chat's marks (S70, R116)", () => {
  it("mark each linked line: sent, failed with or without its reason, pending", () => {
    const outcomes = outcomesAfter(
      (now) => answered(now, { request_id: "a", message_id: 1, outcome: "sent" }, 0),
      (now) => linked(now, "b", 2),
      (now) => finished(now, "b", { kind: "failed", failure: "chat_unavailable" }),
      (now) => linked(now, "c", 3),
      (now) => finished(now, "c", { kind: "failed" }),
      (now) => answered(now, { request_id: "d", message_id: 4, outcome: "pending" }, 0),
      (now) => linked(now, "e", 5),
    );

    expect([...marks(outcomes, 0)]).toEqual([
      [1, { kind: "sent" }],
      [2, { kind: "failed", failure: "chat_unavailable" }],
      [3, { kind: "failed" }],
      [4, { kind: "pending" }],
    ]);
  });

  it("drop a pending mark once the count moved, even when it moved during the send", () => {
    const outcomes = answered({}, answer("pending"), 3);

    expect(marks(outcomes, 3).get(7)).toEqual({ kind: "pending" });
    expect(marks(outcomes, 4).has(7)).toBe(false);
  });

  it("mark another user's line from its frames, never as pending", () => {
    const linkedOnly = linked({}, REQUEST, 7);
    const sent = finished(linkedOnly, REQUEST, { kind: "sent" });

    expect(marks(linkedOnly, 0).has(7)).toBe(false);
    expect(marks(sent, 0).get(7)).toEqual({ kind: "sent" });
  });
});
