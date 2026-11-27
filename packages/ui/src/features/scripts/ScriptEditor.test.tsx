// The script editor in jsdom (Plan.md P15.24, P15.25; ADR-0045): mounted in its host's shadow root
// with its styles adopted there and none in the document, its marks from the app's check, the
// description and the server, a check that fails as "couldn't check", and its place to show.
// Edge and the built app check how it looks and types (the e2e suite, the check exe).
import { forEachDiagnostic } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ApiClientProvider } from "../../api/context";
import { FakeApiClient, fakeApiError } from "../../api/FakeApiClient";
import type { ScriptCheck } from "../../generated/ScriptCheck";
import { SCRIPT_EDITOR_HOST } from "../../test/styleGuard";
import { ScriptEditor, type ScriptEditorProps } from "./components/ScriptEditor";

const SCRIPT = "fn on_join(first) {\n\tkees.jump.tap();\n\tkeys.jump.tapp();\n}\n";

/** The place the fake check names: `kees` on line 2. */
const KEES: ScriptCheck = {
  problem: { path: "script.source", message: "`kees` isn't defined", line: 2, column: 2 },
  level: 0,
  needs: [],
};

/** The editor rendered with a check that answers `check`. */
function renderEditor(
  props: Partial<ScriptEditorProps> = {},
  check: (source: string) => Promise<ScriptCheck> = async () => KEES,
) {
  const checkScript = vi.fn(async (request: { source: string }) => check(request.source));
  const client = new FakeApiClient({ checkScript });
  const result = render(
    <ApiClientProvider client={client}>
      <ScriptEditor label="Script" value={SCRIPT} {...props} />
    </ApiClientProvider>,
  );
  const host = result.container.querySelector<HTMLElement>(SCRIPT_EDITOR_HOST);
  if (host?.shadowRoot === null || host === null) {
    throw new Error("no shadow root");
  }
  const root = host.shadowRoot;
  const view = () => {
    const editor = root.querySelector(".cm-editor");
    const found = editor === null ? null : EditorView.findFromDOM(editor as HTMLElement);
    if (found === null) {
      throw new Error("no editor");
    }
    return found;
  };
  return { ...result, host, root, view, checkScript };
}

/** The marks on `view`'s text, as `[text, message]`. */
function marks(view: EditorView): [string, string][] {
  const found: [string, string][] = [];
  forEachDiagnostic(view.state, (diagnostic, from, to) => {
    found.push([view.state.doc.sliceString(from, to), diagnostic.message]);
  });
  return found;
}

describe("ScriptEditor", () => {
  it("mounts CodeMirror in its host's shadow root, its styles adopted there and none in the document", () => {
    const { root, view } = renderEditor();

    expect(root.querySelector(".cm-content")?.getAttribute("aria-label")).toBe("Script");
    expect(view().state.doc.toString()).toBe(SCRIPT);
    expect(root.adoptedStyleSheets.length).toBeGreaterThan(0);
    expect(root.querySelectorAll("style")).toHaveLength(0);
    expect(document.querySelectorAll("style")).toHaveLength(0);
  });

  it("marks the app's check's problem and the member the compiler can't see, and lists both", async () => {
    const { view, checkScript } = renderEditor();

    expect(await screen.findByText("2 problems.", {}, { timeout: 3000 })).toBeTruthy();
    expect(checkScript).toHaveBeenCalledWith({ source: SCRIPT });
    expect(marks(view())).toEqual([
      ["kees", "`kees` isn't defined"],
      ["tapp", "keys.jump has no tapp: the script would stop on this line."],
    ]);
    expect(
      screen.getByRole("button", { name: "Line 2, column 2: `kees` isn't defined" }),
    ).toBeTruthy();
  });

  it("says a check that failed couldn't check, keeps the description's marks, and never throws", async () => {
    const { view } = renderEditor({}, async () => {
      throw fakeApiError("script_check");
    });

    expect(
      await screen.findByText(/^Couldn't check the script: /, {}, { timeout: 3000 }),
    ).toBeTruthy();
    expect(marks(view()).map(([text]) => text)).toEqual(["tapp"]);
  });

  it("says there are no problems when there are none", async () => {
    renderEditor({ value: "fn on_join(first) {\n    keys.jump.tap();\n}\n" }, async () => ({
      level: 1,
      needs: [],
    }));

    expect(await screen.findByText("No problems found.", {}, { timeout: 3000 })).toBeTruthy();
  });

  it("says the level the server would stamp, and why in words (Plan.md P16.15)", async () => {
    const plain = "fn on_join(first) {\n    keys.jump.tap();\n}\n";
    renderEditor({ value: plain }, async () => ({ level: 1, needs: [] }));

    expect((await screen.findByTestId("script-level", {}, { timeout: 3000 })).textContent).toBe(
      "Script API level 1: every agent that runs scripts runs it.",
    );
  });

  it("names each reason a script needs level 2, and declared signs as one more", async () => {
    const { unmount } = renderEditor({}, async () => ({ level: 2, needs: ["menus", "clock"] }));
    expect((await screen.findByTestId("script-level", {}, { timeout: 3000 })).textContent).toBe(
      "Script API level 2 (uses screens and the inventory, uses the clock): only agents of this release or newer run it. A bot on an older agent waits until its agent is updated.",
    );
    unmount();

    renderEditor({ declared: { signs: ["price"], messages: [], replies: [] } }, async () => ({
      level: 1,
      needs: [],
    }));
    expect((await screen.findByTestId("script-level", {}, { timeout: 3000 })).textContent).toMatch(
      /^Script API level 2 \(declares signs\): /,
    );
  });

  it("names a sign once when the text answers sign editors and the mode declares signs", async () => {
    renderEditor({ declared: { signs: ["price"], messages: [], replies: [] } }, async () => ({
      level: 2,
      needs: ["signs"],
    }));

    expect((await screen.findByTestId("script-level", {}, { timeout: 3000 })).textContent).toMatch(
      /^Script API level 2 \(answers sign editors\): /,
    );
  });

  it("says no level while the text has a problem the server finds", async () => {
    renderEditor();

    expect(
      await screen.findByText("Line 2, column 2: `kees` isn't defined", {}, { timeout: 3000 }),
    );
    expect(screen.queryByTestId("script-level")).toBeNull();
  });

  it("shows the server's problem at once, until the text changes", async () => {
    const problem = { path: "script.source", message: "refused at save", line: 3, column: 2 };
    const { view } = renderEditor({ serverProblem: problem }, async () => ({
      level: 1,
      needs: [],
    }));

    await waitFor(() => expect(marks(view())).toContainEqual(["keys", "refused at save"]));
    view().dispatch({ changes: { from: 0, insert: " " } });
    await waitFor(
      () => expect(marks(view()).map(([, message]) => message)).not.toContain("refused at save"),
      {
        timeout: 3000,
      },
    );
  });

  it("hands every change to onChange, and takes a new value from outside", () => {
    const onChange = vi.fn();
    const { view, rerender, checkScript } = renderEditor({ onChange });

    view().dispatch({ changes: { from: 0, insert: "// x\n" } });
    expect(onChange).toHaveBeenLastCalledWith(`// x\n${SCRIPT}`);

    rerender(
      <ApiClientProvider client={new FakeApiClient({ checkScript })}>
        <ScriptEditor label="Script" value="fn on_stop() { }" onChange={onChange} />
      </ApiClientProvider>,
    );
    expect(view().state.doc.toString()).toBe("fn on_stop() { }");
  });

  it("types over a selection itself, so the browser never restyles the text it replaces", () => {
    const { root, view } = renderEditor();
    const content = root.querySelector(".cm-content") as HTMLElement;
    view().dispatch({ selection: { anchor: 0, head: view().state.doc.length } });

    const typed = new InputEvent("beforeinput", {
      inputType: "insertText",
      data: "(",
      bubbles: true,
      cancelable: true,
    });
    content.dispatchEvent(typed);

    expect(typed.defaultPrevented).toBe(true);
    // Through CodeMirror's own input handlers: closing brackets wrap the selection.
    expect(view().state.doc.toString()).toBe(`(${SCRIPT})`);
  });

  it("leaves typing with a plain cursor to the browser, and refuses any into a read-only text", () => {
    const { root, view } = renderEditor({ readOnly: true });
    const content = root.querySelector(".cm-content") as HTMLElement;
    const event = () =>
      new InputEvent("beforeinput", {
        inputType: "insertText",
        data: "x",
        bubbles: true,
        cancelable: true,
      });

    const refused = event();
    content.dispatchEvent(refused);
    expect(refused.defaultPrevented).toBe(true);
    expect(view().state.doc.toString()).toBe(SCRIPT);

    view().dispatch({ effects: [] });
    const { root: other } = renderEditor();
    const plain = event();
    (other.querySelector(".cm-content") as HTMLElement).dispatchEvent(plain);
    expect(plain.defaultPrevented).toBe(false);
  });

  it("keeps a read-only text from the keyboard", () => {
    const { view } = renderEditor({ readOnly: true });

    expect(view().state.readOnly).toBe(true);
  });

  it("moves the cursor to a place it's asked to show, and to a listed mark", async () => {
    const { view } = renderEditor({ focusAt: { line: 3, column: 7 } });

    expect(view().state.selection.main.head).toBe(SCRIPT.indexOf("jump.tapp"));
    await userEvent.click(
      await screen.findByRole(
        "button",
        { name: "Line 2, column 2: `kees` isn't defined" },
        { timeout: 3000 },
      ),
    );
    expect(view().state.selection.main.head).toBe(SCRIPT.indexOf("kees"));
  });
});
