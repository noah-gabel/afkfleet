// The mode editor as one story (Plan.md P11.9; ADR-0020, U62–U75, S82–S84): the Owner creates a
// mode with steps in two sections, drags one, saves, adds a command off the allowlist through the
// step-up, meets each 409, keeps edits as a copy, and deletes after the in-use refusal. A Member
// sees a shared mode locked, its command cut, and copies it. Every spec here runs under the
// release CSP, so the drag proves its library adds no runtime style.

import { MEMBER, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

const SAVED = "Saved. Bots using this mode switch to the change at once.";

test("the Owner creates a mode, drags a step, and meets each conflict", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");
  const section = (name: string) => main.getByRole("group", { name, exact: true });
  const step = (name: string, n: number) =>
    section(name).getByRole("group", { name: `Step ${n}`, exact: true });
  const save = main.getByRole("button", { name: "Save", exact: true });

  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await main.getByRole("link", { name: "New mode" }).click();
  await main.getByLabel("Name").fill("Fishing");

  const joining = "When it joins";
  await section(joining).getByRole("button", { name: "Add step" }).click();
  await step(joining, 1).getByLabel("What it does").selectOption({ label: "Select a hotbar slot" });
  await step(joining, 1).getByLabel("Slot").selectOption("3");

  const online = "While it's online";
  await section(online).getByRole("button", { name: "Add step" }).click();
  await step(online, 1).getByLabel("Every", { exact: true }).fill("5");
  await step(online, 1).getByLabel("Plus up to, at random", { exact: true }).fill("0");
  await section(online).getByRole("button", { name: "Add step" }).click();
  await step(online, 2).getByLabel("What it does").selectOption({ label: "Swing arm" });
  await step(online, 2).getByLabel("Every", { exact: true }).fill("2");
  await step(online, 2).getByLabel("Every: unit").selectOption("minutes");
  await step(online, 2).getByLabel("Plus up to, at random", { exact: true }).fill("0");

  // A real drag: the second step's handle, held, then dropped above the first.
  const handle = step(online, 2).getByRole("button", { name: "Drag step 2" });
  const from = await handle.boundingBox();
  const above = await step(online, 1).boundingBox();
  if (from === null || above === null) {
    throw new Error("the step isn't on the screen");
  }
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(from.x + from.width / 2, above.y - 10, { steps: 12 });
  await page.mouse.up();
  await expect(step(online, 1).getByLabel("What it does")).toHaveValue("swing_arm");
  await expect(step(online, 2).getByLabel("What it does")).toHaveValue("jump");

  await save.click();
  await expect(page.getByText("Created Fishing.")).toBeVisible();
  await expect(main.getByRole("heading", { name: "Fishing", level: 1 })).toBeVisible();
  await expect(step(online, 1).getByLabel("What it does")).toHaveValue("swing_arm");
  await expect(step(online, 1).getByLabel("Every", { exact: true })).toHaveValue("2");
  await expect(step(online, 1).getByLabel("Every: unit")).toHaveValue("minutes");
  await expect(step(joining, 1).getByLabel("Slot")).toHaveValue("3");
  await expect(save).toBeDisabled();

  // The server's refusal at its field, in its words.
  await step(online, 1).getByLabel("Chance (%)").fill("0");
  await save.click();
  await expect(step(online, 1).getByText("must be from 1 to 100 percent")).toBeVisible();
  await step(online, 1).getByLabel("Chance (%)").fill("100");
  await expect(save).toBeDisabled();

  // A command off the allowlist, through the step-up.
  await section(joining).getByRole("button", { name: "Add step" }).click();
  await step(joining, 2)
    .getByLabel("What it does")
    .selectOption({ label: "Send chat or a command" });
  await step(joining, 2).getByLabel("Message").fill("/warp home");
  await save.click();
  await expect(
    page.getByText(
      "Enter your password to save a command off the allowlist. It counts for 5 minutes.",
    ),
  ).toBeVisible();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText(SAVED)).toBeVisible();
  await expect(main.getByText("Sends a command off the allowlist:")).toBeVisible();

  // Another device changed it meanwhile: Save's 409, then the newest version.
  backend.changeMode("Fishing", { name: "Elsewhere" });
  await main.getByLabel("Name").fill("Mine");
  await save.click();
  await expect(
    main.getByText("This mode changed since you opened it. Load it again to see the change."),
  ).toBeVisible();
  await main.getByRole("button", { name: "Load again" }).click();
  await expect(main.getByLabel("Name")).toHaveValue("Elsewhere");
  await expect(save).toBeDisabled();

  // Another user's bot runs it: the edits stay, and go on as a copy.
  backend.usedByOthers("Elsewhere");
  await main.getByLabel("Name").fill("Mine");
  await save.click();
  await expect(
    main.getByText("A bot you can't set this mode on uses it, so this change can't be saved."),
  ).toBeVisible();
  await expect(
    main.getByText("Someone who can control that bot has to switch it to another mode first."),
  ).toBeVisible();
  await main.getByRole("button", { name: "Save as copy" }).click();
  await expect(page.getByText("Saved a copy as Mine.")).toBeVisible();
  await expect(main.getByRole("heading", { name: "Mine", level: 1 })).toBeVisible();

  // Delete: AfkBot1 runs the copy, so it's refused, and says which bot.
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Home" }).click();
  await main.getByRole("link", { name: /AfkBot1/ }).click();
  await main.getByLabel("Mode", { exact: true }).selectOption({ label: "Mine" });
  await expect(main.getByLabel("Mode", { exact: true })).toBeEnabled();
  await main.getByRole("link", { name: "Modes", exact: true }).click();
  await main.getByRole("link", { name: "Open Mine" }).click();
  await main.getByRole("button", { name: "Delete", exact: true }).click();
  await main.getByRole("button", { name: "Delete mode" }).click();
  await expect(
    main.getByText(
      "A bot uses this mode, so it can't be deleted. Switch every bot to another mode first.",
    ),
  ).toBeVisible();
  await expect(main.getByText("These of the bots you can see run it:")).toBeVisible();
  await main.getByRole("link", { name: "AfkBot1" }).click();
  await main.getByLabel("Mode", { exact: true }).selectOption({ label: "afk (built in)" });
  await expect(main.getByLabel("Mode", { exact: true })).toBeEnabled();
  await main.getByRole("link", { name: "Modes", exact: true }).click();
  await main.getByRole("link", { name: "Open Mine" }).click();
  await main.getByRole("button", { name: "Delete", exact: true }).click();
  await main.getByRole("button", { name: "Delete mode" }).click();
  await expect(page.getByText("Deleted Mine.")).toBeVisible();
  await expect(main.getByRole("link", { name: "Open Mine" })).toHaveCount(0);
});

test("a Member sees a shared mode locked, its command cut, and copies it", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  const main = page.getByRole("main");
  const joining = main.getByRole("group", { name: "When it joins", exact: true });
  const first = joining.getByRole("group", { name: "Step 1", exact: true });

  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await main.getByRole("link", { name: "Open warp" }).click();
  await expect(main.getByRole("heading", { name: "warp", level: 1 })).toBeVisible();
  await expect(main.getByRole("button", { name: "Save", exact: true })).toHaveCount(0);
  await expect(first.getByLabel("Message")).toHaveValue("/warp");
  await expect(first.getByLabel("Message")).toHaveAttribute("readonly", "");
  await expect(
    first.getByText("Its arguments are hidden: only someone who can edit this mode sees them."),
  ).toBeVisible();

  await main.getByRole("button", { name: "Make a copy" }).click();
  await expect(main.getByRole("heading", { name: "New mode", level: 1 })).toBeVisible();
  await expect(main.getByLabel("Name")).toHaveValue("warp copy");
  await expect(
    first.getByText("Add its arguments: the original's are hidden from you."),
  ).toBeVisible();
  await first.getByLabel("Message").fill("/spawn");
  await expect(first.getByText("Add its arguments:")).toHaveCount(0);
  await main.getByRole("button", { name: "Save", exact: true }).click();

  await expect(page.getByText("Saved a copy as warp copy.")).toBeVisible();
  await expect(main.getByRole("heading", { name: "warp copy", level: 1 })).toBeVisible();
  await expect(main.getByText("Yours · Private · 1 step")).toBeVisible();
});

test("a step moves with the keyboard on its handle, within its section", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");
  const joining = main.getByRole("group", { name: "When it joins", exact: true });
  const add = joining.getByRole("button", { name: "Add step" });
  const step = (n: number) => joining.getByRole("group", { name: `Step ${n}`, exact: true });

  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await main.getByRole("link", { name: "New mode" }).click();
  await add.click();
  await add.click();
  await step(1).getByLabel("What it does").selectOption({ label: "Sneak" });

  await step(1).getByRole("button", { name: "Drag step 1" }).focus();
  await page.keyboard.press("Space");
  await expect(page.getByText("Picked up step 1.")).toBeAttached();
  // The sensor starts listening for arrows just after the pick-up, a moment no person meets.
  await expect(async () => {
    await page.keyboard.press("ArrowDown");
    await expect(page.getByText("Step 1 moved to position 2 of 2.")).toBeAttached({ timeout: 300 });
  }).toPass({ timeout: 5_000 });
  await page.keyboard.press("Space");

  await expect(step(1).getByLabel("What it does")).toHaveValue("jump");
  await expect(step(2).getByLabel("What it does")).toHaveValue("sneak");
});
