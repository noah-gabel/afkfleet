// A mode's copies (Plan.md P11.9; ADR-0020, U63, U67, R5, R168, R170): "Make a copy" on every mode
// the viewer can see, "Save as copy" in the notices that can't save, each built from the form,
// saved private like any new mode.
import { act, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MODES_LIST_KEY } from "../../api/queries";
import type { CreateModeRequest } from "../../generated/CreateModeRequest";
import type { ModeActionKind } from "../../generated/ModeActionKind";
import { mode, modeId } from "../../test/live";
import { atMode, calls, card, every, fill, saveButton, shownStep } from "../../test/modes";
import { refusal } from "../../test/signIn";

const COPY = "Make a copy";
const SAVE_AS_COPY = "Save as copy";
const CUT_IN_COPY = "Add its arguments: the original's are hidden from you.";

const BUILT_IN = mode(1, {
  name: "afk",
  visibility: "shared",
  ownership: "built_in",
  editable: false,
  deletable: false,
  visibility_editable: false,
  unlisted_command: false,
  steps: [shownStep({ type: "rotate_random", max_yaw: 30, max_pitch: 10 }, every(45, 75))],
});

const CUT = mode(2, {
  name: "warp",
  visibility: "shared",
  ownership: "someone_else",
  editable: false,
  deletable: false,
  visibility_editable: false,
  unlisted_command: true,
  steps: [shownStep({ type: "send_chat", message: "/warp", arguments_hidden: true })],
});

const MINE = mode(3, {
  name: "Fishing",
  version: 3,
  visibility: "shared",
  steps: [shownStep({ type: "jump" })],
});

/** The Name field. */
function nameField(): HTMLInputElement {
  return screen.getByLabelText("Name") as HTMLInputElement;
}

/** The request of the `createMode` call `n`. */
function createdWith(app: Parameters<typeof calls>[0], n = 0): CreateModeRequest {
  return (calls(app, "createMode")[n] as [CreateModeRequest])[0];
}

describe("Make a copy (U63)", () => {
  it("opens a built-in mode's values as a new, private mode, and saves like any new one", async () => {
    const { app } = await atMode(BUILT_IN.id, { rows: [BUILT_IN] });
    await screen.findByRole("heading", { name: "afk", level: 1 });

    await app.user.click(screen.getByRole("button", { name: COPY }));

    expect(await screen.findByRole("heading", { name: "New mode", level: 1 })).toBeDefined();
    expect(nameField().value).toBe("afk copy");
    expect(nameField().readOnly).toBe(false);
    expect(
      (within(card("repeating", 1)).getByLabelText("What it does") as HTMLSelectElement).value,
    ).toBe("rotate_random");
    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "createMode")).toHaveLength(1));
    expect(createdWith(app)).toEqual({
      name: "afk copy",
      visibility: "private",
      steps: [
        {
          action: { type: "rotate_random", max_yaw: 30, max_pitch: 10 },
          schedule: { type: "every", interval_ms: 45_000, jitter_ms: 75_000 },
          probability: 100,
        },
      ],
    });
    expect(await screen.findByText("Saved a copy as afk copy.")).toBeDefined();
    await vi.waitFor(() => expect(app.path()).toBe(`/modes/${modeId(9)}`));
    expect(await screen.findByRole("heading", { name: "afk copy", level: 1 })).toBeDefined();
  });

  it("is private even from a shared mode the viewer owns", async () => {
    const { app } = await atMode(MINE.id, { rows: [MINE] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });

    await app.user.click(screen.getByRole("button", { name: COPY }));
    await screen.findByRole("heading", { name: "New mode", level: 1 });
    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "createMode")).toHaveLength(1));
    expect(createdWith(app).visibility).toBe("private");
  });

  it("holds the form's unsaved edits", async () => {
    const { app } = await atMode(MINE.id, { rows: [MINE] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await app.user.selectOptions(
      within(card("atStart", 1)).getByLabelText("What it does"),
      "Swing arm",
    );

    await app.user.click(screen.getByRole("button", { name: COPY }));
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    expect(
      (within(card("atStart", 1)).getByLabelText("What it does") as HTMLSelectElement).value,
    ).toBe("swing_arm");
  });

  it("brings a cut command in as it came, marked outside its text, until it changes", async () => {
    const { app } = await atMode(CUT.id, { rows: [CUT] });
    await screen.findByRole("heading", { name: "warp", level: 1 });

    await app.user.click(screen.getByRole("button", { name: COPY }));
    await screen.findByRole("heading", { name: "New mode", level: 1 });
    const step = card("atStart", 1);
    const message = within(step).getByLabelText("Message") as HTMLInputElement;

    expect(message.value).toBe("/warp");
    expect(message.readOnly).toBe(false);
    expect(message.getAttribute("aria-describedby")).toContain(
      within(step).getByText(CUT_IN_COPY).id,
    );
    await fill(app, message, "/warp home");
    expect(within(step).queryByText(CUT_IN_COPY)).toBeNull();
  });

  it("sends no answer-only key, and a command through the step-up to the twin", async () => {
    const { app } = await atMode(CUT.id, { rows: [CUT], stamped: true });
    await screen.findByRole("heading", { name: "warp", level: 1 });
    await app.user.click(screen.getByRole("button", { name: COPY }));
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "createModeWithCommands")).toHaveLength(1));
    const request = (calls(app, "createModeWithCommands")[0] as [CreateModeRequest])[0];
    expect(request).toEqual({
      name: "warp copy",
      visibility: "private",
      steps: [
        {
          action: { type: "send_chat", message: "/warp" },
          schedule: { type: "at_start" },
          probability: 100,
        },
      ],
    });
  });

  it("shows a taken name at Name, and the cap's text", async () => {
    const answers = [
      refusal(422, "validation_failed", {
        fields: [{ path: "name", message: "is already another mode's name" }],
      }),
      refusal(409, "conflict"),
    ];
    const { app } = await atMode(BUILT_IN.id, {
      rows: [BUILT_IN],
      handlers: {
        createMode: async () => {
          throw answers.shift();
        },
      },
    });
    await screen.findByRole("heading", { name: "afk", level: 1 });
    await app.user.click(screen.getByRole("button", { name: COPY }));
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    await app.user.click(saveButton());
    const said = await screen.findByText("is already another mode's name");
    expect(nameField().getAttribute("aria-describedby")).toContain(said.id);
    await app.user.click(saveButton());

    expect(
      await screen.findByText(
        "You already have 50 modes, the most you can have. Delete one to add another.",
      ),
    ).toBeDefined();
  });

  it("isn't offered on a mode with a step this app doesn't know", async () => {
    const unknown = mode(1, { steps: [shownStep({ type: "dance" as ModeActionKind })] });
    await atMode(unknown.id, { rows: [unknown] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(screen.queryByRole("button", { name: COPY })).toBeNull();
  });

  it("isn't a second save beside Save in the normal editor", async () => {
    await atMode(MINE.id, { rows: [MINE] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });

    expect(screen.queryByRole("button", { name: SAVE_AS_COPY })).toBeNull();
  });
});

describe("Save as copy (U63, R168)", () => {
  /** A step edited, and the edit refused by a 409 `code`; the name stays. */
  async function refusedBy(code: string) {
    const found = await atMode(MINE.id, {
      rows: [MINE],
      handlers: {
        updateMode: async () => {
          throw refusal(409, code, { message: `refused: ${code}` });
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await found.app.user.selectOptions(
      within(card("atStart", 1)).getByLabelText("What it does"),
      "Swing arm",
    );
    await found.app.user.click(saveButton());
    await screen.findByText(`refused: ${code}`);
    return found;
  }

  it.each(["mode_changed", "mode_in_use_by_others"])(
    "is in %s's notice, and sends the form as a new private mode, under the suggested name",
    async (code) => {
      const { app } = await refusedBy(code);

      await app.user.click(screen.getByRole("button", { name: SAVE_AS_COPY }));

      await vi.waitFor(() => expect(calls(app, "createMode")).toHaveLength(1));
      expect(createdWith(app)).toEqual({
        name: "Fishing copy",
        visibility: "private",
        steps: [
          { action: { type: "swing_arm" }, schedule: { type: "at_start" }, probability: 100 },
        ],
      });
      expect(calls(app, "updateMode")).toHaveLength(1);
      expect(await screen.findByText("Saved a copy as Fishing copy.")).toBeDefined();
      await vi.waitFor(() => expect(app.path()).toBe(`/modes/${modeId(9)}`));
    },
  );

  it("sends a name the user changed as it is, and shows what's sent", async () => {
    const found = await atMode(MINE.id, {
      rows: [MINE],
      handlers: {
        updateMode: async () => {
          throw refusal(409, "mode_changed", { message: "refused: mode_changed" });
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(found.app, nameField(), "Mine");
    await found.app.user.click(saveButton());
    await screen.findByText("refused: mode_changed");

    await found.app.user.click(screen.getByRole("button", { name: SAVE_AS_COPY }));

    await vi.waitFor(() => expect(calls(found.app, "createMode")).toHaveLength(1));
    expect(createdWith(found.app).name).toBe("Mine");
  });

  it("puts the suggested name on the form, and a taken one at its field, with the notice still there", async () => {
    const { app } = await atMode(MINE.id, {
      rows: [MINE],
      handlers: {
        updateMode: async () => {
          throw refusal(409, "mode_in_use_by_others", { message: "refused: in use" });
        },
        createMode: async () => {
          throw refusal(422, "validation_failed", {
            fields: [{ path: "name", message: "is already another mode's name" }],
          });
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await app.user.selectOptions(
      within(card("atStart", 1)).getByLabelText("What it does"),
      "Swing arm",
    );
    await app.user.click(saveButton());
    await screen.findByText("refused: in use");

    await app.user.click(screen.getByRole("button", { name: SAVE_AS_COPY }));

    const said = await screen.findByText("is already another mode's name");
    expect(nameField().value).toBe("Fishing copy");
    expect(nameField().getAttribute("aria-describedby")).toContain(said.id);
    expect(screen.getByRole("button", { name: SAVE_AS_COPY })).toBeDefined();
  });

  it("is in the notice of a row that left the list, the only way to keep the edits", async () => {
    const { app } = await atMode(MINE.id, { rows: [MINE] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");
    await vi.waitFor(() => expect(app.runtime.queryClient.isFetching()).toBe(0));

    await act(async () =>
      app.runtime.queryClient.setQueryData(MODES_LIST_KEY, {
        pages: [{ items: [] }],
        pageParams: [undefined],
      }),
    );
    await app.user.click(await screen.findByRole("button", { name: SAVE_AS_COPY }));

    await vi.waitFor(() => expect(calls(app, "createMode")).toHaveLength(1));
    expect(createdWith(app).name).toBe("Mine");
  });

  it("goes through the step-up for a command off the allowlist", async () => {
    const withCommand = mode(3, {
      name: "Fishing",
      steps: [shownStep({ type: "send_chat", message: "/warp home" })],
      unlisted_command: true,
    });
    const { app } = await atMode(withCommand.id, {
      rows: [withCommand],
      stamped: true,
      handlers: {
        updateModeWithCommands: async () => {
          throw refusal(409, "mode_changed", { message: "refused: mode_changed" });
        },
      },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    await fill(app, nameField(), "Mine");
    await app.user.click(saveButton());
    await screen.findByText("refused: mode_changed");

    await app.user.click(screen.getByRole("button", { name: SAVE_AS_COPY }));

    await vi.waitFor(() => expect(calls(app, "createModeWithCommands")).toHaveLength(1));
    expect(calls(app, "createMode")).toHaveLength(1);
  });
});
