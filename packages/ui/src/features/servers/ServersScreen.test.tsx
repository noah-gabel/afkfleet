// The Minecraft servers' list (Plan.md P11.9; ADR-0020, U57, R136): every page read, 10 at a
// time, each entry with its count and its link, "Add server", and the empty, loading and
// failed states.
import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ServerSettingsPage } from "../../generated/ServerSettingsPage";
import { deferred, me } from "../../test/fixtures";
import { settings, settingsId } from "../../test/servers";
import { atScreen } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ADMIN = me("admin", "afkadmin");

/** The labelled facts on a card: each label and its value. */
function facts(card: HTMLElement): Record<string, string | null | undefined> {
  return Object.fromEntries(
    [...card.querySelectorAll("dt")].map((term) => [
      term.textContent,
      term.nextElementSibling?.textContent,
    ]),
  );
}

describe("the Minecraft servers' list", () => {
  it("lists each server's settings with its count, each opening its own screen", async () => {
    const app = await atScreen(
      "/admin/servers",
      {
        serverSettings: async () => ({
          items: [
            settings(4, "chat.example.com:25565", [], 1, ["{sender} : ", "<{sender}> "]),
            settings(3, "both.example.com:25565", [["One"]], 1, ["{sender} : ", "<{sender}> "]),
            settings(2, "play.example.com:25566", [["One"], ["Two", "lines"], ["Three"]]),
            settings(1, "mc.example.com:25565", [["Already connected"]]),
          ],
        }),
      },
      { user: ADMIN },
    );

    expect(
      await screen.findByRole("heading", { name: "Minecraft servers", level: 1 }),
    ).toBeDefined();
    expect(screen.getByText("Apply to every bot on that server, whoever owns it.")).toBeDefined();
    const items = within(screen.getByRole("list", { name: "Minecraft servers" })).getAllByRole(
      "listitem",
    );
    expect(items).toHaveLength(4);
    const [formatsOnly, both, first, second] = items;
    // Only the parts a row has (the user's change at H7's plan review), each labelled (U119).
    expect(facts(formatsOnly as HTMLElement)).toEqual({ "Chat formats": "2" });
    expect(facts(both as HTMLElement)).toEqual({ "Conflict texts": "1", "Chat formats": "2" });
    expect(within(first as HTMLElement).getByText("play.example.com:25566")).toBeDefined();
    expect(facts(first as HTMLElement)).toEqual({ "Conflict texts": "3" });
    expect(within(second as HTMLElement).getByText("mc.example.com:25565")).toBeDefined();
    expect(facts(second as HTMLElement)).toEqual({ "Conflict texts": "1" });
    expect(screen.getByRole("list", { name: "Minecraft servers" }).className).toContain("grid");
    await app.user.click(screen.getByRole("link", { name: "Open mc.example.com:25565" }));
    await vi.waitFor(() => expect(app.path()).toBe(`/admin/servers/${settingsId(1)}`));
  });

  it("reads every page, 10 at a time", async () => {
    const first: ServerSettingsPage = {
      items: [settings(2, "two.example.com:25565", [["One"]])],
      next: settingsId(2),
    };
    const second: ServerSettingsPage = { items: [settings(1, "one.example.com:25565", [["One"]])] };
    const app = await atScreen(
      "/admin/servers",
      { serverSettings: async (before) => (before === undefined ? first : second) },
      { user: ADMIN },
    );

    expect(await screen.findByText("one.example.com:25565")).toBeDefined();
    expect(screen.getByText("two.example.com:25565")).toBeDefined();
    expect(
      app.fake.calls.filter((call) => call.method === "serverSettings").map((call) => call.args),
    ).toEqual([
      [undefined, 10],
      [settingsId(2), 10],
    ]);
  });

  it("says when no server has settings, and Add server opens a new one", async () => {
    const app = await atScreen(
      "/admin/servers",
      { serverSettings: async () => ({ items: [] }) },
      { user: ADMIN },
    );

    expect(await screen.findByText("No servers.")).toBeDefined();
    const links = screen.getAllByRole("link", { name: "Add server" });
    expect(links).toHaveLength(1);
    expect(links[0]?.closest("[data-slot=screen-state]")).not.toBeNull();
    await app.user.click(links[0] as HTMLElement);
    await vi.waitFor(() => expect(app.path()).toBe("/admin/servers/new"));
  });

  it("says so while the servers load", async () => {
    const answer = deferred<ServerSettingsPage>();
    await atScreen("/admin/servers", { serverSettings: () => answer.promise }, { user: ADMIN });

    expect((await screen.findByText("Loading servers…")).getAttribute("role")).toBe("status");
    expect(screen.queryByRole("link", { name: "Add server" })).toBeNull();
  });

  it("says why when the servers couldn't be read, and keeps Add server (PR #92's review)", async () => {
    await atScreen(
      "/admin/servers",
      {
        serverSettings: async () => {
          throw refusal(500, "internal");
        },
      },
      { user: ADMIN },
    );

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/^Couldn't load servers: refused: internal/);
    expect(screen.getByRole("link", { name: "Add server" })).toBeDefined();
  });
});
