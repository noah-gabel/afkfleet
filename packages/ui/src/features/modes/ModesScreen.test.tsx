// The modes' list (Plan.md P11.9; ADR-0020, U74, S14, S68): every mode the viewer may see, in the
// API's order, each card with whose it is, who sees it, its steps and its mark, and every page
// read at once. Every role gets it.
import { act, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PAGE_MAX } from "../../api/limits";
import type { ModePage } from "../../generated/ModePage";
import { deferred, me } from "../../test/fixtures";
import { mode, modeId } from "../../test/live";
import { atScreen } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const STEP = {
  action: { type: "jump", arguments_hidden: false },
  schedule: { type: "at_start" },
  probability: 100,
} as const;

/** The labelled facts on a card: each label and its value. */
function facts(card: HTMLElement): Record<string, string | null | undefined> {
  return Object.fromEntries(
    [...card.querySelectorAll("dt")].map((term) => [
      term.textContent,
      term.nextElementSibling?.textContent,
    ]),
  );
}

describe("the modes' list", () => {
  it("shows every mode in the API's order, with whose it is, who sees it and its steps", async () => {
    await atScreen("/modes", {
      modes: async () => ({
        items: [
          mode(3, { name: "Yours", steps: [STEP, STEP] }),
          mode(2, {
            name: "Theirs",
            visibility: "shared",
            ownership: "someone_else",
            steps: [STEP],
          }),
          mode(1, { name: "afk", visibility: "shared", ownership: "built_in" }),
        ],
      }),
    });

    const list = await screen.findByRole("list", { name: "Modes" });
    const [yours, theirs, builtIn] = within(list).getAllByRole("listitem") as HTMLElement[];

    expect(facts(yours as HTMLElement)).toEqual({
      Owner: "Yours",
      Visibility: "Private",
      Steps: "2",
    });
    expect(facts(theirs as HTMLElement)).toEqual({
      Owner: "Someone else's",
      Visibility: "Shared",
      Steps: "1",
    });
    expect(facts(builtIn as HTMLElement)).toEqual({
      Owner: "Built in",
      Visibility: "Shared",
      Steps: "0",
    });
    expect(screen.getByText("What a bot does while online.")).toBeDefined();
    expect(list.className).toContain("grid");
  });

  it("names the owner only where the server sent one", async () => {
    await atScreen("/modes", {
      modes: async () => ({
        items: [
          mode(1, {
            ownership: "someone_else",
            owner: { id: "01900000-0000-7000-8000-000000000001", username: "afkadmin" },
          }),
        ],
      }),
    });

    const card = (await screen.findByRole("link", { name: "Open Mode 1" })).closest("li");
    expect(facts(card as HTMLElement).Owner).toBe("afkadmin");
  });

  it("marks a mode that sends a command off the allowlist, outside its name", async () => {
    await atScreen("/modes", {
      modes: async () => ({
        items: [mode(2, { name: "Warp", unlisted_command: true }), mode(1, { name: "Plain" })],
      }),
    });

    const list = await screen.findByRole("list", { name: "Modes" });
    const [warp, plain] = within(list).getAllByRole("listitem") as HTMLElement[];
    const mark = "Sends a command off the allowlist. Needs Manage to choose.";

    expect(within(warp as HTMLElement).getByText(mark)).toBeDefined();
    expect(within(plain as HTMLElement).queryByText(mark)).toBeNull();
  });

  it("opens a mode from its card, and a new one from New mode", async () => {
    const app = await atScreen("/modes", {
      modes: async () => ({ items: [mode(1, { name: "Fishing" })] }),
    });

    await app.user.click(await screen.findByRole("link", { name: "Open Fishing" }));
    await vi.waitFor(() => expect(app.path()).toBe(`/modes/${modeId(1)}`));
    await act(() => app.runtime.router.navigate({ to: "/modes" }));
    await app.user.click(await screen.findByRole("link", { name: "New mode" }));

    await vi.waitFor(() => expect(app.path()).toBe("/modes/new"));
  });

  it("reads every page at once", async () => {
    const first: ModePage = { items: [mode(2, { name: "Two" })], next: modeId(2) };
    const second: ModePage = { items: [mode(1, { name: "One" })] };
    const app = await atScreen("/modes", {
      modes: async (before) => (before === undefined ? first : second),
    });

    expect(await screen.findByRole("link", { name: "Open One" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Open Two" })).toBeDefined();
    expect(
      app.fake.calls.filter((call) => call.method === "modes").map((call) => call.args),
    ).toContainEqual([modeId(2), PAGE_MAX]);
  });

  it("says so while the modes load", async () => {
    const answer = deferred<ModePage>();
    await atScreen("/modes", { modes: () => answer.promise });

    expect((await screen.findByText("Loading modes…")).getAttribute("role")).toBe("status");
    expect(screen.queryByRole("link", { name: "New mode" })).toBeNull();
  });

  it("says there are none, with the way to a new one", async () => {
    const app = await atScreen("/modes", { modes: async () => ({ items: [] }) });

    expect(await screen.findByText("No modes.")).toBeDefined();
    expect(screen.queryByRole("list", { name: "Modes" })).toBeNull();
    const links = screen.getAllByRole("link", { name: "New mode" });
    expect(links).toHaveLength(1);
    expect(links[0]?.closest("[data-slot=screen-state]")).not.toBeNull();
    await app.user.click(links[0] as HTMLElement);
    await vi.waitFor(() => expect(app.path()).toBe("/modes/new"));
  });

  it("says why when the modes couldn't be read, and keeps New mode (PR #92's review)", async () => {
    await atScreen("/modes", {
      modes: async () => {
        throw refusal(500, "internal");
      },
    });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/^Couldn't load modes: refused: internal/);
    expect(screen.getByRole("link", { name: "New mode" })).toBeDefined();
  });

  it("is in the nav for a Member, right after Home", async () => {
    await atScreen("/modes", { modes: async () => ({ items: [] }) }, { user: me("member") });

    const links = within(screen.getByRole("navigation", { name: "Main" })).getAllByRole("link");

    expect(links.map((link) => link.textContent).slice(0, 2)).toEqual(["Home", "Modes"]);
  });
});
