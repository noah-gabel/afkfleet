// A mode's screen (Plan.md P11.9; ADR-0020, U67, U68, U70, U72, U73, S12, S14–S16, S82, S84,
// R158–R162): the form filled from the row by section, every request built from the form, Save
// only when something changed, the plain route first and the twin through "Confirm it's you",
// the server's errors at their fields, and the locked view.
import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MODES_LIST_KEY } from "../../api/queries";
import type { ModeActionKind } from "../../generated/ModeActionKind";
import type { ModeResponse } from "../../generated/ModeResponse";
import type { ModeScheduleKind } from "../../generated/ModeScheduleKind";
import type { ModeVisibility } from "../../generated/ModeVisibility";
import type { UpdateModeRequest } from "../../generated/UpdateModeRequest";
import { mode, modeId } from "../../test/live";
import {
  atMode,
  calls,
  card,
  every,
  fill,
  ON_RESPAWN,
  SECTION,
  saveButton,
  section,
  shownStep,
} from "../../test/modes";
import { PASSWORD, refusal } from "../../test/signIn";

afterEach(() => {
  vi.useRealTimers();
});

describe("a new mode", () => {
  it("is private, with no switch, and says the choice comes after saving (U67)", async () => {
    await atMode("new");

    expect(await screen.findByRole("heading", { name: "New mode", level: 1 })).toBeDefined();
    expect(
      screen.getByText(
        "New modes start private. If you're allowed to share it, the choice appears after saving.",
      ),
    ).toBeDefined();
    expect(screen.queryByRole("combobox", { name: "Who can see it" })).toBeNull();
    expect(saveButton().disabled).toBe(false);
  });

  it("sends what the form holds, then opens the new mode's screen", async () => {
    const { app } = await atMode("new");
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    await fill(app, screen.getByLabelText("Name"), "Fishing");
    await app.user.click(within(section("atStart")).getByRole("button", { name: "Add step" }));
    await app.user.selectOptions(
      within(card("atStart", 1)).getByLabelText("What it does"),
      "Send chat or a command",
    );
    await fill(app, within(card("atStart", 1)).getByLabelText("Message"), "hello");
    await app.user.click(within(section("repeating")).getByRole("button", { name: "Add step" }));
    const repeating = card("repeating", 1);
    await app.user.selectOptions(
      within(repeating).getByLabelText("What it does"),
      "Look around randomly",
    );
    await fill(app, within(repeating).getByLabelText("Turn up to (degrees either way)"), "30");
    await fill(app, within(repeating).getByLabelText("Tilt up to (degrees either way)"), "10.5");
    await fill(app, within(repeating).getByLabelText("Every"), "2");
    await app.user.selectOptions(within(repeating).getByLabelText("Every: unit"), "minutes");
    await fill(app, within(repeating).getByLabelText("Plus up to, at random"), "30");
    await fill(app, within(repeating).getByLabelText("Chance (%)"), "75");
    await app.user.click(within(section("onRespawn")).getByRole("button", { name: "Add step" }));
    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "createMode")).toHaveLength(1));
    expect(calls(app, "createMode")[0]).toEqual([
      {
        name: "Fishing",
        visibility: "private",
        steps: [
          {
            action: { type: "send_chat", message: "hello" },
            schedule: { type: "at_start" },
            probability: 100,
          },
          {
            action: { type: "rotate_random", max_yaw: 30, max_pitch: 10.5 },
            schedule: { type: "every", interval_ms: 120_000, jitter_ms: 30_000 },
            probability: 75,
          },
          { action: { type: "jump" }, schedule: { type: "on_respawn" }, probability: 100 },
        ],
      },
    ]);
    expect(await screen.findByText("Created Fishing.")).toBeDefined();
    await vi.waitFor(() => expect(app.path()).toBe(`/modes/${modeId(9)}`));
    expect(await screen.findByRole("heading", { name: "Fishing", level: 1 })).toBeDefined();
  });

  it("shows the cap's text at a 409 conflict, with S19's limit", async () => {
    const { app } = await atMode("new", {
      handlers: {
        createMode: async () => {
          throw refusal(409, "conflict");
        },
      },
    });
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    await fill(app, screen.getByLabelText("Name"), "Fishing");
    await app.user.click(saveButton());

    expect(
      await screen.findByText(
        "You already have 50 modes, the most you can have. Delete one to add another.",
      ),
    ).toBeDefined();
  });

  it("offers 32 steps over its sections, then says so", async () => {
    const { app } = await atMode("new");
    await screen.findByRole("heading", { name: "New mode", level: 1 });
    const add = () =>
      within(section("repeating")).getByRole("button", { name: "Add step" }) as HTMLButtonElement;

    for (let n = 0; n < 32; n += 1) {
      await app.user.click(add());
    }

    expect(add().disabled).toBe(true);
    expect(
      (within(section("atStart")).getByRole("button", { name: "Add step" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(within(section("repeating")).getByText("At most 32 steps.")).toBeDefined();
  }, 30_000);
});

describe("a step's card (S12, U68, U70, U72)", () => {
  /** A new mode's first step at the start, as `kind`. */
  async function asKind(kind: string) {
    const { app } = await atMode("new");
    await screen.findByRole("heading", { name: "New mode", level: 1 });
    await app.user.click(within(section("atStart")).getByRole("button", { name: "Add step" }));
    await app.user.selectOptions(within(card("atStart", 1)).getByLabelText("What it does"), kind);
    return { app, card: card("atStart", 1) };
  }

  /** The labels of a card's fields but "What it does" and "Chance". */
  function labels(element: HTMLElement): string[] {
    return [...element.querySelectorAll<HTMLInputElement>("input, select")]
      .map((field) => field.labels?.[0]?.textContent ?? "")
      .filter((label) => label !== "" && label !== "What it does" && label !== "Chance (%)");
  }

  it.each<[string, string[]]>([
    [
      "Look in a direction",
      [
        "Yaw (left or right, as the F3 screen shows it)",
        "Pitch (up or down, as the F3 screen shows it)",
      ],
    ],
    [
      "Look around randomly",
      ["Turn up to (degrees either way)", "Tilt up to (degrees either way)"],
    ],
    ["Jump", []],
    ["Sneak", ["Sneaking"]],
    ["Swing arm", []],
    ["Use the held item (right-click)", []],
    ["Hold right-click", ["Right-click"]],
    ["Attack what's in front (left-click)", []],
    ["Select a hotbar slot", ["Slot"]],
    ["Send chat or a command", ["Message"]],
  ])("shows only %s's fields", async (kind, expected) => {
    const found = await asKind(kind);

    expect(labels(found.card)).toEqual(expected);
  });

  it("gives every section's card a Chance, 100 for a new step, and only a repeating one its wait", async () => {
    const { app } = await atMode("new");
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    for (const name of ["atStart", "repeating", "onRespawn"] as const) {
      await app.user.click(within(section(name)).getByRole("button", { name: "Add step" }));
      const chance = within(card(name, 1)).getByLabelText("Chance (%)") as HTMLInputElement;
      expect(chance.value).toBe("100");
    }

    expect(within(card("atStart", 1)).queryByLabelText("Every")).toBeNull();
    expect(within(card("onRespawn", 1)).queryByLabelText("Every")).toBeNull();
    expect(within(card("repeating", 1)).getByLabelText("Every")).toBeDefined();
  });

  it("offers the hotbar's keys 1 to 9, and sends the API's 0 to 8", async () => {
    const found = await asKind("Select a hotbar slot");
    const slot = within(found.card).getByLabelText("Slot") as HTMLSelectElement;

    expect([...slot.options].map((option) => option.textContent)).toEqual([
      "1",
      "2",
      "3",
      "4",
      "5",
      "6",
      "7",
      "8",
      "9",
    ]);
    await fill(found.app, screen.getByLabelText("Name"), "Slots");
    await found.app.user.selectOptions(slot, "9");
    await found.app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(found.app, "createMode")).toHaveLength(1));
    expect(calls(found.app, "createMode")[0]).toEqual([
      {
        name: "Slots",
        visibility: "private",
        steps: [
          {
            action: { type: "select_hotbar_slot", slot: 8 },
            schedule: { type: "at_start" },
            probability: 100,
          },
        ],
      },
    ]);
  });

  it("explains a respawn, and each section says when it runs", async () => {
    await atMode("new");
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    expect(
      within(section("onRespawn")).getByText(
        "After a death, the bot respawns, lets go of the use button and stops sneaking, then runs these steps. A chat step here is skipped if the last one went out less than 30 seconds ago.",
      ),
    ).toBeDefined();
    expect(
      within(section("atStart")).getByText("Runs once each time the bot joins a server."),
    ).toBeDefined();
    expect(
      within(section("repeating")).getByText(
        "Repeats while the bot is online. The first run comes one wait after it joins.",
      ),
    ).toBeDefined();
  });

  it("moves a step within its section, says where it went, and sends the shown order", async () => {
    const row = mode(1, {
      steps: [
        shownStep({ type: "jump" }, every(10)),
        shownStep({ type: "swing_arm" }, every(20)),
        shownStep({ type: "use_item" }, every(30)),
      ],
    });
    const { app } = await atMode(row.id, { rows: [row] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    const button = (n: number, name: string) =>
      within(card("repeating", n)).getByRole("button", { name }) as HTMLButtonElement;

    expect(button(1, "Move up").disabled).toBe(true);
    expect(button(3, "Move down").disabled).toBe(true);
    await app.user.click(button(3, "Move up"));

    expect(screen.getByText("Step 3 moved to position 2 of 3.").getAttribute("role")).toBe(
      "status",
    );
    await app.user.click(saveButton());
    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(1));
    const [, request] = calls(app, "updateMode")[0] as [string, UpdateModeRequest];
    expect(request.steps.map((step) => step.action.type)).toEqual([
      "jump",
      "use_item",
      "swing_arm",
    ]);
  });

  it("takes a step out with Remove, and the next step takes its number", async () => {
    const row = mode(1, {
      steps: [shownStep({ type: "jump" }), shownStep({ type: "swing_arm" })],
    });
    const { app } = await atMode(row.id, { rows: [row] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    await app.user.click(within(card("atStart", 1)).getByRole("button", { name: "Remove" }));

    expect(within(section("atStart")).getAllByRole("group")).toHaveLength(1);
    expect(
      (within(card("atStart", 1)).getByLabelText("What it does") as HTMLSelectElement).value,
    ).toBe("swing_arm");
  });
});

describe("editing a mode (S14, S18, S84)", () => {
  const ROW = mode(1, {
    name: "Fishing",
    version: 3,
    steps: [
      shownStep({ type: "look", yaw: -12.5, pitch: 30 }, every(45, 75)),
      shownStep({ type: "send_chat", message: "hello" }),
      shownStep({ type: "sneak", on: false }, ON_RESPAWN, 25),
    ],
  });

  it("fills the form from the row, by section", async () => {
    await atMode(ROW.id, { rows: [ROW] });

    expect(await screen.findByRole("heading", { name: "Fishing", level: 1 })).toBeDefined();
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Fishing");
    const repeating = card("repeating", 1);
    expect(
      (
        within(repeating).getByLabelText(
          "Yaw (left or right, as the F3 screen shows it)",
        ) as HTMLInputElement
      ).value,
    ).toBe("-12.5");
    expect((within(repeating).getByLabelText("Every") as HTMLInputElement).value).toBe("45");
    expect((within(repeating).getByLabelText("Every: unit") as HTMLSelectElement).value).toBe(
      "seconds",
    );
    expect((within(card("atStart", 1)).getByLabelText("Message") as HTMLInputElement).value).toBe(
      "hello",
    );
    expect(
      (within(card("onRespawn", 1)).getByLabelText("Sneaking") as HTMLSelectElement).value,
    ).toBe("stop");
    expect(
      (within(card("onRespawn", 1)).getByLabelText("Chance (%)") as HTMLInputElement).value,
    ).toBe("25");
  });

  it("offers Save only while the form differs from what was loaded", async () => {
    const { app } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });
    const name = screen.getByLabelText("Name");

    expect(saveButton().disabled).toBe(true);
    await fill(app, name, "Fishing 2");
    expect(saveButton().disabled).toBe(false);
    await fill(app, name, "Fishing");

    expect(saveButton().disabled).toBe(true);
    expect(calls(app, "updateMode")).toEqual([]);
  });

  it("sends the version read and the sections' order, stays, and is clean again", async () => {
    const { app, state } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });

    await fill(app, screen.getByLabelText("Name"), "Fishing 2");
    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(1));
    expect(calls(app, "updateMode")[0]).toEqual([
      ROW.id,
      {
        version: 3,
        name: "Fishing 2",
        visibility: "private",
        steps: [
          {
            action: { type: "send_chat", message: "hello" },
            schedule: { type: "at_start" },
            probability: 100,
          },
          {
            action: { type: "look", yaw: -12.5, pitch: 30 },
            schedule: { type: "every", interval_ms: 45_000, jitter_ms: 75_000 },
            probability: 100,
          },
          {
            action: { type: "sneak", on: false },
            schedule: { type: "on_respawn" },
            probability: 25,
          },
        ],
      },
    ]);
    expect(
      await screen.findByText("Saved. Bots using this mode switch to the change at once."),
    ).toBeDefined();
    expect(app.path()).toBe(`/modes/${ROW.id}`);
    await waitFor(() => expect(saveButton().disabled).toBe(true));
    // The next edit sends the version the answer gave.
    await fill(app, screen.getByLabelText("Name"), "Fishing 3");
    await app.user.click(saveButton());
    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(2));
    expect((calls(app, "updateMode")[1] as [string, UpdateModeRequest])[1].version).toBe(4);
    expect(state.rows[0]?.version).toBe(5);
  });

  it("says nothing changed when the answer keeps the version (S21, S84)", async () => {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: { updateMode: async () => ROW },
    });
    await screen.findByRole("heading", { name: "Fishing", level: 1 });

    await fill(app, screen.getByLabelText("Name"), "Fishing 2");
    await app.user.click(saveButton());

    expect(
      await screen.findByText("Nothing to save: the mode already reads like this."),
    ).toBeDefined();
  });

  it("sends nothing for an interleaved mode until something changes, then the sections' order", async () => {
    const interleaved = mode(1, {
      steps: [
        shownStep({ type: "jump" }, every(10)),
        shownStep({ type: "swing_arm" }),
        shownStep({ type: "use_item" }, every(20)),
        shownStep({ type: "sneak", on: true }, ON_RESPAWN),
        shownStep({ type: "attack_facing_entity" }),
      ],
    });
    const { app } = await atMode(interleaved.id, { rows: [interleaved] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(saveButton().disabled).toBe(true);
    await app.user.keyboard("{Enter}");
    expect(calls(app, "updateMode")).toEqual([]);

    await fill(app, screen.getByLabelText("Name"), "Renamed");
    await app.user.click(saveButton());
    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(1));
    const [, request] = calls(app, "updateMode")[0] as [string, UpdateModeRequest];
    expect(request.steps.map((step) => step.action.type)).toEqual([
      "swing_arm",
      "attack_facing_entity",
      "jump",
      "use_item",
      "sneak",
    ]);
  });

  it("shows the switch only from visibility_editable, and sends what it says", async () => {
    const shareable = mode(1, { visibility_editable: true });
    const { app } = await atMode(shareable.id, { rows: [shareable] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    const select = screen.getByLabelText("Who can see it") as HTMLSelectElement;
    expect([...select.options].map((option) => option.textContent)).toEqual([
      "Private",
      "Shared with everyone",
    ]);
    await app.user.selectOptions(select, "Shared with everyone");
    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(1));
    expect((calls(app, "updateMode")[0] as [string, UpdateModeRequest])[1].visibility).toBe(
      "shared",
    );
  });

  it("says who sees it, without a switch, when the flag is off", async () => {
    const fixed = mode(1, { visibility_editable: false });
    await atMode(fixed.id, { rows: [fixed] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect((screen.getByLabelText("Who can see it") as HTMLInputElement).readOnly).toBe(true);
    expect((screen.getByLabelText("Who can see it") as HTMLInputElement).value).toBe("Private");
  });

  it("shows whose it is and its mark, and sends neither (R5)", async () => {
    const marked = mode(1, {
      unlisted_command: true,
      owner: { id: "01900000-0000-7000-8000-000000000001", username: "afkadmin" },
      ownership: "someone_else",
      steps: [shownStep({ type: "send_chat", message: "/warp farm" })],
    });
    const { app } = await atMode(marked.id, { rows: [marked], stamped: true });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(screen.getByText("afkadmin · Private · 1 step")).toBeDefined();
    expect(
      screen.getByText(
        "Sends a command off the allowlist: only someone who manages a bot can choose this mode.",
      ),
    ).toBeDefined();
    await fill(app, screen.getByLabelText("Name"), "Renamed");
    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "updateModeWithCommands")).toHaveLength(1));
    const [, request] = calls(app, "updateModeWithCommands")[0] as [string, object];
    expect(Object.keys(request).sort()).toEqual(["name", "steps", "version", "visibility"]);
  });
});

describe("dragging a step (U68, R167)", () => {
  const TWO = mode(1, {
    steps: [shownStep({ type: "jump" }, every(10)), shownStep({ type: "swing_arm" }, every(20))],
  });

  it("gives each card a named handle, within its section", async () => {
    await atMode(TWO.id, { rows: [TWO] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(within(card("repeating", 1)).getByRole("button", { name: "Drag step 1" })).toBeDefined();
    expect(within(card("repeating", 2)).getByRole("button", { name: "Drag step 2" })).toBeDefined();
  });

  it("tells a screen reader how, and that it's a step that's sorted", async () => {
    await atMode(TWO.id, { rows: [TWO] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    const handle = within(card("repeating", 1)).getByRole("button", { name: "Drag step 1" });
    const instructions = document.getElementById(handle.getAttribute("aria-describedby") ?? "");

    expect(instructions?.textContent).toBe(
      "To pick up a step, press Space or Enter. Use the arrow keys to move it, then Space or Enter to drop it, or Escape to cancel.",
    );
    expect(handle.getAttribute("aria-roledescription")).toBe("sortable");
  });

  it("has no handle on a locked mode", async () => {
    const locked = { ...TWO, editable: false };
    await atMode(locked.id, { rows: [locked] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(screen.queryByRole("button", { name: /^Drag step/ })).toBeNull();
  });

  it("disables the handle while Confirm it's you is up", async () => {
    const { app } = await atMode(TWO.id, {
      rows: [TWO],
      handlers: {
        updateMode: async () => {
          throw refusal(403, "mode_needs_step_up");
        },
        updateModeWithCommands: async () => {
          throw refusal(403, "step_up_required");
        },
      },
    });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    await fill(app, screen.getByLabelText("Name"), "Renamed");

    await app.user.click(saveButton());
    await screen.findByLabelText("Current password");

    expect(
      (
        within(card("repeating", 1)).getByRole("button", {
          name: "Drag step 1",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
  });
});

describe("a command off the allowlist (S16, R162)", () => {
  const COMMAND = mode(1, { steps: [shownStep({ type: "send_chat", message: "hello" })] });
  const PROMPT =
    "Enter your password to save a command off the allowlist. It counts for 5 minutes.";

  /** The mode's chat step, changed to a command. */
  async function typed(options: Parameters<typeof atMode>[1] = {}) {
    const found = await atMode(COMMAND.id, { rows: [COMMAND], ...options });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    await fill(found.app, within(card("atStart", 1)).getByLabelText("Message"), "/warp home");
    return found;
  }

  it("resends the same body to the twin after Confirm it's you", async () => {
    const { app } = await typed();

    await app.user.click(saveButton());
    expect(await screen.findByText(PROMPT)).toBeDefined();
    await app.user.type(screen.getByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    await vi.waitFor(() => expect(calls(app, "updateModeWithCommands")).toHaveLength(2));
    const body = calls(app, "updateMode")[0];
    expect(body).toEqual([
      COMMAND.id,
      {
        version: 1,
        name: "Mode 1",
        visibility: "private",
        steps: [
          {
            action: { type: "send_chat", message: "/warp home" },
            schedule: { type: "at_start" },
            probability: 100,
          },
        ],
      },
    ]);
    expect(calls(app, "updateModeWithCommands")).toEqual([body, body]);
    expect(
      await screen.findByText("Saved. Bots using this mode switch to the change at once."),
    ).toBeDefined();
  });

  it("asks nothing while the step-up still holds", async () => {
    const { app } = await typed({ stamped: true });

    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "updateModeWithCommands")).toHaveLength(1));
    expect(screen.queryByText(PROMPT)).toBeNull();
  });

  it("locks the form while Confirm it's you is up, so what's sent is what's shown", async () => {
    const { app } = await typed();

    await app.user.click(saveButton());
    await screen.findByText(PROMPT);

    expect(screen.getByLabelText("Name").hasAttribute("readonly")).toBe(true);
    expect(within(card("atStart", 1)).getByLabelText("Message").hasAttribute("readonly")).toBe(
      true,
    );
    expect(
      (within(section("atStart")).getByRole("button", { name: "Add step" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(
      (within(card("atStart", 1)).getByRole("button", { name: "Remove" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(saveButton().disabled).toBe(true);
  });

  it("sends nothing to the twin after Cancel, and the form opens again", async () => {
    const { app } = await typed();

    await app.user.click(saveButton());
    await screen.findByText(PROMPT);
    await app.user.click(within(screen.getByRole("alert")).getByRole("button", { name: "Cancel" }));

    expect(screen.queryByText(PROMPT)).toBeNull();
    expect(screen.getByLabelText("Name").hasAttribute("readonly")).toBe(false);
    expect(calls(app, "updateModeWithCommands")).toHaveLength(1);
  });

  it("creates through the twin too", async () => {
    const { app } = await atMode("new", { stamped: true });
    await screen.findByRole("heading", { name: "New mode", level: 1 });

    await fill(app, screen.getByLabelText("Name"), "Warp");
    await app.user.click(within(section("atStart")).getByRole("button", { name: "Add step" }));
    await app.user.selectOptions(
      within(card("atStart", 1)).getByLabelText("What it does"),
      "Send chat or a command",
    );
    await fill(app, within(card("atStart", 1)).getByLabelText("Message"), "/warp home");
    await app.user.click(saveButton());

    await vi.waitFor(() => expect(calls(app, "createModeWithCommands")).toHaveLength(1));
    expect(calls(app, "createModeWithCommands")[0]).toEqual(calls(app, "createMode")[0]);
  });
});

describe("the refusals at the fields (S12, S82, R158, R164)", () => {
  const ROW = mode(1, {
    steps: [
      shownStep({ type: "jump" }, every(10)),
      shownStep({ type: "send_chat", message: "hi" }),
      shownStep({ type: "send_chat", message: "hi again" }),
    ],
  });

  /** A refusal of an edit at `path` with `message`, its field invalid. */
  async function refusedAt(path: string, message: string) {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async () => {
          throw refusal(422, "validation_failed", { fields: [{ path, message }] });
        },
      },
    });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    await fill(app, screen.getByLabelText("Name"), "Renamed");
    await app.user.click(saveButton());
    return screen.findByText(message);
  }

  it("shows a name's refusal at the name", async () => {
    const said = await refusedAt("name", "is already another mode's name");

    expect(screen.getByLabelText("Name").getAttribute("aria-describedby")).toContain(said.id);
  });

  it("goes through the order sent: the second chat step at start is steps[1]", async () => {
    const said = await refusedAt(
      "steps[1]",
      "is a second chat step at start; a mode can have only one",
    );

    expect(
      within(card("atStart", 2)).getByLabelText("What it does").getAttribute("aria-describedby"),
    ).toContain(said.id);
  });

  it("shows a repeating step's wait at its field, and the action's rule on its card", async () => {
    const said = await refusedAt(
      "steps[2].schedule.interval_ms",
      "must be at least 30 seconds for chat",
    );

    expect(
      within(card("repeating", 1)).getByLabelText("Every").getAttribute("aria-describedby"),
    ).toContain(said.id);
  });

  it("shows a rule it has no field for as the form's line", async () => {
    const said = await refusedAt("steps", "must hold at most 32 steps");

    expect(said.closest('[role="alert"]')).not.toBeNull();
  });

  it("clears a server's error at the next save", async () => {
    let first = true;
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async (_id, request) => {
          if (first) {
            first = false;
            throw refusal(422, "validation_failed", {
              fields: [{ path: "steps", message: "must hold at most 32 steps" }],
            });
          }
          return { ...ROW, name: request.name, version: 2 };
        },
      },
    });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    await fill(app, screen.getByLabelText("Name"), "Renamed");

    await app.user.click(saveButton());
    await screen.findByText("must hold at most 32 steps");
    await app.user.click(saveButton());

    await waitFor(() => expect(screen.queryByText("must hold at most 32 steps")).toBeNull());
    await vi.waitFor(() => expect(calls(app, "updateMode")).toHaveLength(2));
  });

  it.each<[string, string]>([
    ["50.5", "must be a whole number"],
    ["300", "is out of range"],
    ["", "must be a number"],
  ])("refuses a chance of '%s' at its field, before any request (S82)", async (chance, message) => {
    const { app } = await atMode(ROW.id, { rows: [ROW] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    const field = within(card("repeating", 1)).getByLabelText("Chance (%)");

    await fill(app, field, chance);
    await app.user.click(saveButton());

    const said = await screen.findByText(message);
    expect(field.getAttribute("aria-describedby")).toContain(said.id);
    expect(calls(app, "updateMode")).toEqual([]);
  });

  it("leaves a chance of 0 to the server, which answers at the field", async () => {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async () => {
          throw refusal(422, "validation_failed", {
            fields: [{ path: "steps[2].probability", message: "must be from 1 to 100 percent" }],
          });
        },
      },
    });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    const field = within(card("repeating", 1)).getByLabelText("Chance (%)");

    await fill(app, field, "0");
    await app.user.click(saveButton());

    const said = await screen.findByText("must be from 1 to 100 percent");
    expect(field.getAttribute("aria-describedby")).toContain(said.id);
    expect(calls(app, "updateMode")).toHaveLength(1);
  });

  it("counts down a 429 on Save", async () => {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async () => {
          throw refusal(429, "rate_limited", { retry_after_secs: 30 });
        },
      },
    });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    await fill(app, screen.getByLabelText("Name"), "Renamed");

    await app.user.click(saveButton());

    await waitFor(() => expect(saveButton().disabled).toBe(true));
    expect(calls(app, "updateMode")).toHaveLength(1);
  });

  it("shows a 403 as the server's text and reads the modes again", async () => {
    const { app } = await atMode(ROW.id, {
      rows: [ROW],
      handlers: {
        updateMode: async () => {
          throw refusal(403, "forbidden", { message: "You don't have permission to do this." });
        },
      },
    });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });
    const reads = () => calls(app, "modes").length;
    const before = reads();
    await fill(app, screen.getByLabelText("Name"), "Renamed");

    await app.user.click(saveButton());

    expect(await screen.findByText("You don't have permission to do this.")).toBeDefined();
    await vi.waitFor(() => expect(reads()).toBeGreaterThan(before));
  });
});

describe("the locked view (U73, R160, R161)", () => {
  const BUILT_IN = mode(1, {
    name: "afk",
    visibility: "shared",
    ownership: "built_in",
    editable: false,
    deletable: false,
    visibility_editable: false,
    steps: [
      shownStep({ type: "select_hotbar_slot", slot: 3 }),
      shownStep({ type: "rotate_random", max_yaw: 30, max_pitch: 10 }, every(60, 90)),
      shownStep({ type: "sneak", on: true }, ON_RESPAWN),
    ],
  });

  it("shows the same sections and cards, read-only rather than disabled, with no way to change them", async () => {
    await atMode(BUILT_IN.id, { rows: [BUILT_IN] });
    await screen.findByRole("heading", { name: "afk", level: 1 });

    for (const field of screen.getAllByRole("textbox") as HTMLInputElement[]) {
      expect(field.readOnly).toBe(true);
      expect(field.disabled).toBe(false);
    }
    for (const field of screen.getAllByRole("spinbutton") as HTMLInputElement[]) {
      expect(field.readOnly).toBe(true);
      expect(field.disabled).toBe(false);
    }
    expect(screen.queryByRole("combobox")).toBeNull();
    for (const name of ["Add step", "Remove", "Move up", "Move down", "Save", "Delete"]) {
      expect(screen.queryByRole("button", { name })).toBeNull();
    }
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("afk");
    const slot = within(card("atStart", 1));
    expect((slot.getByLabelText("What it does") as HTMLInputElement).value).toBe(
      "Select a hotbar slot",
    );
    expect((slot.getByLabelText("Slot") as HTMLInputElement).value).toBe("4");
    const repeating = within(card("repeating", 1));
    expect((repeating.getByLabelText("Every") as HTMLInputElement).value).toBe("1");
    expect((repeating.getByLabelText("Every: unit") as HTMLInputElement).value).toBe("minutes");
    expect((repeating.getByLabelText("Plus up to, at random") as HTMLInputElement).value).toBe(
      "90",
    );
    expect(
      (repeating.getByLabelText("Plus up to, at random: unit") as HTMLInputElement).value,
    ).toBe("seconds");
    expect(
      (within(card("onRespawn", 1)).getByLabelText("Sneaking") as HTMLInputElement).value,
    ).toBe("Start");
    expect(screen.getByText("Built in · Shared · 3 steps")).toBeDefined();
  });

  it("marks a cut command outside its text", async () => {
    const cut = mode(1, {
      editable: false,
      deletable: false,
      visibility_editable: false,
      ownership: "someone_else",
      steps: [shownStep({ type: "send_chat", message: "/warp", arguments_hidden: true })],
    });
    await atMode(cut.id, { rows: [cut] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    const message = within(card("atStart", 1)).getByLabelText("Message") as HTMLInputElement;
    expect(message.value).toBe("/warp");
    expect(
      within(card("atStart", 1)).getByText(
        "Its arguments are hidden: only someone who can edit this mode sees them.",
      ),
    ).toBeDefined();
  });

  it("locks a cut command's mode even if the row says editable", async () => {
    const cut = mode(1, {
      steps: [shownStep({ type: "send_chat", message: "/warp", arguments_hidden: true })],
    });
    await atMode(cut.id, { rows: [cut] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it.each<[string, Partial<ModeResponse>, string | null]>([
    [
      "an unknown action",
      { steps: [shownStep({ type: "dance" as ModeActionKind }, every(10))] },
      SECTION.repeating,
    ],
    [
      "an unknown schedule",
      {
        steps: [shownStep({ type: "jump" }, { type: "at_dawn" as ModeScheduleKind })],
      },
      "Other steps",
    ],
  ])("opens a mode with %s read-only, one line for that step", async (_name, changes, block) => {
    const unknown = mode(1, changes);
    await atMode(unknown.id, { rows: [unknown] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect(
      screen.getByText(
        "This mode has a step this app doesn't know, so it opens read-only. Update the app to edit it.",
      ),
    ).toBeDefined();
    expect(
      within(screen.getByRole("group", { name: block ?? "" })).getByText(
        "This app doesn't know this step.",
      ),
    ).toBeDefined();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("opens a mode with an unknown visibility read-only, showing it as sent", async () => {
    const unknown = mode(1, { visibility: "secret" as ModeVisibility });
    await atMode(unknown.id, { rows: [unknown] });
    await screen.findByRole("heading", { name: "Mode 1", level: 1 });

    expect((screen.getByLabelText("Who can see it") as HTMLInputElement).value).toBe("secret");
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("follows the row while it's open, since nothing there can be lost", async () => {
    const { app } = await atMode(BUILT_IN.id, { rows: [BUILT_IN] });
    await screen.findByRole("heading", { name: "afk", level: 1 });

    await act(async () =>
      app.runtime.queryClient.setQueryData(MODES_LIST_KEY, {
        pages: [{ items: [{ ...BUILT_IN, name: "afk 2" }] }],
        pageParams: [undefined],
      }),
    );

    expect(await screen.findByRole("heading", { name: "afk 2", level: 1 })).toBeDefined();
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("afk 2");
  });
});

describe("a mode that isn't there", () => {
  it("says so, with the way back", async () => {
    await atMode(modeId(5));

    expect(
      await screen.findByText("This mode doesn't exist any more, or you can't see it."),
    ).toBeDefined();
    expect(within(screen.getByRole("main")).getByRole("link", { name: "Modes" })).toBeDefined();
  });

  it("says why the modes couldn't be read", async () => {
    await atMode(modeId(5), {
      handlers: {
        modes: async () => {
          throw refusal(500, "internal");
        },
      },
    });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/^Couldn't load the modes: refused: internal/);
  });
});
