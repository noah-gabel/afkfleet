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

    expect(within(yours as HTMLElement).getByText("Yours · Private · 2 steps")).toBeDefined();
    expect(
      within(theirs as HTMLElement).getByText("Someone else's · Shared · 1 step"),
    ).toBeDefined();
    expect(within(builtIn as HTMLElement).getByText("Built in · Shared · No steps")).toBeDefined();
    expect(screen.getByText("A mode is what a bot does while it's online.")).toBeDefined();
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

    expect(await screen.findByText("afkadmin · Private · No steps")).toBeDefined();
  });

  it("marks a mode that sends a command off the allowlist, outside its name", async () => {
    await atScreen("/modes", {
      modes: async () => ({
        items: [mode(2, { name: "Warp", unlisted_command: true }), mode(1, { name: "Plain" })],
      }),
    });

    const list = await screen.findByRole("list", { name: "Modes" });
    const [warp, plain] = within(list).getAllByRole("listitem") as HTMLElement[];
    const mark =
      "Sends a command off the allowlist: only someone who manages a bot can choose this mode.";

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

    expect(await screen.findByText("Loading modes…")).toBeDefined();
  });

  it("says why when the modes couldn't be read", async () => {
    await atScreen("/modes", {
      modes: async () => {
        throw refusal(500, "internal");
      },
    });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/^Couldn't load the modes: refused: internal/);
  });

  it("is in the nav for a Member, right after Home", async () => {
    await atScreen("/modes", { modes: async () => ({ items: [] }) }, { user: me("member") });

    const links = within(screen.getByRole("navigation", { name: "Main" })).getAllByRole("link");

    expect(links.map((link) => link.textContent).slice(0, 2)).toEqual(["Home", "Modes"]);
  });
});
