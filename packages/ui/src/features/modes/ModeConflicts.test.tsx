// A mode's conflicts and its delete (Plan.md P11.9; ADR-0020, U64–U66, S17, S18, S83, R164): the
// three 409s, the form while its row changes underneath it, and Delete.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { MODES_LIST_KEY } from "../../api/queries";
import type { BotPage } from "../../generated/BotPage";
import type { ModeResponse } from "../../generated/ModeResponse";
import type { UpdateModeRequest } from "../../generated/UpdateModeRequest";
import { deferred } from "../../test/fixtures";
import { bot, botId, mode, modeId } from "../../test/live";
import { atMode, calls, card, fill, saveButton, shownStep } from "../../test/modes";
import type { RenderedApp } from "../../test/renderApp";
import { PASSWORD, refusal } from "../../test/signIn";

const CHANGED = "This mode changed since you opened it. Load it again to see the change.";
const BY_OTHERS = "A bot you can't set this mode on uses it, so this change can't be saved.";
const EXPLAINED =
  "Someone who can control that bot has to switch it to another mode first. Your edits are still here; you can save them as a copy.";
const IN_USE =
  "A bot uses this mode, so it can't be deleted. Switch every bot to another mode first.";
const LEFT =
  "This mode was deleted, or you can no longer see it. Your edits are still here; you can save them as a copy.";
const NOT_EDITABLE = "You can no longer edit this mode, so your edits weren't kept.";

const ROW = mode(1, {
  name: "Fishing",
  version: 3,
  steps: [shownStep({ type: "send_chat", message: "hello" })],
});

/** The Name field. */
function nameField(): HTMLInputElement {
  return screen.getByLabelText("Name") as HTMLInputElement;
}

/**
 * Lets the screen render what the query's state just became: TanStack tells its observers on a
 * zero timeout, so an assertion right after a state change would still see the screen before it.
 */
async function screenSawIt(): Promise<void> {
  await act(() => new Promise((resolve) => setTimeout(resolve, 0)));
}

/** `rows` in the list's cache, as a live frame or a read leaves it. */
async function listHolds(app: RenderedApp, rows: readonly ModeResponse[]): Promise<void> {
  // The screen's own read mustn't land after this and write over it.
  await vi.waitFor(() => expect(app.runtime.queryClient.isFetching()).toBe(0));
  await act(async () =>
    app.runtime.queryClient.setQueryData(MODES_LIST_KEY, {
      pages: [{ items: [...rows] }],
      pageParams: [undefined],
    }),
  );
  await screenSawIt();
}

describe("409 mode_changed (U64, S18)", () => {
  /** The form open with an edit, and a stale Save refused. */
  async function refused() {
    const control = { refuses: true };
    const found = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async (_id, request) => {
          if (control.refuses) {
            throw refusal(409, "mode_changed", { message: CHANGED });
          }
          return { ...ROW, name: request.name, version: request.version + 1 };
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(found.app, nameField(), "Mine");
    await found.app.user.click(saveButton());
    await screen.findByText(CHANGED);
    return { ...found, control };
  }

  it("shows the server's text with Load again, and keeps the edits", async () => {
    await refused();

    expect(screen.getByRole("button", { name: "Load again" })).toBeDefined();
    expect(nameField().value).toBe("Mine");
  });

  it("replaces the form with the newest version at Load again, and the next save sends it", async () => {
    const { app, state } = await refused();
    state.rows = [{ ...ROW, name: "Theirs", version: 4 }];

    await app.user.click(screen.getByRole("button", { name: "Load again" }));

    await waitFor(() => expect(screen.queryByText(CHANGED)).toBeNull());
    expect(nameField().value).toBe("Theirs");
    expect(saveButton().disabled).toBe(true);
    await fill(app, nameField(), "Mine again");
    await app.user.click(saveButton());
    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(2));
    expect((calls(app, "updateMode")[1] as [string, UpdateModeRequest])[1].version).toBe(4);
  });

  it("keeps the notice and the edits when Load again's read fails, and says why", async () => {
    const { app, state } = await refused();
    state.readFails = true;

    await app.user.click(screen.getByRole("button", { name: "Load again" }));

    const failed = await screen.findByText(/^Couldn't load the modes: refused: internal/);
    expect(failed.closest('[role="alert"]')).toBe(
      screen.getByText(CHANGED).closest('[role="alert"]'),
    );
    expect(nameField().value).toBe("Mine");
  });

  it("goes to the locked view when the newest version isn't one the form can hold (S83)", async () => {
    const { app, state } = await refused();
    state.rows = [{ ...ROW, editable: false, version: 4, name: "Cut" }];

    await app.user.click(screen.getByRole("button", { name: "Load again" }));

    expect(await screen.findByRole("heading", { name: "Cut", level: 1 })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
    expect(screen.getByText(NOT_EDITABLE)).toBeDefined();
  });

  it("takes the notice away at the next Save", async () => {
    const { app, control } = await refused();
    control.refuses = false;

    await app.user.click(saveButton());

    await waitFor(() => expect(screen.queryByText(CHANGED)).toBeNull());
  });
});

describe("what's confirmed with the password is what's shown (PR #85's review)", () => {
  const COMMAND = mode(1, {
    name: "Warp",
    steps: [shownStep({ type: "send_chat", message: "/warp home" })],
  });

  /** A 409 notice up, then Save as copy waiting at "Confirm it's you". */
  async function asked() {
    const found = await atMode(COMMAND.id, {
      rows: [COMMAND],
      handlers: {
        updateModeWithCommands: async () => {
          throw refusal(409, "mode_changed", { message: CHANGED });
        },
      },
      stamped: true,
    });
    await screen.findByRole("heading", { name: "Warp", level: 1 });
    await fill(found.app, nameField(), "Mine");
    await found.app.user.click(saveButton());
    await screen.findByText(CHANGED);
    found.state.stamped = false;
    await found.app.user.click(screen.getByRole("button", { name: "Save as copy" }));
    await screen.findByLabelText("Current password");
    return found;
  }

  /** `name`, a button. */
  const button = (name: string) => screen.getByRole("button", { name }) as HTMLButtonElement;

  it("disables Load again and Save as copy, and locks the fields", async () => {
    await asked();

    expect(screen.getByText(CHANGED)).toBeDefined();
    expect(button("Load again").disabled).toBe(true);
    expect(button("Save as copy").disabled).toBe(true);
    expect(nameField().readOnly).toBe(true);
    expect(within(card("atStart", 1)).getByLabelText("Message").hasAttribute("readonly")).toBe(
      true,
    );
    expect(
      (within(card("atStart", 1)).getByLabelText("What it does") as HTMLSelectElement).disabled,
    ).toBe(true);
    expect(button("Move up").disabled).toBe(true);
    expect(button("Move down").disabled).toBe(true);
    expect(button("Remove").disabled).toBe(true);
    expect(button("Drag step 1").disabled).toBe(true);
  });

  it("disables them while Delete's password is asked too", async () => {
    const { app, state } = await atMode(COMMAND.id, {
      rows: [COMMAND],
      stamped: true,
      handlers: {
        updateModeWithCommands: async () => {
          throw refusal(409, "mode_changed", { message: CHANGED });
        },
        deleteMode: async () => {
          throw refusal(403, "step_up_required");
        },
      },
    });
    await screen.findByRole("heading", { name: "Warp", level: 1 });
    await fill(app, nameField(), "Mine");
    await app.user.click(saveButton());
    await screen.findByText(CHANGED);
    state.stamped = false;

    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    await app.user.click(await screen.findByRole("button", { name: "Delete mode" }));
    await screen.findByLabelText("Current password");

    expect(button("Load again").disabled).toBe(true);
    expect(button("Save as copy").disabled).toBe(true);
  });

  it("offers nothing that would change the form while Load again's read runs", async () => {
    const { app, state } = await atMode(COMMAND.id, {
      rows: [COMMAND],
      stamped: true,
      handlers: {
        updateModeWithCommands: async () => {
          throw refusal(409, "mode_changed", { message: CHANGED });
        },
      },
    });
    await screen.findByRole("heading", { name: "Warp", level: 1 });
    await fill(app, nameField(), "Mine");
    await app.user.click(saveButton());
    await screen.findByText(CHANGED);
    const read = deferred<void>();
    state.hold = read.promise;

    await app.user.click(button("Load again"));

    await vi.waitFor(() => expect(button("Load again").disabled).toBe(true));
    expect(saveButton().disabled).toBe(true);
    expect(button("Save as copy").disabled).toBe(true);
    await act(async () => read.resolve());
    await waitFor(() => expect(screen.queryByText(CHANGED)).toBeNull());
  });
});

describe("409 mode_in_use_by_others (U65, S17)", () => {
  it("shows the server's text and the explanation, and the form stays editable", async () => {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async () => {
          throw refusal(409, "mode_in_use_by_others", { message: BY_OTHERS });
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");

    await app.user.click(saveButton());

    expect(await screen.findByText(BY_OTHERS)).toBeDefined();
    expect(screen.getByText(EXPLAINED)).toBeDefined();
    expect(screen.queryByRole("button", { name: "Load again" })).toBeNull();
    expect(nameField().readOnly).toBe(false);
    expect(nameField().value).toBe("Mine");
    expect(saveButton().disabled).toBe(false);
  });
});

describe("the form while its row changes (S83)", () => {
  it("ignores a newer version, keeping the edits and the version read, until Save's 409", async () => {
    const { app } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");

    await listHolds(app, [{ ...ROW, name: "Theirs", version: 4 }]);

    expect(nameField().value).toBe("Mine");
    expect(screen.queryByText(CHANGED)).toBeNull();
    await app.user.click(saveButton());
    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(1));
    expect((calls(app, "updateMode")[0] as [string, UpdateModeRequest])[1].version).toBe(3);
  });

  it("follows the flags of the newest row in what it offers", async () => {
    const { app } = await atMode(ROW.id, { rows: [{ ...ROW, visibility_editable: true }] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    expect(screen.getByRole("button", { name: "Delete" })).toBeDefined();
    expect(screen.getByRole("combobox", { name: "Who can see it" })).toBeDefined();

    await listHolds(app, [{ ...ROW, deletable: false, visibility_editable: false }]);

    await waitFor(() => expect(screen.queryByRole("button", { name: "Delete" })).toBeNull());
    expect(screen.queryByRole("combobox", { name: "Who can see it" })).toBeNull();
  });

  it("drops to the locked row when editable turns false, without the edits or the prompt", async () => {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async () => {
          throw refusal(403, "mode_needs_step_up");
        },
        updateModeWithCommands: async () => {
          throw refusal(403, "step_up_required");
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    const message = within(card("atStart", 1)).getByLabelText("Message");
    await fill(app, message, "/login my-secret");
    await app.user.click(saveButton());
    await screen.findByLabelText("Current password");

    await listHolds(app, [
      {
        ...ROW,
        editable: false,
        deletable: false,
        visibility_editable: false,
        steps: [shownStep({ type: "send_chat", message: "/warp", arguments_hidden: true })],
      },
    ]);

    expect(await screen.findByText(NOT_EDITABLE)).toBeDefined();
    expect(screen.queryByLabelText("Current password")).toBeNull();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
    const shown = within(card("atStart", 1)).getByLabelText("Message") as HTMLInputElement;
    expect(shown.value).toBe("/warp");
    expect(shown.readOnly).toBe(true);
    expect(screen.queryByDisplayValue("/login my-secret")).toBeNull();
  });

  it("keeps the form with a notice when the row leaves the list, offering no Save or Delete", async () => {
    const { app } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");

    await listHolds(app, []);

    expect(await screen.findByText(LEFT)).toBeDefined();
    expect(nameField().value).toBe("Mine");
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
  });

  it("keeps the form through a reset query while its read hangs: only data read says a row left", async () => {
    const { app, state } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");
    await vi.waitFor(() => expect(app.runtime.queryClient.isFetching()).toBe(0));
    const read = deferred<void>();
    state.hold = read.promise;

    await act(async () => {
      void app.runtime.queryClient.resetQueries({ queryKey: MODES_LIST_KEY, exact: true });
    });
    await vi.waitFor(() =>
      expect(app.runtime.queryClient.getQueryData(MODES_LIST_KEY)).toBeUndefined(),
    );
    await screenSawIt();

    expect(screen.queryByText("Loading modes…")).toBeNull();
    expect(screen.queryByText(LEFT)).toBeNull();
    expect(nameField().value).toBe("Mine");
    await act(async () => read.resolve());
  });

  it("keeps the form when a reset query's read fails: a failed read says nothing of the row", async () => {
    const { app, state } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");
    await vi.waitFor(() => expect(app.runtime.queryClient.isFetching()).toBe(0));
    state.readFails = true;

    await act(async () => {
      await app.runtime.queryClient.resetQueries({ queryKey: MODES_LIST_KEY, exact: true });
    });
    await vi.waitFor(() =>
      expect(app.runtime.queryClient.getQueryState(MODES_LIST_KEY)?.status).toBe("error"),
    );
    await screenSawIt();

    expect(screen.queryByText(/^Couldn't load the modes/)).toBeNull();
    expect(screen.queryByText(LEFT)).toBeNull();
    expect(nameField().value).toBe("Mine");
  });

  describe("a form the screen dropped never comes back (PR #85's review)", () => {
    const WHOLE = mode(1, {
      name: "Warp",
      steps: [shownStep({ type: "send_chat", message: "/login my-secret" })],
    });
    const CUT = {
      ...WHOLE,
      editable: false,
      deletable: false,
      visibility_editable: false,
      steps: [shownStep({ type: "send_chat", message: "/login", arguments_hidden: true })],
    };

    /** The form open on the whole row, then dropped to the locked row. */
    async function dropped() {
      const found = await atMode(WHOLE.id, { rows: [WHOLE] });
      await screen.findByRole("heading", { name: "Warp", level: 1 });
      await listHolds(found.app, [CUT]);
      found.state.rows = [CUT];
      await screen.findByText(NOT_EDITABLE);
      return found;
    }

    it("shows the gone screen when the row then leaves, with no full command and no copy", async () => {
      const { app, state } = await dropped();

      state.rows = [];
      await listHolds(app, []);

      expect(
        await screen.findByText("This mode doesn't exist any more, or you can't see it."),
      ).toBeDefined();
      expect(screen.queryByDisplayValue("/login my-secret")).toBeNull();
      expect(screen.queryByRole("button", { name: "Save as copy" })).toBeNull();
      expect(screen.queryByText(LEFT)).toBeNull();
    });

    it("doesn't remount the editor through a reset query while its read hangs", async () => {
      const { app, state } = await dropped();
      const read = deferred<void>();
      state.hold = read.promise;

      await act(async () => {
        void app.runtime.queryClient.resetQueries({ queryKey: MODES_LIST_KEY, exact: true });
      });
      await vi.waitFor(() =>
        expect(app.runtime.queryClient.getQueryData(MODES_LIST_KEY)).toBeUndefined(),
      );

      expect(await screen.findByText("Loading modes…")).toBeDefined();
      expect(screen.queryByDisplayValue("/login my-secret")).toBeNull();
      expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
      await act(async () => read.resolve());
    });

    it("keeps the no-longer-editable note after that reset's read lands", async () => {
      const { app, state } = await dropped();
      const read = deferred<void>();
      state.hold = read.promise;

      await act(async () => {
        void app.runtime.queryClient.resetQueries({ queryKey: MODES_LIST_KEY, exact: true });
      });
      await screen.findByText("Loading modes…");
      await act(async () => read.resolve());

      expect(await screen.findByText(NOT_EDITABLE)).toBeDefined();
    });

    it("opens a fresh form, without the note, if the row can be edited again", async () => {
      const { app, state } = await dropped();

      state.rows = [{ ...WHOLE, version: 2 }];
      await listHolds(app, state.rows);

      expect(await screen.findByRole("button", { name: "Save" })).toBeDefined();
      expect(screen.queryByText(NOT_EDITABLE)).toBeNull();
    });
  });

  it("shows the gone notice for a Save's 404, and reads the modes again", async () => {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async () => {
          throw refusal(404, "not_found");
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    const reads = calls(app, "modes").length;
    await fill(app, nameField(), "Mine");

    await app.user.click(saveButton());

    expect(await screen.findByText(LEFT)).toBeDefined();
    expect(nameField().value).toBe("Mine");
    await vi.waitFor(() => expect(calls(app, "modes").length).toBeGreaterThan(reads));
  });
});

describe("Delete (S16, S17)", () => {
  const deleted = "Deleted Fishing.";
  const ask = "Delete Fishing? This can't be undone.";

  /** The App on a mode with `deleteMode` as `answer`, stepped up unless told otherwise. */
  async function atDelete(deleteMode: (id: string) => Promise<void>, stamped = true) {
    const found = await atMode(ROW.id, {
      rows: [ROW],
      stamped,
      handlers: { deleteMode },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await found.app.user.click(screen.getByRole("button", { name: "Delete" }));
    await screen.findByText(ask);
    return found;
  }

  it("asks first, goes through the step-up, then goes back to the list without it", async () => {
    let stepped = false;
    const { app, state } = await atDelete(async () => {
      if (!stepped) {
        stepped = true;
        throw refusal(403, "step_up_required");
      }
      state.rows = [];
    }, false);

    await app.user.click(screen.getByRole("button", { name: "Delete mode" }));
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText(deleted)).toBeDefined();
    await vi.waitFor(() => expect(app.path()).toBe("/modes"));
    expect(calls(app, "deleteMode")).toEqual([[ROW.id], [ROW.id]]);
    await waitFor(() => expect(screen.queryByRole("link", { name: "Open Fishing" })).toBeNull());
  });

  it("does nothing at Cancel", async () => {
    const { app } = await atDelete(async () => {});

    await app.user.click(
      within(screen.getByText(ask).closest('[role="alert"]') as HTMLElement).getByRole("button", {
        name: "Cancel",
      }),
    );

    expect(screen.queryByText(ask)).toBeNull();
    expect(calls(app, "deleteMode")).toEqual([]);
  });

  it("shows mode_in_use's text in place, and the mode stays", async () => {
    const { app } = await atDelete(async () => {
      throw refusal(409, "mode_in_use", { message: IN_USE });
    });

    await app.user.click(screen.getByRole("button", { name: "Delete mode" }));

    expect(await screen.findByText(IN_USE)).toBeDefined();
    expect(app.path()).toBe(`/modes/${ROW.id}`);
    expect(screen.getByRole("heading", { name: "Fishing", level: 1 })).toBeDefined();
    expect(screen.queryByText(deleted)).toBeNull();
  });

  it("shows the gone notice for a 404, and a 403 as its text", async () => {
    let answer = refusal(403, "forbidden", { message: "You don't have permission to do this." });
    const { app } = await atDelete(async () => {
      throw answer;
    });

    await app.user.click(screen.getByRole("button", { name: "Delete mode" }));
    expect(await screen.findByText(/^You don't have permission to do this\./)).toBeDefined();
    answer = refusal(404, "not_found");
    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    await app.user.click(await screen.findByRole("button", { name: "Delete mode" }));

    expect(await screen.findByText(LEFT)).toBeDefined();
  });

  it("locks the form while its password is asked", async () => {
    const { app } = await atDelete(async () => {
      throw refusal(403, "step_up_required");
    }, false);

    await app.user.click(screen.getByRole("button", { name: "Delete mode" }));
    await screen.findByLabelText("Current password");

    expect(nameField().readOnly).toBe(true);
  });

  it("is on a locked mode too, from its flag", async () => {
    const locked = mode(1, {
      name: "Unknown",
      steps: [shownStep({ type: "dance" as never })],
    });
    const { app } = await atMode(locked.id, {
      rows: [locked],
      stamped: true,
      handlers: { deleteMode: async () => {} },
    });
    await screen.findByRole("heading", { name: "Unknown", level: 1 });

    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    await app.user.click(await screen.findByRole("button", { name: "Delete mode" }));

    expect(await screen.findByText("Deleted Unknown.")).toBeDefined();
  });

  it("isn't offered without the flag", async () => {
    const fixed = mode(1, { deletable: false });
    await atMode(fixed.id, { rows: [fixed] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
  });
});

describe("the bots at mode_in_use (U66, R169)", () => {
  /** A bot of the viewer's, named `name`, on mode `modeIdOf`. */
  function onMode(n: number, name: string, modeIdOf: string) {
    const base = bot(n);
    return { ...base, mode_id: modeIdOf, account: { ...base.account, name } };
  }

  /** The App refused at Delete, with `bots` as the bots' reads. */
  async function refused(bots: ApiClient["bots"]) {
    const found = await atMode(ROW.id, {
      rows: [ROW],
      stamped: true,
      handlers: {
        bots,
        deleteMode: async () => {
          throw refusal(409, "mode_in_use", { message: IN_USE });
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await found.app.user.click(screen.getByRole("button", { name: "Delete" }));
    await found.app.user.click(await screen.findByRole("button", { name: "Delete mode" }));
    await screen.findByText(IN_USE);
    return found;
  }

  it("lists the bots the viewer can see that run it, each linking to its screen", async () => {
    const first: BotPage = {
      items: [onMode(3, "AfkBot3", ROW.id), onMode(2, "AfkBot2", modeId(7))],
      next: botId(2),
    };
    const second: BotPage = { items: [onMode(1, "AfkBot1", ROW.id)] };
    const { app } = await refused(async (before) => (before === undefined ? first : second));

    expect(await screen.findByText("Bots you can see that use this mode:")).toBeDefined();
    const links = screen.getAllByRole("link", { name: /^AfkBot/ });
    expect(links.map((link) => link.textContent)).toEqual(["AfkBot3", "AfkBot1"]);
    await app.user.click(links[1] as HTMLElement);
    await vi.waitFor(() => expect(app.path()).toBe(`/bots/${botId(1)}`));
  });

  it("reads every page before it says the bot is one the viewer can't see", async () => {
    const later = deferred<BotPage>();
    const { app } = await refused(async (before) =>
      before === undefined ? { items: [], next: botId(1) } : later.promise,
    );
    const SAID = "It's a bot you can't see, so its owner has to switch it.";

    await vi.waitFor(() => expect(calls(app, "bots").map((args) => args[0])).toContain(botId(1)));
    expect(screen.queryByText(SAID)).toBeNull();
    await act(async () => later.resolve({ items: [] }));

    expect(await screen.findByText(SAID)).toBeDefined();
  });

  it("ignores an answer from an older attempt", async () => {
    const older = deferred<BotPage>();
    let reads = 0;
    const { app } = await refused(async (_before, limit) => {
      if (limit === undefined) {
        return { items: [] };
      }
      reads += 1;
      return reads === 1 ? older.promise : { items: [] };
    });

    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    await app.user.click(await screen.findByRole("button", { name: "Delete mode" }));
    const SAID = "It's a bot you can't see, so its owner has to switch it.";
    expect(await screen.findByText(SAID)).toBeDefined();
    await act(async () => older.resolve({ items: [onMode(1, "AfkBot1", ROW.id)] }));

    expect(screen.queryByRole("link", { name: "AfkBot1" })).toBeNull();
    expect(screen.getByText(SAID)).toBeDefined();
  });

  it("keeps the refusal and says so when the bots couldn't be read", async () => {
    await refused(async () => {
      throw refusal(500, "internal");
    });

    expect(await screen.findByText(/^Couldn't read your bots: refused: internal/)).toBeDefined();
    expect(screen.getByText(IN_USE)).toBeDefined();
    expect(
      screen.queryByText("It's a bot you can't see, so its owner has to switch it."),
    ).toBeNull();
  });

  it("drops the list at the next try", async () => {
    const later = deferred<BotPage>();
    const answers = [async () => ({ items: [onMode(1, "AfkBot1", ROW.id)] }), () => later.promise];
    const home: BotPage = { items: [] };
    let delivered = false;
    const { app } = await refused(async (_before, limit) => {
      // Home's own read asks without a page size.
      if (limit === undefined || delivered) {
        return limit === undefined ? home : (answers[1] as () => Promise<BotPage>)();
      }
      delivered = true;
      return (answers[0] as () => Promise<BotPage>)();
    });
    await screen.findByRole("link", { name: "AfkBot1" });

    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    await app.user.click(await screen.findByRole("button", { name: "Delete mode" }));

    await waitFor(() => expect(calls(app, "deleteMode")).toHaveLength(2));
    expect(screen.queryByRole("link", { name: "AfkBot1" })).toBeNull();
    await act(async () => later.resolve({ items: [] }));
  });
});

describe("a mode gone before it opened", () => {
  it("is the gone screen, not a form", async () => {
    await atMode(modeId(5), { rows: [ROW] });

    expect(
      await screen.findByText("This mode doesn't exist any more, or you can't see it."),
    ).toBeDefined();
    expect(screen.queryByLabelText("Name")).toBeNull();
  });
});
