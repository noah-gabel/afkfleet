// "Convert to script" as one story (Plan.md P15.27; ADR-0036, D17; ADR-0046), in Edge under the
// release CSP: the Owner writes a step mode, which offers no minecart step on a new step, converts
// it, reads what the script does differently, goes back to the steps and converts again, and saves
// the script as the mode. The conversion is the app's Rust side's; the fake answers this story's
// steps with that side's own answer (e2e/fake/converted.json, held equal by the desktop crate).

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

test("the Owner converts a step mode to a script and saves it", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");
  const section = (name: string) => main.getByRole("group", { name, exact: true });
  const step = (name: string, n: number) =>
    section(name).getByRole("group", { name: `Step ${n}`, exact: true });
  const editor = page.locator('[data-slot="script-editor"] .cm-content');
  const notice = main.getByRole("region", { name: "Converted to a script" });

  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await main.getByRole("link", { name: "New mode" }).click();
  await main.getByLabel("Name").fill("Swing");

  const joining = "When it joins";
  await section(joining).getByRole("button", { name: "Add step" }).click();
  // The stopgap isn't offered on a new step: it goes in the next release.
  await expect(step(joining, 1).getByRole("option", { name: /minecart/ })).toHaveCount(0);
  await step(joining, 1).getByLabel("What it does").selectOption({ label: "Select a hotbar slot" });
  const online = "While it's online";
  await section(online).getByRole("button", { name: "Add step" }).click();
  await step(online, 1).getByLabel("What it does").selectOption({ label: "Swing arm" });
  await step(online, 1).getByLabel("Every", { exact: true }).fill("20");
  await step(online, 1).getByLabel("Plus up to, at random", { exact: true }).fill("0");

  await main.getByRole("button", { name: "Convert to script" }).click();
  await expect(editor).toContainText("swing(); // while it's online, step 1");
  await expect(notice.getByRole("listitem")).toHaveText([
    "While it's online, step 1: swings only while the crosshair isn't on an entity. A script swings with a left click, which would hit one.",
  ]);
  await expect(section(online)).toHaveCount(0);

  // Back to the steps, as they were, and converted again.
  await notice.getByRole("button", { name: "Keep the steps" }).click();
  await expect(step(online, 1).getByLabel("What it does")).toHaveValue("swing_arm");
  await expect(step(online, 1).getByLabel("Every", { exact: true })).toHaveValue("20");
  await main.getByRole("button", { name: "Convert to script" }).click();
  await expect(editor).toContainText("hotbar.select(1); // when it joins, step 1");

  await main.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Created Swing.")).toBeVisible();
  await expect(main.getByRole("heading", { name: "Swing", level: 1 })).toBeVisible();
  await expect(editor).toContainText('timer.every(20000, "online_1");');
  await expect(notice).toHaveCount(0);
});
