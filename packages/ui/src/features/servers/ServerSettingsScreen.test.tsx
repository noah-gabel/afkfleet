// One Minecraft server's settings (Plan.md P11.9; ADR-0020, U57, U61, R136, S77): the form
// filled from the row, every request built from the form, the writes through "Confirm it's
// you", Add opening the new entry and Save staying, the 409 with "Load again", the 404, the
// cap, the field errors, the most texts, and Delete asking first.
import { act, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import type { ServerSettingsPage } from "../../generated/ServerSettingsPage";
import type { ServerSettingsResponse } from "../../generated/ServerSettingsResponse";
import { deferred, me } from "../../test/fixtures";
import type { RenderedApp } from "../../test/renderApp";
import { settings, settingsId } from "../../test/servers";
import { signedIn } from "../../test/signedIn";
import { PASSWORD, refusal } from "../../test/signIn";
import { TEXTS_HINT } from "./texts";

const ADMIN = me("admin", "afkadmin");
const CHANGED = "These settings changed since you opened them. Load them again to see the change.";

/** The key the server answers for `server`: lowercase, the port always written. */
function keyOf(server: string): string {
  const lower = server.toLowerCase();
  return lower.includes(":") ? lower : `${lower}:25565`;
}

/**
 * The App on settings `id`'s screen, or a new server's (`"new"`), over a fake that keeps
 * `rows` and asks for the password until `stepUp` ran (unless `stamped`).
 */
async function atEditor(
  id: string,
  options: {
    readonly rows?: ServerSettingsResponse[];
    readonly stamped?: boolean;
    readonly handlers?: Partial<ApiClient>;
  } = {},
) {
  const state = { rows: [...(options.rows ?? [])], stamped: options.stamped ?? true };
  const needs = () => {
    if (!state.stamped) {
      throw refusal(403, "step_up_required");
    }
  };
  const app = await signedIn(
    {
      serverSettings: async () => ({ items: state.rows }),
      stepUp: async () => {
        state.stamped = true;
      },
      createServerSettings: async (request) => {
        needs();
        const made = settings(
          9,
          keyOf(request.server),
          request.conflict_texts.map((text) => text.split("\n")),
        );
        state.rows = [made, ...state.rows];
        return made;
      },
      updateServerSettings: async (rowId, request) => {
        needs();
        const row = state.rows.find((one) => one.id === rowId);
        if (row === undefined) {
          throw refusal(404, "not_found");
        }
        if (row.version !== request.version) {
          throw refusal(409, "server_settings_changed", { message: CHANGED });
        }
        const edited: ServerSettingsResponse = {
          ...row,
          server: keyOf(request.server),
          conflict_texts: request.conflict_texts.map((text) => ({ lines: text.split("\n") })),
          version: row.version + 1,
        };
        state.rows = state.rows.map((one) => (one.id === rowId ? edited : one));
        return edited;
      },
      deleteServerSettings: async (rowId) => {
        needs();
        if (!state.rows.some((one) => one.id === rowId)) {
          throw refusal(404, "not_found");
        }
        state.rows = state.rows.filter((one) => one.id !== rowId);
      },
      ...options.handlers,
    },
    { user: ADMIN },
  );
  await act(() =>
    id === "new"
      ? app.runtime.router.navigate({ to: "/admin/servers/new" })
      : app.runtime.router.navigate({ to: "/admin/servers/$id", params: { id } }),
  );
  return { app, state };
}

/** The arguments of every call of `method`. */
function calls(app: RenderedApp, method: string): readonly unknown[][] {
  return app.fake.calls.filter((call) => call.method === method).map((call) => [...call.args]);
}

/** Text `n`'s field, counted from 1. */
function text(n: number): HTMLTextAreaElement {
  return screen.getByLabelText(`Conflict text ${n}`) as HTMLTextAreaElement;
}

/** The screen's own content, without the navigation. */
function main(): HTMLElement {
  return screen.getByRole("main");
}

const ROW = settings(1, "mc.example.com:25565", [
  ["Already connected"],
  ["You logged in", "from another location"],
]);

describe("a Minecraft server's settings", () => {
  it("fills the form from the row: its key, and each text's lines joined by line breaks", async () => {
    await atEditor(ROW.id, { rows: [ROW] });

    expect(
      await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 }),
    ).toBeDefined();
    expect((screen.getByLabelText("Server address") as HTMLInputElement).value).toBe(
      "mc.example.com:25565",
    );
    expect(text(1).value).toBe("Already connected");
    expect(text(2).value).toBe("You logged in\nfrom another location");
    expect(screen.getByText(TEXTS_HINT)).toBeDefined();
    expect(screen.getByText("Host or host:port, e.g. play.example.com")).toBeDefined();
  });

  it("adds a server after Confirm it's you, then opens its own screen (U61)", async () => {
    const { app } = await atEditor("new", { stamped: false });

    expect(await screen.findByRole("heading", { name: "Add a server", level: 1 })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
    await app.user.type(screen.getByLabelText("Server address"), "MC.example.com");
    await app.user.type(text(1), "Two{Enter}lines");
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("Added settings for mc.example.com:25565.")).toBeDefined();
    await vi.waitFor(() => expect(app.path()).toBe(`/admin/servers/${settingsId(9)}`));
    expect(
      await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 }),
    ).toBeDefined();
    expect(text(1).value).toBe("Two\nlines");
    const request = { server: "MC.example.com", conflict_texts: ["Two\nlines"] };
    expect(calls(app, "createServerSettings")).toEqual([[request], [request]]);
  });

  it("saves and stays, sending the form as it is at the version read, and asks once", async () => {
    const { app } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    await app.user.clear(text(1));
    await app.user.type(text(1), "One");
    await app.user.click(screen.getByRole("button", { name: "Add text" }));
    await app.user.type(text(3), "Three");
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    expect(
      await screen.findByText("Saved. Bots on this server use the change at once."),
    ).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(() => expect(calls(app, "updateServerSettings")).toHaveLength(2));

    expect(app.path()).toBe(`/admin/servers/${ROW.id}`);
    const texts = ["One", "You logged in\nfrom another location", "Three"];
    expect(calls(app, "updateServerSettings")).toEqual([
      [ROW.id, { version: 1, server: "mc.example.com:25565", conflict_texts: texts }],
      [ROW.id, { version: 2, server: "mc.example.com:25565", conflict_texts: texts }],
    ]);
    expect(calls(app, "stepUp")).toEqual([]);
  });

  it("shows the server's text when the settings changed, and Load again reads them again", async () => {
    const { app, state } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });
    state.rows = [settings(1, "mc.example.com:25565", [["Someone else's"]], 2)];

    await app.user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(CHANGED)).toBeDefined();
    expect(text(1).value).toBe("Already connected");
    await app.user.click(screen.getByRole("button", { name: "Load again" }));
    await vi.waitFor(() => expect(text(1).value).toBe("Someone else's"));
    expect(screen.queryByText(CHANGED)).toBeNull();
    expect(screen.queryByLabelText("Conflict text 2")).toBeNull();
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(() => expect(calls(app, "updateServerSettings")).toHaveLength(2));

    expect(calls(app, "updateServerSettings")[1]).toEqual([
      ROW.id,
      { version: 2, server: "mc.example.com:25565", conflict_texts: ["Someone else's"] },
    ]);
  });

  it("says the settings don't exist any more when a save finds none", async () => {
    const { app, state } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });
    state.rows = [];

    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("These settings don't exist any more.")).toBeDefined();
    expect(within(main()).getByRole("link", { name: "Minecraft servers" })).toBeDefined();
  });

  it("says the settings don't exist when the list doesn't hold them", async () => {
    await atEditor(settingsId(5), { rows: [ROW] });

    expect(await screen.findByText("These settings don't exist any more.")).toBeDefined();
    expect(screen.queryByLabelText("Server address")).toBeNull();
  });

  it("says so while the servers load", async () => {
    const answer = deferred<ServerSettingsPage>();
    await atEditor(ROW.id, { handlers: { serverSettings: () => answer.promise } });

    expect(await screen.findByText("Loading servers…")).toBeDefined();
  });

  it("says when the fleet holds the most servers' settings", async () => {
    const { app } = await atEditor("new", {
      handlers: {
        createServerSettings: async () => {
          throw refusal(409, "conflict");
        },
      },
    });
    await screen.findByRole("heading", { name: "Add a server", level: 1 });

    await app.user.type(screen.getByLabelText("Server address"), "mc.example.com");
    await app.user.type(text(1), "One");
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(
      await screen.findByText(
        "There are settings for 100 servers, the most there can be. Delete a server's settings to add another.",
      ),
    ).toBeDefined();
    expect(app.path()).toBe("/admin/servers/new");
  });

  it("puts each refused field's message at its own input", async () => {
    const { app } = await atEditor(ROW.id, {
      rows: [ROW],
      handlers: {
        updateServerSettings: async () => {
          throw refusal(422, "validation_failed", {
            fields: [
              { path: "server", message: "already has settings" },
              { path: "conflict_texts[1]", message: "must not be empty" },
            ],
          });
        },
      },
    });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("already has settings")).toBeDefined();
    expect(screen.getByText("must not be empty")).toBeDefined();
    expect(screen.getByLabelText("Server address").getAttribute("aria-invalid")).toBe("true");
    expect(text(1).getAttribute("aria-invalid")).toBe("false");
    expect(text(2).getAttribute("aria-invalid")).toBe("true");
  });

  it("shows a refusal of the list itself at the texts", async () => {
    const { app } = await atEditor(ROW.id, {
      rows: [ROW],
      handlers: {
        updateServerSettings: async () => {
          throw refusal(422, "validation_failed", {
            fields: [{ path: "conflict_texts", message: "must hold at least one text" }],
          });
        },
      },
    });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    for (const remove of screen.getAllByRole("button", { name: "Remove" }).reverse()) {
      await app.user.click(remove);
    }
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("must hold at least one text")).toBeDefined();
    expect(calls(app, "updateServerSettings")).toEqual([
      [ROW.id, { version: 1, server: "mc.example.com:25565", conflict_texts: [] }],
    ]);
  });

  it("refuses a text past 1024 characters before any call", async () => {
    const { app } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    await app.user.clear(text(1));
    await app.user.click(text(1));
    await app.user.paste("x".repeat(1025));
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("must be at most 1024 characters")).toBeDefined();
    expect(calls(app, "updateServerSettings")).toEqual([]);
  });

  it("stops Add text at 16 texts, and Remove takes a text out", async () => {
    const many = settings(
      1,
      "mc.example.com:25565",
      Array.from({ length: 15 }, (_, n) => [`Text ${n + 1}`]),
    );
    const { app } = await atEditor(many.id, { rows: [many] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    expect(screen.queryByText("At most 16 texts.")).toBeNull();
    await app.user.click(screen.getByRole("button", { name: "Add text" }));
    await app.user.type(text(16), "Text 16");
    expect((screen.getByRole("button", { name: "Add text" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(screen.getByText("At most 16 texts.")).toBeDefined();
    const second = screen.getAllByRole("button", { name: "Remove" })[1];
    await app.user.click(second as HTMLElement);
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(() => expect(calls(app, "updateServerSettings")).toHaveLength(1));

    const sent = Array.from({ length: 16 }, (_, n) => `Text ${n + 1}`).filter(
      (one) => one !== "Text 2",
    );
    expect(calls(app, "updateServerSettings")).toEqual([
      [many.id, { version: 1, server: "mc.example.com:25565", conflict_texts: sent }],
    ]);
  });

  it("deletes after asking and Confirm it's you, then goes back to the list", async () => {
    const { app } = await atEditor(ROW.id, { rows: [ROW], stamped: false });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    expect(
      screen.getByText(
        "Delete the settings of mc.example.com:25565? Its bots then treat no kick message as someone logging in.",
      ),
    ).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Delete settings" }));
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("Deleted the settings of mc.example.com:25565.")).toBeDefined();
    await vi.waitFor(() => expect(app.path()).toBe("/admin/servers"));
    expect(await screen.findByText("No server has settings yet.")).toBeDefined();
    expect(calls(app, "deleteServerSettings")).toEqual([[ROW.id], [ROW.id]]);
  });

  it("deletes nothing when the question is cancelled", async () => {
    const { app } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    const question = screen.getByText(/^Delete the settings of/).closest("div");
    await app.user.click(
      within(question?.parentElement as HTMLElement).getByRole("button", { name: "Cancel" }),
    );

    expect(screen.queryByText(/^Delete the settings of/)).toBeNull();
    expect(calls(app, "deleteServerSettings")).toEqual([]);
  });

  it("goes back to the list on Cancel, sending nothing", async () => {
    const { app } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    await app.user.type(text(1), " and more");
    await app.user.click(screen.getByRole("button", { name: "Cancel" }));

    await vi.waitFor(() => expect(app.path()).toBe("/admin/servers"));
    expect(calls(app, "updateServerSettings")).toEqual([]);
  });
});

describe("a Minecraft server's settings, PR #80's review", () => {
  it("keeps the notice and the edits when Load again can't read the servers", async () => {
    let failing = false;
    // The fake's rows once `atEditor` returns them; the first read comes before.
    const kept: { rows?: () => ServerSettingsResponse[] } = {};
    const { app, state } = await atEditor(ROW.id, {
      rows: [ROW],
      handlers: {
        serverSettings: async () => {
          if (failing) {
            throw refusal(500, "internal");
          }
          return { items: kept.rows?.() ?? [ROW] };
        },
      },
    });
    kept.rows = () => state.rows;
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });
    state.rows = [settings(1, "mc.example.com:25565", [["Someone else's"]], 2)];
    await app.user.clear(text(1));
    await app.user.type(text(1), "Mine");
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(CHANGED)).toBeDefined();

    failing = true;
    await app.user.click(screen.getByRole("button", { name: "Load again" }));

    const failed = await screen.findByText(/^Couldn't load the servers: /);
    expect(screen.getByText(CHANGED)).toBeDefined();
    expect(failed.closest('[role="alert"]')).toBe(
      screen.getByText(CHANGED).closest('[role="alert"]'),
    );
    expect(text(1).value).toBe("Mine");
    expect(text(2).value).toBe("You logged in\nfrom another location");
  });

  it("locks the fields, Remove and Add text while Confirm it's you is up, and the retry sends what's shown", async () => {
    const { app } = await atEditor(ROW.id, { rows: [ROW], stamped: false });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });
    await app.user.type(text(1), " now");
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByLabelText("Current password");

    const address = screen.getByLabelText("Server address") as HTMLInputElement;
    expect(address.readOnly).toBe(true);
    expect(text(1).readOnly).toBe(true);
    expect(text(2).readOnly).toBe(true);
    for (const remove of screen.getAllByRole("button", { name: "Remove" })) {
      expect((remove as HTMLButtonElement).disabled).toBe(true);
    }
    expect((screen.getByRole("button", { name: "Add text" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    await app.user.type(address, "x");
    await app.user.type(text(1), " later");
    await app.user.type(screen.getByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));
    expect(
      await screen.findByText("Saved. Bots on this server use the change at once."),
    ).toBeDefined();

    const shown = {
      version: 1,
      server: "mc.example.com:25565",
      conflict_texts: ["Already connected now", "You logged in\nfrom another location"],
    };
    expect(calls(app, "updateServerSettings")).toEqual([
      [ROW.id, shown],
      [ROW.id, shown],
    ]);
  });

  it("never lets a background read overwrite what's being typed, and saves it at the version read", async () => {
    const { app, state } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });
    await app.user.clear(screen.getByLabelText("Server address"));
    await app.user.type(screen.getByLabelText("Server address"), "play.example.com");
    await app.user.clear(text(1));
    await app.user.type(text(1), "Typed");
    state.rows = [settings(1, "other.example.com:25565", [["Someone else's"]], 2)];

    await act(() => app.runtime.queryClient.invalidateQueries({ queryKey: ["server-settings"] }));
    await vi.waitFor(() => expect(calls(app, "serverSettings")).toHaveLength(2));

    expect((screen.getByLabelText("Server address") as HTMLInputElement).value).toBe(
      "play.example.com",
    );
    expect(text(1).value).toBe("Typed");
    expect(text(2).value).toBe("You logged in\nfrom another location");
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(CHANGED)).toBeDefined();
    expect(calls(app, "updateServerSettings")).toEqual([
      [
        ROW.id,
        {
          version: 1,
          server: "play.example.com",
          conflict_texts: ["Typed", "You logged in\nfrom another location"],
        },
      ],
    ]);
  });

  it("describes each Remove by its text's label", async () => {
    await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });

    expect(
      screen.getByRole("button", { name: "Remove", description: "Conflict text 1" }),
    ).toBeDefined();
    expect(
      screen.getByRole("button", { name: "Remove", description: "Conflict text 2" }),
    ).toBeDefined();
  });

  it("adds a refusal of the list itself to the texts' description", async () => {
    const { app } = await atEditor(ROW.id, {
      rows: [ROW],
      handlers: {
        updateServerSettings: async () => {
          throw refusal(422, "validation_failed", {
            fields: [{ path: "conflict_texts", message: "must hold at most 16 texts" }],
          });
        },
      },
    });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });
    expect(
      screen.getByRole("group", { name: "Conflict texts", description: TEXTS_HINT }),
    ).toBeDefined();

    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText("must hold at most 16 texts");

    expect(
      screen.getByRole("group", {
        name: "Conflict texts",
        description: `${TEXTS_HINT} must hold at most 16 texts`,
      }),
    ).toBeDefined();
  });

  it("announces the changed settings through one live region", async () => {
    const { app, state } = await atEditor(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "mc.example.com:25565", level: 1 });
    state.rows = [settings(1, "mc.example.com:25565", [["Someone else's"]], 2)];

    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText(CHANGED);

    const regions = screen
      .getAllByRole("alert")
      .filter((region) => region.textContent?.includes(CHANGED) === true);
    expect(regions).toHaveLength(1);
    expect(within(regions[0] as HTMLElement).queryAllByRole("alert")).toEqual([]);
  });
});
