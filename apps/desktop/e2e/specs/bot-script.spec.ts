// A bot's script on its screen, and the chat's ↑ and ↓ (Plan.md P15.26; ADR-0036, D13; ADR-0045),
// in Edge: the live events bring the script's status, its error and its log; the error's link opens
// its mode's script at that line; a bot that no agent online can run says one must be updated; and
// ↑ recalls the line just sent, ↓ what was typed.
import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";

const SOURCE = "fn on_join(first) {\n    keys.jump.tap();\n    mouse.left.hodl();\n}\n";

test("a stopped script's error links to its line, and its log shows", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  const modeId = backend.addScriptedMode(OWNER, "Jumping", {
    source: SOURCE,
    leash: 8,
    tool_guard: 2,
    block_guard: [],
    messages: [],
    patterns: [],
  });
  backend.setBot("AfkBot1", {
    modeId,
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const script = page.getByRole("main").getByRole("region", { name: "Script" });
  await expect(script.getByText("No word from its script since the app connected.")).toBeVisible();
  const id = backend.bot("AfkBot1").id;

  await push(page, {
    update: "event",
    event: { type: "bot_script_log", bot_id: id, log: { kind: "line", text: "joined" } },
  });
  await push(page, {
    update: "event",
    event: {
      type: "bot_script",
      bot_id: id,
      script: {
        state: "stopped",
        problem: {
          kind: "error",
          hook: "on_join",
          line: 3,
          column: 16,
          message: "Function not found: hodl (input)",
        },
      },
    },
  });

  await expect(script.getByText("Stopped", { exact: true })).toBeVisible();
  await expect(script.getByText("In on_join, line 3, column 16")).toBeVisible();
  await expect(script.getByRole("list", { name: "Log" }).getByRole("listitem")).toHaveText([
    /joined$/,
  ]);
  await script.getByRole("link", { name: "Show in the script" }).click();

  await expect(page.getByRole("main").getByRole("heading", { name: "Jumping" })).toBeVisible();
  const head = await page.evaluate(() => {
    const host = document.querySelector('[data-slot="script-editor"]');
    const content = host?.shadowRoot?.querySelector(".cm-content");
    const selection = (
      host?.shadowRoot as unknown as { getSelection?: () => Selection | null }
    )?.getSelection?.();
    return {
      focused: document.activeElement === host,
      line: content?.querySelector(".cm-activeLine")?.textContent ?? null,
      selected: selection?.anchorNode?.textContent ?? null,
    };
  });
  expect(head.focused).toBe(true);
  expect(head.line).toBe("    mouse.left.hodl();");
});

test("a scripted bot no agent online can run says an agent must be updated", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "unplaced", why: "agent_too_old" },
  });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();

  await expect(page.getByText("Waiting for an updated agent").first()).toBeVisible();
  await expect(
    page.getByText("Its mode is a script, and every agent online runs a version too old for it."),
  ).toBeVisible();
});

test("↑ recalls the line just sent, and ↓ what was typed", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const chat = page.getByRole("main").getByRole("region", { name: "Chat" });
  const field = chat.getByRole("textbox", { name: "Message or /command" });
  await field.fill("/spawn");
  await chat.getByRole("button", { name: "Send" }).click();
  await expect(field).toHaveValue("");

  await field.fill("half");
  await field.press("ArrowUp");
  await expect(field).toHaveValue("/spawn");
  await field.press("ArrowDown");
  await expect(field).toHaveValue("half");
});
