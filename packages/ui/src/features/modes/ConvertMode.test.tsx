// "Convert to script" in the mode editor (Plan.md P15.27; ADR-0036, D17; ADR-0046): offered on a
// step mode to whoever writes scripts, the form's steps converted by the app's Rust side (a direct
// call: a chat step can hold a password), the script in the editor with what it does differently
// named by its steps, "Keep the steps", the save of the script, and a problem at its step. The
// stopgap `enter_minecart` is offered only on a step that has it, marked as going away.
import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApiError } from "../../api/FakeApiClient";
import type { ScriptConversion } from "../../generated/ScriptConversion";
import { me } from "../../test/fixtures";
import { mode, modeId } from "../../test/live";
import {
  atMode,
  calls,
  card,
  every,
  ON_RESPAWN,
  saveButton,
  section,
  shownScript,
  shownStep,
} from "../../test/modes";
import { SCRIPT_EDITOR_HOST } from "../../test/styleGuard";

const ADMIN = me("admin");

/** A step mode: a slot when it joins, a swing every 20 s, `/login` after it respawns. */
const STEPS = mode(1, {
  name: "Swing",
  steps: [
    shownStep({ type: "select_hotbar_slot", slot: 0 }),
    shownStep({ type: "swing_arm" }, every(20)),
    shownStep({ type: "send_chat", message: "/login hunter2" }, ON_RESPAWN),
  ],
});

/** What the app's Rust side answers for `STEPS`. */
const CONVERTED: ScriptConversion = {
  script: shownScript({
    source: "fn on_join(first) {\n    hotbar.select(1); // when it joins, step 1\n}\n",
    leash: 8,
    tool_guard: 2,
    block_guard: [],
    messages: [{ key: "respawn_1", text: "/login hunter2", values: [] }],
    patterns: [],
  }),
  notes: [
    { step: 1, kind: "swing_skips_entities" },
    { step: 1, kind: "shares_limit", other: 0, every_ms: 500 },
  ],
};

/** The text the script editor holds. */
function editorText(): string {
  const host = document.querySelector(SCRIPT_EDITOR_HOST);
  return host?.shadowRoot?.querySelector(".cm-content")?.textContent ?? "";
}

function convertButton(): HTMLElement {
  return screen.getByRole("button", { name: "Convert to script" });
}

describe("Convert to script (P15.27)", () => {
  it("is offered on a step mode to an Admin, and not to a Member", async () => {
    await atMode(modeId(1), { rows: [STEPS], user: ADMIN });
    expect(convertButton()).toBeTruthy();
  });

  it("isn't offered to a Member, who writes no script", async () => {
    await atMode(modeId(1), { rows: [STEPS] });
    expect(screen.queryByRole("button", { name: "Convert to script" })).toBeNull();
  });

  it("sends the form's steps and shows the script with what it does differently, step by step", async () => {
    const { app } = await atMode(modeId(1), {
      rows: [STEPS],
      user: ADMIN,
      handlers: { convertToScript: async () => CONVERTED },
    });

    await app.user.click(convertButton());

    await waitFor(() => expect(editorText()).toContain("hotbar.select(1);"));
    expect(calls(app, "convertToScript")).toEqual([
      [
        {
          steps: [
            {
              action: { type: "select_hotbar_slot", slot: 0 },
              schedule: { type: "at_start" },
              probability: 100,
            },
            {
              action: { type: "swing_arm" },
              schedule: { type: "every", interval_ms: 20_000, jitter_ms: 0 },
              probability: 100,
            },
            {
              action: { type: "send_chat", message: "/login hunter2" },
              schedule: { type: "on_respawn" },
              probability: 100,
            },
          ],
        },
      ],
    ]);
    const notice = screen.getByRole("region", { name: "Converted to a script" });
    expect(
      within(notice)
        .getAllByRole("listitem")
        .map((item) => item.textContent),
    ).toEqual([
      "While it's online, step 1: swings only while the crosshair isn't on an entity. A script swings with a left click, which would hit one.",
      "While it's online, step 1: shares a script's limit of once every 0.5 s with when it joins, step 1: a run that close to one of its runs is skipped.",
    ]);
    expect(within(notice).getByText(/lets go of every key and button/)).toBeTruthy();
    // The steps are gone from the form until "Keep the steps".
    expect(screen.queryByRole("group", { name: "While it's online" })).toBeNull();
  });

  // A script's body has no steps, so the plain route takes it, whatever its messages hold.
  it("saves the converted mode as a script with no steps", async () => {
    const { app } = await atMode(modeId(1), {
      rows: [STEPS],
      user: ADMIN,
      handlers: { convertToScript: async () => CONVERTED },
    });
    await app.user.click(convertButton());
    await waitFor(() => expect(editorText()).toContain("hotbar.select(1);"));

    await app.user.click(saveButton());

    await waitFor(() => expect(calls(app, "updateMode")).toHaveLength(1));
    expect(calls(app, "updateMode")[0]).toEqual([
      modeId(1),
      {
        version: 1,
        name: "Swing",
        visibility: "private",
        steps: [],
        script: {
          source: CONVERTED.script?.source,
          leash: 8,
          tool_guard: 2,
          block_guard: [],
          messages: [{ key: "respawn_1", text: "/login hunter2", values: [] }],
          patterns: [],
        },
      },
    ]);
    await waitFor(() =>
      expect(screen.queryByRole("region", { name: "Converted to a script" })).toBeNull(),
    );
  });

  it("keeps the steps on the user's word, and nothing is sent", async () => {
    const { app } = await atMode(modeId(1), {
      rows: [STEPS],
      user: ADMIN,
      handlers: { convertToScript: async () => CONVERTED },
    });
    await app.user.click(convertButton());
    await waitFor(() => expect(editorText()).toContain("hotbar.select(1);"));

    await app.user.click(screen.getByRole("button", { name: "Keep the steps" }));

    expect(within(section("repeating")).getByRole("group", { name: "Step 1" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Converted to a script" })).toBeNull();
    expect(calls(app, "updateMode")).toEqual([]);
    expect(calls(app, "updateModeWithCommands")).toEqual([]);
  });

  it("shows why the steps can't be a script at the step that keeps them from it", async () => {
    const { app } = await atMode(modeId(1), {
      rows: [STEPS],
      user: ADMIN,
      handlers: {
        convertToScript: async () => ({
          notes: [],
          problem: {
            path: "steps[2].action.message",
            message: "can't be a script's declared message: the command's name holds a brace",
          },
        }),
      },
    });

    await app.user.click(convertButton());

    await waitFor(() =>
      expect(
        within(card("onRespawn", 1)).getByText(
          "can't be a script's declared message: the command's name holds a brace",
        ),
      ).toBeTruthy(),
    );
    expect(screen.queryByRole("region", { name: "Converted to a script" })).toBeNull();
    expect(document.querySelector(SCRIPT_EDITOR_HOST)).toBeNull();
  });

  it("shows a rule the steps break at its field, as a save would", async () => {
    const { app } = await atMode(modeId(1), {
      rows: [STEPS],
      user: ADMIN,
      handlers: {
        convertToScript: async () => {
          throw fakeApiError("invalid_input", {
            fields: [{ path: "steps[1].probability", message: "must be 1 to 100" }],
          });
        },
      },
    });

    await app.user.click(convertButton());

    await waitFor(() =>
      expect(within(card("repeating", 1)).getByText("must be 1 to 100")).toBeTruthy(),
    );
  });
});

describe("the stopgap step (ADR-0036, D17)", () => {
  it("isn't offered on a new step", async () => {
    const { app } = await atMode("new");

    await app.user.click(within(section("atStart")).getByRole("button", { name: "Add step" }));

    const options = within(card("atStart", 1))
      .getAllByRole("option")
      .map((option) => option.textContent);
    expect(options.some((text) => text?.includes("minecart"))).toBe(false);
  });

  it("stays on a stored step that has it, marked as going away with the way out", async () => {
    await atMode(modeId(2), {
      rows: [
        mode(2, {
          name: "Back in",
          steps: [shownStep({ type: "enter_minecart" }, ON_RESPAWN)],
        }),
      ],
    });

    const step = card("onRespawn", 1);
    expect(
      (within(step).getByRole("combobox", { name: "What it does" }) as HTMLSelectElement).value,
    ).toBe("enter_minecart");
    expect(within(step).getByText(/Going away in the next release/)).toBeTruthy();
    expect(within(step).getByText(/Convert to script/)).toBeTruthy();
    // No field of its own: what it does, and its chance.
    const labels = [...step.querySelectorAll<HTMLInputElement>("input, select")].map(
      (field) => field.labels?.[0]?.textContent ?? "",
    );
    expect(labels).toEqual(["What it does", "Chance (%)"]);
  });
});
