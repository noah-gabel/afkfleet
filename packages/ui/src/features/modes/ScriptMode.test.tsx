// A scripted mode's screen (Plan.md P15.25, P15.35; ADR-0036, D15; ADR-0037, C5; ADR-0045): who
// chooses a script, the examples to start from, the request built from the form, the server's
// errors at their fields and at their line in the editor, the any-time warning, and the read-only
// view a Member gets of a shared script. The editor itself is ScriptEditor.test.tsx's.
import { forEachDiagnostic } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SCRIPT_API } from "../../generated/script-api";
import { me } from "../../test/fixtures";
import { mode, modeId } from "../../test/live";
import { atMode, calls, fill, saveButton } from "../../test/modes";
import { refusal } from "../../test/signIn";
import { SCRIPT_EDITOR_HOST } from "../../test/styleGuard";

const ADMIN = me("admin");
const COBBLESTONE = SCRIPT_API.examples.find((one) => one.id === "cobblestone_generator");
const BALANCE = SCRIPT_API.examples.find((one) => one.id === "balance");
if (COBBLESTONE === undefined || BALANCE === undefined) {
  throw new Error("no examples");
}

/** The script editor's view. */
function editor(): EditorView {
  const host = document.querySelector(SCRIPT_EDITOR_HOST);
  const dom = host?.shadowRoot?.querySelector(".cm-editor");
  const view =
    dom === null || dom === undefined ? null : EditorView.findFromDOM(dom as HTMLElement);
  if (view === null) {
    throw new Error("no editor");
  }
  return view;
}

/** The marks on the editor's text, as `[text, message]`. */
function marks(): [string, string][] {
  const view = editor();
  const found: [string, string][] = [];
  forEachDiagnostic(view.state, (diagnostic, from, to) => {
    found.push([view.state.doc.sliceString(from, to), diagnostic.message]);
  });
  return found;
}

describe("a new scripted mode (ADR-0036, D15)", () => {
  it("offers an Admin a script, and a Member only steps", async () => {
    await atMode("new", { user: ADMIN });
    expect(screen.getByRole("combobox", { name: "What it runs" })).toBeTruthy();
  });

  it("offers a Member no choice: their modes are steps", async () => {
    await atMode("new");
    expect(screen.queryByRole("combobox", { name: "What it runs" })).toBeNull();
  });

  it("starts from an example and sends its body with no steps, then opens the new mode", async () => {
    const { app } = await atMode("new", { user: ADMIN });

    await app.user.selectOptions(screen.getByRole("combobox", { name: "What it runs" }), "script");
    await app.user.selectOptions(
      screen.getByRole("combobox", { name: "Start from an example" }),
      "cobblestone_generator",
    );
    await app.user.click(screen.getByRole("button", { name: "Use example" }));
    await fill(app, screen.getByRole("textbox", { name: "Name" }), "Cobble");
    expect(editor().state.doc.toString()).toBe(COBBLESTONE?.script.source);
    expect((screen.getByRole("textbox", { name: "Block 1" }) as HTMLInputElement).value).toBe(
      "minecraft:cobblestone",
    );
    await app.user.click(saveButton());

    await waitFor(() => expect(calls(app, "createMode")).toHaveLength(1));
    expect(calls(app, "createMode")[0]).toEqual([
      {
        name: "Cobble",
        visibility: "private",
        steps: [],
        script: {
          source: COBBLESTONE?.script.source,
          leash: 8,
          tool_guard: 2,
          block_guard: ["minecraft:cobblestone"],
          messages: [],
          patterns: [],
          signs: [],
        },
      },
    ]);
    expect(await screen.findByRole("heading", { name: "Cobble" })).toBeTruthy();
  });

  it("shows the server's error at the script's line, and a one-line text's at its character", async () => {
    const answer = refusal(422, "validation", {
      fields: [
        { path: "script.source", message: "`kees` isn't defined", line: 2, column: 5 },
        { path: "script.messages[0].text", message: "holds a brace", line: 1, column: 7 },
        { path: "script.leash", message: "must be 1 to 64 blocks" },
      ],
    });
    const { app } = await atMode("new", {
      user: ADMIN,
      handlers: {
        createMode: async () => {
          throw answer;
        },
      },
    });
    await app.user.selectOptions(screen.getByRole("combobox", { name: "What it runs" }), "script");
    await app.user.selectOptions(
      screen.getByRole("combobox", { name: "Start from an example" }),
      "balance",
    );
    await app.user.click(screen.getByRole("button", { name: "Use example" }));
    await fill(app, screen.getByRole("textbox", { name: "Name" }), "Pay");

    await app.user.click(saveButton());

    expect(await screen.findByText("holds a brace (at character 7)")).toBeTruthy();
    expect(screen.getByText("must be 1 to 64 blocks")).toBeTruthy();
    await waitFor(() =>
      expect(marks().map(([, message]) => message)).toContain("`kees` isn't defined"),
    );
  });
});

describe("a mode's declared signs (Plan.md P16.15; ADR-0036, M16)", () => {
  /** A new scripted mode with one sign whose first line is `line`, saved. */
  async function saveWithSign(line: string, createMode?: () => Promise<never>) {
    const { app } = await atMode("new", {
      user: ADMIN,
      handlers: createMode === undefined ? {} : { createMode },
    });
    await app.user.selectOptions(screen.getByRole("combobox", { name: "What it runs" }), "script");
    await fill(app, screen.getByRole("textbox", { name: "Name" }), "Signer");
    await app.user.click(screen.getByRole("button", { name: "Add sign" }));
    const sign = screen.getByRole("group", { name: "Sign 1" });
    await fill(app, within(sign).getByRole("textbox", { name: "Key" }), "confirm");
    await fill(app, within(sign).getByRole("textbox", { name: "Line 1" }), line);
    await fill(app, within(sign).getByRole("textbox", { name: "Line 3" }), "afk");
    await app.user.click(saveButton());
    return app;
  }

  it("sends each sign with its lines, the empty ones at its end left out", async () => {
    const app = await saveWithSign("yes");

    await waitFor(() => expect(calls(app, "createMode")).toHaveLength(1));
    expect(calls(app, "createMode")[0]?.[0]).toMatchObject({
      script: { signs: [{ key: "confirm", lines: ["yes", "", "afk"], values: [] }] },
    });
  });

  it("shows the server's refusal of a sign's line at that line", async () => {
    const answer = refusal(422, "validation", {
      fields: [{ path: "script.signs[0].lines[0]", message: "names no value" }],
    });

    await saveWithSign("{price}", async () => {
      throw answer;
    });

    const sign = screen.getByRole("group", { name: "Sign 1" });
    const line = within(sign).getByRole("textbox", { name: "Line 1" });
    await waitFor(() => expect(line.getAttribute("aria-invalid")).toBe("true"));
    expect(within(sign).getByText("names no value")).toBeTruthy();
  });
});

describe("a pattern that may come at any time (ADR-0037, C5)", () => {
  it("is warned of as the riskier kind, and a reply isn't", async () => {
    const { app } = await atMode(modeId(1), {
      user: ADMIN,
      rows: [mode(1, { script: BALANCE?.script })],
    });
    const pattern = await screen.findByRole("group", { name: "Pattern 1" });
    expect(within(pattern).queryByText(/^Riskier:/)).toBeNull();

    await app.user.selectOptions(
      within(pattern).getByRole("combobox", { name: "When it counts" }),
      "any_time",
    );

    expect(within(pattern).getByText(/^Riskier:/)).toBeTruthy();
  });
});

describe("a shared scripted mode a Member runs (ADR-0036, D15)", () => {
  const shared = mode(2, {
    visibility: "shared",
    ownership: "someone_else",
    editable: false,
    deletable: false,
    visibility_editable: false,
    script: BALANCE?.script,
  });

  it("is read-only, its script in the editor, with no copy to make", async () => {
    await atMode(modeId(2), { rows: [shared] });

    await screen.findByRole("group", { name: "Message 2" });
    expect(editor().state.readOnly).toBe(true);
    expect(editor().state.doc.toString()).toBe(BALANCE?.script.source);
    expect(
      (screen.getByRole("textbox", { name: "Leash, in blocks" }) as HTMLInputElement).readOnly,
    ).toBe(true);
    expect(screen.queryByRole("button", { name: "Make a copy" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Add message" })).toBeNull();
  });

  it("shows the place a bot's error links to", async () => {
    const { app } = await atMode(modeId(2), { rows: [shared] });
    await screen.findByRole("group", { name: "Message 2" });

    await act(() =>
      app.runtime.router.navigate({
        to: "/modes/$id",
        params: { id: modeId(2) },
        search: { line: 3, column: 5 },
      }),
    );

    await waitFor(() => {
      const view = editor();
      const line = view.state.doc.lineAt(view.state.selection.main.head);
      expect([line.number, view.state.selection.main.head - line.from + 1]).toEqual([3, 5]);
    });
  });
});
