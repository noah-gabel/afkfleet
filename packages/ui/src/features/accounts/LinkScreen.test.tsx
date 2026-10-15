import { act, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError } from "../../api/FakeApiClient";
import type { LinkFlowResponse } from "../../generated/LinkFlowResponse";
import {
  account,
  accountId,
  FINISHING,
  linked,
  STARTED,
  USER_CODE,
  VERIFICATION_URI,
  WAITING,
} from "../../test/accounts";
import { deferred } from "../../test/fixtures";
import { fakeTime } from "../../test/renderApp";
import { limited } from "../../test/retryWait";
import { atAccount, atScreen } from "../../test/signedIn";
import { cacheText, refusal } from "../../test/signIn";
import {
  CODE_COPIED,
  couldntOpen,
  LINK_GONE,
  LINK_INTRO,
  LINK_NOT_MICROSOFTS,
  LINK_WARNING,
  NOTHING_TO_OPEN,
  reasonText,
  START_BUSY,
  START_LIMITED,
  START_OFFLINE,
  START_OVER_QUOTA,
  START_PENDING,
  startFailed,
} from "./texts";

/** Answers each status read with the next of `answers`, the last one from then on. */
function statuses(...answers: (LinkFlowResponse | Error)[]): ApiClient["linkStatus"] {
  let at = 0;
  return async () => {
    const answer = answers[Math.min(at, answers.length - 1)];
    at += 1;
    if (answer instanceof Error) {
      throw answer;
    }
    return answer as LinkFlowResponse;
  };
}

function calls(app: { fake: { calls: { method: string }[] } }, method: string): number {
  return app.fake.calls.filter((call) => call.method === method).length;
}

/** Start `n`'s answer: each start is a flow of its own. */
function started(n: number): typeof STARTED {
  return { ...STARTED, flow_id: `01900000-0000-7000-8000-0000000002a${n.toString(16)}` };
}

/** The link screen with fake time, over `handlers`. */
async function linkScreen(handlers: Partial<ApiClient>) {
  const time = fakeTime();
  let starts = 0;
  const app = await atScreen(
    "/accounts/link",
    {
      startLink: async () => {
        starts += 1;
        return started(starts);
      },
      openVerificationLink: async () => {},
      linkStatus: statuses(WAITING),
      ...handlers,
    },
    { advanceTimers: time.advanceTimers },
  );
  await screen.findByRole("heading", { name: "Link a Microsoft account", level: 1 });
  return { app, time };
}

/** Clicks Get a code and waits for the code. */
async function getCode(app: Awaited<ReturnType<typeof linkScreen>>["app"]) {
  await app.user.click(screen.getByRole("button", { name: "Get a code" }));
  await screen.findByText(USER_CODE);
}

describe("the link screen", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows the warning first and starts nothing before Get a code", async () => {
    const { app } = await linkScreen({});

    expect(screen.getByText(LINK_WARNING)).toBeDefined();
    expect(screen.getByText(LINK_INTRO)).toBeDefined();
    expect(calls(app, "startLink")).toBe(0);
    await app.user.click(screen.getByRole("link", { name: "Back to accounts" }));
    expect(app.path()).toBe("/accounts");
    expect(calls(app, "startLink")).toBe(0);
  });

  it("shows the code with the warning, and opens Microsoft's page once by itself", async () => {
    const { app } = await linkScreen({});

    await getCode(app);

    expect(screen.getByText(LINK_WARNING)).toBeDefined();
    expect(calls(app, "openVerificationLink")).toBe(1);
    expect(
      await screen.findByText(
        /^Waiting for you to enter the code on Microsoft's page\. It works until /,
      ),
    ).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Open Microsoft's page" }));
    expect(calls(app, "openVerificationLink")).toBe(2);
  });

  it("copies the code", async () => {
    const { app } = await linkScreen({});
    await getCode(app);

    await app.user.click(screen.getByRole("button", { name: "Copy code" }));

    expect(await navigator.clipboard.readText()).toBe(USER_CODE);
    expect(await screen.findByText(CODE_COPIED)).toBeDefined();
  });

  it("reads the status every 2 s until the link ends, then drops the code", async () => {
    const { app, time } = await linkScreen({
      linkStatus: statuses(WAITING, FINISHING, linked(5, "pending")),
    });
    app.runtime.queryClient.setQueryData(["accounts", "one", accountId(5)], account(5));
    await getCode(app);
    await screen.findByText(/^Waiting for you/);

    await time.advance(2000);
    expect(
      await screen.findByText("Microsoft confirmed your sign-in. Checking the Minecraft profile…"),
    ).toBeDefined();
    await time.advance(2000);

    expect(
      await screen.findByText(
        "AfkBot5 is linked. It waits for an Admin's or the Owner's approval before a bot can use it.",
      ),
    ).toBeDefined();
    expect(screen.queryByText(USER_CODE)).toBeNull();
    expect(screen.queryByRole("button", { name: "Copy code" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Open Microsoft's page" })).toBeNull();
    expect(
      app.runtime.queryClient.getQueryState(["accounts", "one", accountId(5)])?.isInvalidated,
    ).toBe(true);
    const reads = calls(app, "linkStatus");
    await time.advance(10_000);
    expect(calls(app, "linkStatus")).toBe(reads);
    await app.user.click(screen.getByRole("link", { name: "Open account" }));
    expect(app.path()).toBe(`/accounts/${accountId(5)}`);
  });

  it("says an Owner's link is linked, without waiting for approval", async () => {
    const { app } = await linkScreen({ linkStatus: statuses(linked(5, "active")) });

    await app.user.click(screen.getByRole("button", { name: "Get a code" }));

    expect(await screen.findByText("AfkBot5 is linked.")).toBeDefined();
  });

  it("says a flow that's gone is gone, stops reading and offers a new code", async () => {
    const { app, time } = await linkScreen({
      linkStatus: statuses(refusal(404, "not_found")),
    });

    await app.user.click(screen.getByRole("button", { name: "Get a code" }));

    expect(await screen.findByText(LINK_GONE)).toBeDefined();
    expect(screen.queryByText(USER_CODE)).toBeNull();
    const reads = calls(app, "linkStatus");
    await time.advance(10_000);
    expect(calls(app, "linkStatus")).toBe(reads);
    expect(screen.getByRole("button", { name: "Get a new code" })).toBeDefined();
  });

  it.each<[string, LinkFlowResponse, string]>([
    ["declined", { status: "declined" }, "The sign-in was declined at Microsoft."],
    ["expired", { status: "expired" }, "The code expired before it was entered."],
    ["failed", { status: "failed" }, "The link couldn't be finished."],
    ["refused", { status: "refused", reason: "not_owned" }, reasonText("not_owned", "the account")],
  ])("offers a new code after %s, which starts again", async (_state, ended, text) => {
    const { app } = await linkScreen({ linkStatus: statuses(ended, WAITING) });
    await app.user.click(screen.getByRole("button", { name: "Get a code" }));
    expect(await screen.findByText(text)).toBeDefined();

    await app.user.click(screen.getByRole("button", { name: "Get a new code" }));

    expect(await screen.findByText(USER_CODE)).toBeDefined();
    expect(calls(app, "startLink")).toBe(2);
  });

  it.each<[string, LinkFlowResponse, string]>([
    ["taken", { status: "taken" }, "This Minecraft account is already linked in afkfleet."],
    ["over_quota", { status: "over_quota" }, START_OVER_QUOTA],
  ])("offers only the way back after %s", async (_state, ended, text) => {
    const { app } = await linkScreen({ linkStatus: statuses(ended) });

    await app.user.click(screen.getByRole("button", { name: "Get a code" }));

    expect(await screen.findByText(text)).toBeDefined();
    expect(screen.queryByRole("button", { name: "Get a new code" })).toBeNull();
    expect(screen.getByRole("link", { name: "Back to accounts" })).toBeDefined();
  });

  it("keeps reading a state this version doesn't know", async () => {
    const { app, time } = await linkScreen({
      linkStatus: statuses({ status: "brand_new" as LinkFlowResponse["status"] }),
    });
    await getCode(app);
    expect(await screen.findByText('The server reports "brand_new".')).toBeDefined();
    const reads = calls(app, "linkStatus");

    await time.advance(2000);

    await vi.waitFor(() => expect(calls(app, "linkStatus")).toBeGreaterThan(reads));
  });

  it("waits what a 429 on a status read asks before reading again", async () => {
    const { app, time } = await linkScreen({
      linkStatus: statuses(WAITING, limited(5), WAITING),
    });
    await getCode(app);
    await screen.findByText(/^Waiting for you/);
    await time.advance(2000);
    await vi.waitFor(() => expect(calls(app, "linkStatus")).toBe(2));

    await time.advance(2000);
    expect(calls(app, "linkStatus")).toBe(2);
    await time.advance(3000);

    await vi.waitFor(() => expect(calls(app, "linkStatus")).toBe(3));
  });

  it("shows a status read that failed, and tries again on request", async () => {
    const { app } = await linkScreen({
      linkStatus: statuses(refusal(500, "internal"), WAITING),
    });

    await getCode(app);

    expect(await screen.findByText(/refused: internal \(request /)).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText(/^Waiting for you/)).toBeDefined();
  });

  describe("a refused start", () => {
    it.each<[string, Error, string]>([
      ["409", refusal(409, "conflict"), START_OVER_QUOTA],
      ["a bad link", fakeApiError("invalid_verification_link"), LINK_NOT_MICROSOFTS],
      ["500", refusal(500, "internal"), "Couldn't get a code: refused: internal"],
    ])("says %s", async (_case, error, text) => {
      const { app } = await linkScreen({
        startLink: async () => {
          throw error;
        },
      });

      await app.user.click(screen.getByRole("button", { name: "Get a code" }));

      expect(await screen.findByText(text, { exact: false })).toBeDefined();
      expect(screen.queryByText(USER_CODE)).toBeNull();
      expect(calls(app, "openVerificationLink")).toBe(0);
    });

    it.each<[string, Error, string]>([
      ["429", limited(3), START_LIMITED],
      ["503", refusal(503, "busy", { retry_after_secs: 3 }), START_BUSY],
    ])("waits out a %s with its own text", async (_case, error, text) => {
      const { app, time } = await linkScreen({
        startLink: async () => {
          throw error;
        },
      });

      await app.user.click(screen.getByRole("button", { name: "Get a code" }));

      expect(await screen.findByText(text, { exact: false })).toBeDefined();
      expect(screen.getByText("Try again in 3 s.")).toBeDefined();
      expect(
        (screen.getByRole("button", { name: "Get a code" }) as HTMLButtonElement).disabled,
      ).toBe(true);
      await time.advance(3000);
      expect(
        (screen.getByRole("button", { name: "Get a code" }) as HTMLButtonElement).disabled,
      ).toBe(false);
    });

    it("words a failure of some other kind with describeError", () => {
      expect(startFailed("x")).toBe("Couldn't get a code: x");
    });
  });

  describe("opening Microsoft's page", () => {
    it("tells the link to open by hand when Windows refuses", async () => {
      const { app } = await linkScreen({
        openVerificationLink: async () => {
          throw fakeApiError("verification_link_open");
        },
      });

      await getCode(app);

      expect(await screen.findByText(couldntOpen(VERIFICATION_URI))).toBeDefined();
    });

    it("says there's nothing to open any more", async () => {
      let opens = 0;
      const { app } = await linkScreen({
        openVerificationLink: async () => {
          opens += 1;
          if (opens > 1) {
            throw fakeApiError("no_verification_link");
          }
        },
      });
      await getCode(app);

      await app.user.click(screen.getByRole("button", { name: "Open Microsoft's page" }));

      expect(await screen.findByText(NOTHING_TO_OPEN)).toBeDefined();
    });
  });

  describe("linking an account again", () => {
    async function linkAgain(handlers: Partial<ApiClient>, changes = {}) {
      const time = fakeTime();
      const app = await atAccount(
        accountId(1),
        {
          account: async () => account(1, changes),
          relinkAccount: async () => STARTED,
          openVerificationLink: async () => {},
          linkStatus: statuses(WAITING),
          ...handlers,
        },
        { again: true, advanceTimers: time.advanceTimers },
      );
      await screen.findByRole("heading", { name: "Link AfkBot1 again", level: 1 });
      return { app, time };
    }

    it("starts a link again of that account and says when it's linked again", async () => {
      const { app } = await linkAgain({ linkStatus: statuses(linked(1, "active")) });

      expect(
        screen.getByText(
          "Sign in with the Microsoft account that owns AfkBot1. Another account is refused.",
        ),
      ).toBeDefined();
      await app.user.click(screen.getByRole("button", { name: "Get a code" }));

      expect(await screen.findByText("AfkBot1 is linked again.")).toBeDefined();
      expect(app.fake.calls.find((call) => call.method === "relinkAccount")?.args).toEqual([
        accountId(1),
      ]);
      expect(calls(app, "startLink")).toBe(0);
    });

    it("says another profile changed nothing", async () => {
      const { app } = await linkAgain({
        linkStatus: statuses({ status: "refused", reason: "profile_mismatch" }),
      });

      await app.user.click(screen.getByRole("button", { name: "Get a code" }));

      expect(await screen.findByText(reasonText("profile_mismatch", "AfkBot1"))).toBeDefined();
    });

    it.each<[string, object, string]>([
      ["pending", { status: "pending" }, START_PENDING],
      ["offline", { kind: "offline" }, START_OFFLINE],
    ])("tells a 409 for a %s account", async (_case, changes, text) => {
      const { app } = await linkAgain(
        {
          relinkAccount: async () => {
            throw refusal(409, "conflict");
          },
        },
        changes,
      );

      await app.user.click(screen.getByRole("button", { name: "Get a code" }));

      expect(await screen.findByText(text)).toBeDefined();
    });
  });

  describe("the code stays out of the caches", () => {
    it("after a link that ended", async () => {
      const { app, time } = await linkScreen({ linkStatus: statuses(WAITING, linked(5)) });
      await getCode(app);
      await time.advance(2000);
      await screen.findByText(/is linked/);

      expect(cacheText(app)).not.toContain(USER_CODE);
    });

    it("after leaving a link midway", async () => {
      const { app } = await linkScreen({});
      await getCode(app);

      await app.user.click(screen.getByRole("link", { name: "Back to accounts" }));

      expect(cacheText(app)).not.toContain(USER_CODE);
      expect(screen.queryByText(USER_CODE)).toBeNull();
    });

    it("after a link again", async () => {
      const time = fakeTime();
      const app = await atAccount(
        accountId(1),
        {
          account: async () => account(1),
          relinkAccount: async () => STARTED,
          openVerificationLink: async () => {},
          linkStatus: statuses(WAITING),
        },
        { again: true, advanceTimers: time.advanceTimers },
      );
      await app.user.click(await screen.findByRole("button", { name: "Get a code" }));
      await screen.findByText(USER_CODE);

      expect(cacheText(app)).not.toContain(USER_CODE);
    });
  });

  it("drops a start's answer that comes after the user left", async () => {
    const answer = deferred<typeof STARTED>();
    const { app } = await linkScreen({ startLink: () => answer.promise });
    await app.user.click(screen.getByRole("button", { name: "Get a code" }));
    await app.user.click(screen.getByRole("link", { name: "Back to accounts" }));

    await act(async () => answer.resolve(STARTED));

    expect(screen.queryByText(USER_CODE)).toBeNull();
    expect(calls(app, "openVerificationLink")).toBe(0);
  });
});
