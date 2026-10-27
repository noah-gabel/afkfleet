import { act, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeWatch } from "../../api/FakeApiClient";
import { chatOutcomesKey } from "../../api/queries";
import type { BotResponse } from "../../generated/BotResponse";
import type { ChatLine } from "../../generated/ChatLine";
import type { ChatSendResponse } from "../../generated/ChatSendResponse";
import type { GrantLevel } from "../../generated/GrantLevel";
import type { LiveUpdateMessage } from "../../generated/LiveUpdateMessage";
import { account } from "../../test/accounts";
import { deferred } from "../../test/fixtures";
import { bot, botId, event, line, refetch } from "../../test/live";
import { fakeTime } from "../../test/renderApp";
import { expectWait, limited } from "../../test/retryWait";
import { atBot } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ID = botId(1);
const NOW = new Date("2026-10-27T15:00:00.000Z");
const REQUEST = "01900000-0000-7000-8000-0000000004a1";
const PASSWORD = "a password";

afterEach(() => {
  vi.useRealTimers();
});

/** Bot 1 as a viewer at `level` sees it, online. */
function seen(level: GrantLevel = "manage"): BotResponse {
  return bot(1, {
    account: account(1, { level }),
    desired_state: "running",
    status: { kind: "reported", state: "online", since: NOW.toISOString() },
  });
}

/** The user's own line `text`, kept as row `id`. */
function own(id: number, text: string): ChatLine {
  return { ...line(id, text), direction: "out", kind: "user", mine: true };
}

/** The answer to a send kept as row `id`. */
function answer(id: number, outcome: ChatSendResponse["outcome"] = "sent"): ChatSendResponse {
  return { request_id: REQUEST, message_id: id, outcome };
}

/**
 * Bot 1's screen as a viewer at `level`, with a watch the test pushes into; the chat reads
 * `chat.lines`, newest first, which a send adds to through `handlers`.
 */
async function sendBox(handlers: Partial<ApiClient> = {}, level: GrantLevel = "manage") {
  const time = fakeTime();
  vi.setSystemTime(NOW);
  const watch = fakeWatch();
  const chat = { lines: [] as ChatLine[] };
  const app = await atBot(
    ID,
    {
      bot: async () => seen(level),
      botChat: async () => ({ items: [...chat.lines] }),
      stepUp: async () => {},
      watchEvents: watch.handler,
      ...handlers,
    },
    { advanceTimers: time.advanceTimers },
  );
  await screen.findByRole("heading", { name: "Chat" });
  return { app, time, watch, chat };
}

/** The send box's field. */
function field(): HTMLInputElement {
  return screen.getByRole("textbox", { name: "Message or /command" }) as HTMLInputElement;
}

/** Pushes `updates` into the current watch. */
async function push(watch: ReturnType<typeof fakeWatch>, ...updates: LiveUpdateMessage[]) {
  for (const update of updates) {
    await act(async () => {
      watch.latest().push(update);
      await Promise.resolve();
    });
  }
}

/** The text after the line `text` on the console: its marks. */
function marksOf(text: string): string {
  const item = screen.getByText(text).closest("li");
  return item?.textContent?.split(text)[1] ?? "";
}

describe("the send box (U45, S70)", () => {
  it("sends a line, empties, and marks it sent", async () => {
    const { app, chat } = await sendBox({
      sendChat: async (_id, request) => {
        chat.lines = [own(9, request.message)];
        return answer(9);
      },
    });

    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("hello")).toBeDefined();
    expect(field().value).toBe("");
    expect(marksOf("hello")).toBe("sent");
    expect(app.fake.calls.filter((call) => call.method === "sendChat")).toEqual([
      { method: "sendChat", args: [ID, { message: "hello" }] },
    ]);
  });

  it("is locked with a spinner while the line is sent", async () => {
    const sent = deferred<ChatSendResponse>();
    const { app } = await sendBox({ sendChat: () => sent.promise });

    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    expect(screen.getByRole("img", { name: "Sending…" })).toBeDefined();
    expect(field().readOnly).toBe(true);
    sent.resolve(answer(9));
    expect(await screen.findByRole("button", { name: "Send" })).toBeDefined();
  });

  it("marks a line pending until its chat_sent", async () => {
    const { app, watch } = await sendBox({ sendChat: async () => answer(9, "pending") });
    await push(watch, refetch("ready"));

    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));
    await push(watch, event({ type: "chat", bot_id: ID, line: own(9, "hello"), request: REQUEST }));
    expect(await screen.findByText("hello")).toBeDefined();
    const pending = marksOf("hello");
    await push(watch, event({ type: "chat_sent", bot_id: ID, request: REQUEST }));

    expect(pending).toBe("pending");
    expect(await screen.findByText("sent")).toBeDefined();
    expect(marksOf("hello")).toBe("sent");
  });

  it("sends a command off the allowlist to /command after the password, locked meanwhile", async () => {
    let asked = 0;
    const { app, chat } = await sendBox({
      sendChat: async () => {
        throw refusal(403, "command_needs_step_up");
      },
      sendCommand: async (_id, request) => {
        asked += 1;
        if (asked === 1) {
          throw refusal(403, "step_up_required");
        }
        chat.lines = [own(9, request.message)];
        return answer(9);
      },
    });

    await app.user.type(field(), "/op AfkBot2");
    await app.user.click(screen.getByRole("button", { name: "Send" }));
    expect(
      await screen.findByText("Enter your password to send this command. It counts for 5 minutes."),
    ).toBeDefined();
    await app.user.type(field(), "3");
    expect(field().value).toBe("/op AfkBot2");
    expect(screen.getByRole("button", { name: "Send" })).toHaveProperty("disabled", true);
    await app.user.type(screen.getByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("/op AfkBot2")).toBeDefined();
    expect(field().value).toBe("");
    expect(
      app.fake.calls.filter((call) => call.method === "sendCommand").map((call) => call.args),
    ).toEqual([
      [ID, { message: "/op AfkBot2" }],
      [ID, { message: "/op AfkBot2" }],
    ]);
  });

  it("keeps the text and shows the server's message on a 409", async () => {
    const { app } = await sendBox({
      sendChat: async () => {
        throw refusal(409, "bot_not_online");
      },
    });

    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("refused: bot_not_online")).toBeDefined();
    expect(field().value).toBe("hello");
  });

  it("shows a 422 at the field, keeping the text", async () => {
    const { app } = await sendBox({
      sendChat: async () => {
        throw refusal(422, "validation_failed", {
          fields: [
            {
              path: "message",
              message:
                "must be one line, without control characters, formatting codes or text-direction marks",
            },
          ],
        });
      },
    });

    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    expect(
      await screen.findByText(
        "must be one line, without control characters, formatting codes or text-direction marks",
      ),
    ).toBeDefined();
    expect(field().value).toBe("hello");
  });

  it("says an empty line can't be sent, before any call", async () => {
    const { app } = await sendBox();

    await app.user.type(field(), "   ");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("must not be empty")).toBeDefined();
    expect(app.fake.calls.some((call) => call.method === "sendChat")).toBe(false);
  });

  it("waits out a 429 with Send disabled, keeping the text", async () => {
    const { app, time } = await sendBox({
      sendChat: async () => {
        throw limited(3);
      },
    });

    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    await expectWait(app, time, { button: "Send", method: "sendChat", seconds: 3 });
    expect(field().value).toBe("hello");
  });

  it("shows a 403's message and reads the bot again", async () => {
    let reads = 0;
    const { app } = await sendBox(
      {
        bot: async () => {
          reads += 1;
          return seen("control");
        },
        sendChat: async () => {
          throw refusal(403, "forbidden");
        },
      },
      "control",
    );
    const before = reads;

    await app.user.type(field(), "/op AfkBot2");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("refused: forbidden")).toBeDefined();
    expect(reads).toBe(before + 1);
  });

  it("reads the bot again on a 404, which shows the screen's own 404", async () => {
    let reads = 0;
    const { app } = await sendBox({
      bot: async () => {
        reads += 1;
        if (reads > 1) {
          throw refusal(404, "not_found");
        }
        return seen();
      },
      sendChat: async () => {
        throw refusal(404, "not_found");
      },
    });

    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("This bot doesn't exist, or you can't see it.")).toBeDefined();
    expect(reads).toBe(2);
  });

  it("records a late answer only into the sign-in it was sent in", async () => {
    const late = deferred<ChatSendResponse>();
    const { app } = await sendBox({ sendChat: () => late.promise });
    await app.user.type(field(), "hello");
    await app.user.click(screen.getByRole("button", { name: "Send" }));
    const { queryClient, router, stores } = app.runtime;

    await act(async () => {
      stores.session.getState().signOut();
      queryClient.clear();
      stores.session.getState().signIn();
      await router.navigate({ to: "/bots/$id", params: { id: ID } });
    });
    await screen.findByRole("textbox", { name: "Message or /command" });
    await act(async () => {
      late.resolve(answer(9));
      await Promise.resolve();
    });

    expect(queryClient.getQueryData(chatOutcomesKey(ID))).toEqual({});
  });

  it("offers a View holder no box", async () => {
    await sendBox({}, "view");

    expect(screen.queryByRole("textbox", { name: "Message or /command" })).toBeNull();
  });
});

describe("the send box and the secret rule (U34)", () => {
  it("leaves no line and no password in the cache after a send and a step-up", async () => {
    let asked = 0;
    const { app } = await sendBox({
      sendChat: async () => {
        throw refusal(403, "command_needs_step_up");
      },
      sendCommand: async () => {
        asked += 1;
        if (asked === 1) {
          throw refusal(403, "step_up_required");
        }
        return answer(9);
      },
    });

    await app.user.type(field(), "/login hunter2");
    await app.user.click(screen.getByRole("button", { name: "Send" }));
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));
    await vi.waitFor(() => expect(field().value).toBe(""));

    const cached = JSON.stringify([
      app.runtime.queryClient
        .getQueryCache()
        .getAll()
        .map((query) => query.state),
      app.runtime.queryClient
        .getMutationCache()
        .getAll()
        .map((mutation) => mutation.state),
    ]);
    expect(cached).not.toContain("hunter2");
    expect(cached).not.toContain("/login");
    expect(cached).not.toContain(PASSWORD);
    expect(app.runtime.queryClient.getQueryData(chatOutcomesKey(ID))).toEqual({
      [REQUEST]: { line: 9, final: { kind: "sent" } },
    });
    expect(within(document.body).queryByText("/login hunter2")).toBeNull();
  });
});
