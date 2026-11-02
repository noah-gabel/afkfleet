// "Share with" suggesting users as they're typed (Plan.md P11.11; ADR-0020, U110, S127): an ARIA 1.2
// combobox over the server's suggestions. Picking only fills the field, and typing an exact name and
// pressing Share still works, whatever becomes of the suggestions.
import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { account, accountId } from "../../test/accounts";
import { fakeTime } from "../../test/renderApp";
import { atAccount } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ID = accountId(1);

const suggest =
  (...usernames: string[]): ApiClient["grantCandidates"] =>
  async (_id, q) => ({
    usernames: usernames.filter((name) => name.startsWith(q.toLowerCase())),
  });

afterEach(() => {
  vi.useRealTimers();
});

/** Account 1's screen, its suggestions from `handlers`, the Username field ready. */
async function atShare(
  handlers: Partial<ApiClient> = {},
  shown = account(1),
  advanceTimers?: (ms: number) => unknown,
) {
  const app = await atAccount(
    ID,
    {
      account: async () => shown,
      accountGrants: async () => ({ items: [] }),
      grantCandidates: suggest("bob", "bobby", "carol"),
      ...handlers,
    },
    advanceTimers === undefined ? {} : { advanceTimers },
  );
  const field = (await screen.findByRole("combobox", { name: "Username" })) as HTMLInputElement;
  return { app, field };
}

const list = () => screen.findByRole("listbox", { name: "Matching users" });
const options = async () =>
  within(await list())
    .getAllByRole("option")
    .map((option) => option.textContent);
const reads = (app: Awaited<ReturnType<typeof atShare>>["app"]) =>
  app.fake.calls.filter((call) => call.method === "grantCandidates");

/** The cached suggestions' queries. */
const candidateQueries = (app: Awaited<ReturnType<typeof atShare>>["app"]) =>
  app.runtime.queryClient.getQueryCache().findAll({ queryKey: ["accounts", "grant-candidates"] });

describe("the Share with field", () => {
  it("is a combobox that says it suggests, closed until something's typed", async () => {
    const { app, field } = await atShare();

    expect(field.getAttribute("aria-autocomplete")).toBe("list");
    expect(field.getAttribute("aria-expanded")).toBe("false");
    expect(field.getAttribute("autocomplete")).toBe("off");
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(reads(app)).toEqual([]);
  });

  it("lists what the server suggests, from the first character, as options", async () => {
    const { app, field } = await atShare();

    await app.user.type(field, "b");

    expect(await options()).toEqual(["bob", "bobby"]);
    expect(field.getAttribute("aria-expanded")).toBe("true");
    expect(field.getAttribute("aria-controls")).toBe((await list()).id);
    expect(reads(app).at(-1)?.args).toEqual([ID, "b"]);
  });

  it("waits for a pause in the typing before asking", async () => {
    const { app, field } = await atShare();

    await app.user.type(field, "bob");

    await list();
    expect(reads(app).map((call) => call.args[1])).toEqual(["bob"]);
  });

  it("moves through the options by the arrows, and wraps, the focus staying in the field", async () => {
    const { app, field } = await atShare();
    await app.user.type(field, "b");
    await list();
    const active = () =>
      document.getElementById(field.getAttribute("aria-activedescendant") ?? "")?.textContent;

    await app.user.keyboard("{ArrowDown}");
    expect(active()).toBe("bob");
    expect(document.activeElement).toBe(field);
    await app.user.keyboard("{ArrowDown}");
    expect(active()).toBe("bobby");
    await app.user.keyboard("{ArrowDown}");
    expect(active()).toBe("bob");
    await app.user.keyboard("{ArrowUp}");
    expect(active()).toBe("bobby");
    expect(screen.getByRole("option", { name: "bobby" }).getAttribute("aria-selected")).toBe(
      "true",
    );
  });

  it("fills the field by Enter on an active option, without sending anything", async () => {
    const { app, field } = await atShare();
    await app.user.type(field, "b");
    await list();

    await app.user.keyboard("{ArrowDown}{ArrowDown}{Enter}");

    expect(field.value).toBe("bobby");
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(app.fake.calls.some((call) => call.method === "createGrant")).toBe(false);
    expect(document.activeElement).toBe(field);
  });

  it("fills the field by a click, keeping the focus, without sending anything", async () => {
    const { app, field } = await atShare();
    await app.user.type(field, "b");
    await list();

    await app.user.click(screen.getByRole("option", { name: "bob" }));

    expect(field.value).toBe("bob");
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(document.activeElement).toBe(field);
    expect(app.fake.calls.some((call) => call.method === "createGrant")).toBe(false);
  });

  it("closes on Escape and keeps what's typed, and reopens on more typing", async () => {
    const { app, field } = await atShare();
    await app.user.type(field, "b");
    await list();

    await app.user.keyboard("{Escape}");

    expect(screen.queryByRole("listbox")).toBeNull();
    expect(field.value).toBe("b");
    expect(field.getAttribute("aria-activedescendant")).toBeNull();
    await app.user.type(field, "o");
    expect(await options()).toEqual(["bob", "bobby"]);
  });

  it("closes when the focus leaves", async () => {
    const { app, field } = await atShare();
    await app.user.type(field, "b");
    await list();

    await app.user.tab();

    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  });

  it("sends the form on Enter with no option active", async () => {
    const createGrant = vi.fn(async () => {
      throw refusal(409, "conflict");
    });
    const { app, field } = await atShare({ createGrant });
    await app.user.type(field, "bob");
    await list();

    await app.user.keyboard("{Enter}");

    await waitFor(() => expect(createGrant).toHaveBeenCalledTimes(1));
    expect(createGrant.mock.calls.at(0)?.at(1)).toMatchObject({ username: "bob" });
  });

  it("shows no list when nothing matches", async () => {
    const { app, field } = await atShare({ grantCandidates: suggest() });

    await app.user.type(field, "z");

    await waitFor(() => expect(reads(app)).toHaveLength(1));
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(field.getAttribute("aria-expanded")).toBe("false");
  });

  it("asks nothing for what can't start a username, past the pause too", async () => {
    const time = fakeTime();
    const { app, field } = await atShare({}, account(1), time.advanceTimers);

    await app.user.type(field, "_b");
    await act(() => vi.advanceTimersByTimeAsync(1000));

    expect(field.value).toBe("_b");
    expect(reads(app)).toEqual([]);
    // Not even a refused read: nothing of it stays to fail.
    expect(candidateQueries(app).filter((query) => query.state.status === "error")).toEqual([]);
  });

  it("asks nothing on a pending account, which takes no grant, past the pause too", async () => {
    const time = fakeTime();
    const { app, field } = await atShare({}, account(1, { status: "pending" }), time.advanceTimers);

    await app.user.type(field, "b");
    await act(() => vi.advanceTimersByTimeAsync(1000));

    expect(reads(app)).toEqual([]);
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("keeps nothing of what was typed in the cache once it's gone from the screen", async () => {
    const { app, field } = await atShare();
    await app.user.type(field, "bob");
    await list();
    expect(candidateQueries(app).length).toBeGreaterThan(0);

    await act(() => app.runtime.router.navigate({ to: "/modes" }));

    await waitFor(() => expect(candidateQueries(app)).toEqual([]));
  });

  it.each([
    ["a 429", () => refusal(429, "rate_limited", { retry_after_secs: 60 })],
    ["a 403", () => refusal(403, "forbidden")],
    ["a server's failure", () => refusal(500, "internal")],
  ])("shows nothing after %s, and an exact name still shares", async (_name, failure) => {
    const createGrant = vi.fn(async () => {
      throw refusal(409, "conflict");
    });
    const { app, field } = await atShare({
      grantCandidates: async () => {
        throw failure();
      },
      createGrant,
    });

    await app.user.type(field, "bob");
    await waitFor(() => expect(reads(app).length).toBeGreaterThan(0));

    expect(screen.queryByRole("listbox")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    await app.user.click(screen.getByRole("button", { name: "Share" }));
    await waitFor(() => expect(createGrant).toHaveBeenCalledTimes(1));
  });

  it("keeps earlier suggestions only where they still match what's typed", async () => {
    let answer: (value: { usernames: string[] }) => void = () => {};
    const { app, field } = await atShare({
      grantCandidates: async (_id, q) => {
        if (q === "b") {
          return { usernames: ["bob", "bea"] };
        }
        return new Promise((resolve) => {
          answer = resolve;
        });
      },
    });
    await app.user.type(field, "b");
    expect(await options()).toEqual(["bob", "bea"]);

    await app.user.type(field, "o");

    // At once, before the pause has even asked: "bea" no longer starts with what's typed.
    expect(
      within(screen.getByRole("listbox"))
        .getAllByRole("option")
        .map((option) => option.textContent),
    ).toEqual(["bob"]);
    await waitFor(() => expect(reads(app).map((call) => call.args[1])).toEqual(["b", "bo"]));
    answer({ usernames: ["bob"] });
  });
});
