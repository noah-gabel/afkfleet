// A mode's conflicts and its delete (Plan.md P11.9; ADR-0020, U64–U66, S17, S18, S83, R164): the
// three 409s, the form while its row changes underneath it, and Delete.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MODES_LIST_KEY } from "../../api/queries";
import type { ModeResponse } from "../../generated/ModeResponse";
import type { UpdateModeRequest } from "../../generated/UpdateModeRequest";
import { mode, modeId } from "../../test/live";
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

  it("keeps the form through a reset query: only data read says a row left", async () => {
    const { app } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");

    await act(async () => {
      await app.runtime.queryClient.resetQueries({ queryKey: MODES_LIST_KEY, exact: true });
    });

    expect(screen.queryByText(LEFT)).toBeNull();
    expect(nameField().value).toBe("Mine");
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

describe("a mode gone before it opened", () => {
  it("is the gone screen, not a form", async () => {
    await atMode(modeId(5), { rows: [ROW] });

    expect(
      await screen.findByText("This mode doesn't exist any more, or you can't see it."),
    ).toBeDefined();
    expect(screen.queryByLabelText("Name")).toBeNull();
  });
});
