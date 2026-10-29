// The Minecraft servers' list (Plan.md P11.9; ADR-0020, U57, R136): every page read, 10 at a
// time, each entry with its count and its link, "Add a server", and the empty, loading and
// failed states.
import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ServerSettingsPage } from "../../generated/ServerSettingsPage";
import { deferred, me } from "../../test/fixtures";
import { settings, settingsId } from "../../test/servers";
import { atScreen } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ADMIN = me("admin", "afkadmin");

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
    expect(
      screen.getByText("Settings here apply to every bot on that server, whoever's bot it is."),
    ).toBeDefined();
    const items = within(screen.getByRole("list", { name: "Minecraft servers" })).getAllByRole(
      "listitem",
    );
    expect(items).toHaveLength(4);
    const [formatsOnly, both, first, second] = items;
    // Only the parts a row has (the user's change at H7's plan review).
    expect(within(formatsOnly as HTMLElement).getByText("2 chat formats")).toBeDefined();
    expect(within(both as HTMLElement).getByText("1 conflict text · 2 chat formats")).toBeDefined();
    expect(within(first as HTMLElement).getByText("play.example.com:25566")).toBeDefined();
    expect(within(first as HTMLElement).getByText("3 conflict texts")).toBeDefined();
    expect(within(second as HTMLElement).getByText("mc.example.com:25565")).toBeDefined();
    expect(within(second as HTMLElement).getByText("1 conflict text")).toBeDefined();
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

  it("says when no server has settings, and Add a server opens a new one", async () => {
    const app = await atScreen(
      "/admin/servers",
      { serverSettings: async () => ({ items: [] }) },
      { user: ADMIN },
    );

    expect(await screen.findByText("No server has settings yet.")).toBeDefined();
    await app.user.click(screen.getByRole("link", { name: "Add a server" }));
    await vi.waitFor(() => expect(app.path()).toBe("/admin/servers/new"));
  });

  it("says so while the servers load", async () => {
    const answer = deferred<ServerSettingsPage>();
    await atScreen("/admin/servers", { serverSettings: () => answer.promise }, { user: ADMIN });

    expect(await screen.findByText("Loading servers…")).toBeDefined();
  });

  it("says why when the servers couldn't be read", async () => {
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
    expect(alert.textContent).toMatch(/^Couldn't load the servers: refused: internal/);
  });
});
