// Every screen and state in both themes (Plan.md P11.10, P11.11; ADR-0020, S96, S107, S108), under
// the release CSP: a row's text holds WCAG AA against what's behind it, in the theme's
// `color-scheme`, and a Tab through it holds each focus stop's outline at 3:1. There is no list of
// known failures (S97). No screenshots: L4 adds them (U97). The not-found screen isn't here: the
// app's router lives in memory, so no click reaches it, and it holds one text and one link in the
// app's default colours.

import type { Page } from "@playwright/test";

import { type FakeBackend, MEMBER, NEW_USER, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";
import {
  AA,
  bothThemes,
  focusProblems,
  inScheme,
  measured,
  selectsInTokens,
  textsInBothThemes,
  walked,
} from "./theme";

/** Signed in as `username`, on Home. */
async function signIn(page: Page, backend: FakeBackend, username: string): Promise<void> {
  await page.goto("/");
  await logIn(page, username);
  await enterCode(page, backend.code(username));
  await expectHome(page, username);
}

/** Opens `name` in the app's nav. */
async function nav(page: Page, name: string): Promise<void> {
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name }).click();
}

test("the check itself reports what it should, at the numbers it should", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  const plant = (color: string, background: string) =>
    page.evaluate(
      ([text, back]) => {
        document.querySelector("[data-planted]")?.remove();
        const planted = document.createElement("div");
        planted.dataset.planted = "yes";
        planted.textContent = "Planted";
        planted.style.color = text ?? "";
        planted.style.background = back ?? "";
        document.body.append(planted);
      },
      [color, background],
    );
  const planted = async (floor: number) =>
    (await walked(page, { normal: floor, large: floor })).low.find((one) => one.text === "Planted");

  await plant("#111", "#000");
  expect(await planted(3)).toMatchObject({ text: "Planted" });

  await plant("#000", "#fff");
  expect(await planted(3)).toBeUndefined();
  expect((await planted(22))?.ratio).toBe(21);

  await plant("#777", "#fff");
  expect((await planted(5))?.ratio).toBeCloseTo(4.48, 1);

  // A semi-transparent background counts through to what's behind it.
  await page.evaluate(() => {
    document.body.style.background = "#000";
  });
  await plant("#555", "rgb(255 255 255 / 10%)");
  expect((await planted(3))?.ratio).toBeCloseTo(2.33, 1);

  // Text that's disabled or only for screen readers isn't what the floor is about.
  await page.evaluate(() => {
    const holder = document.querySelector("[data-planted]") as HTMLElement;
    holder.setAttribute("aria-disabled", "true");
  });
  expect(await planted(22)).toBeUndefined();
  await plant("#000", "#fff");
  expect((await planted(22))?.ratio).toBe(21);
  await page.evaluate(() => {
    const holder = document.querySelector("[data-planted]") as HTMLElement;
    holder.style.position = "absolute";
    holder.style.width = "1px";
    holder.style.height = "1px";
    holder.style.overflow = "hidden";
    holder.style.clipPath = "inset(50%)";
  });
  expect(await planted(22)).toBeUndefined();
  await page.evaluate(() => {
    document.querySelector("[data-planted]")?.remove();
    document.body.style.background = "";
  });
});

test("the check holds AA: normal and large text, placeholders, and every form a colour comes in", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  const plant = (color: string, size = "16px", weight = "400") =>
    page.evaluate(
      ([shade, fontSize, fontWeight]) => {
        document.querySelector("[data-planted]")?.remove();
        const planted = document.createElement("div");
        planted.dataset.planted = "yes";
        planted.textContent = "Planted";
        planted.style.color = shade ?? "";
        planted.style.background = "#fff";
        planted.style.fontSize = fontSize ?? "";
        planted.style.fontWeight = fontWeight ?? "";
        document.body.append(planted);
      },
      [color, size, weight],
    );
  const low = async () => (await walked(page, AA)).low.find((one) => one.text === "Planted");

  // Normal text: 4.5:1.
  await plant("#777");
  expect((await low())?.floor).toBe(4.5);
  await plant("#767676");
  expect(await low()).toBeUndefined();

  // Large text: 3:1 from 24 px, or from 18.66 px at weight 700; 600 isn't bold.
  await plant("#888", "23.9px");
  expect(await low()).toBeDefined();
  await plant("#888", "24px");
  expect(await low()).toBeUndefined();
  await plant("#888", "18.66px", "700");
  expect(await low()).toBeUndefined();
  await plant("#888", "18.66px", "600");
  expect(await low()).toBeDefined();
  await plant("#888", "18.65px", "700");
  expect(await low()).toBeDefined();
  await plant("#999", "24px");
  expect(await low()).toBeDefined();

  // Every form a computed colour comes in reads through the canvas.
  for (const color of [
    "oklch(0.145 0 0)",
    "oklab(0.145 0 0)",
    "color(srgb 0 0 0)",
    "color-mix(in srgb, #000 90%, #fff)",
    "rgb(0 0 0 / 90%)",
  ]) {
    await plant(color);
    expect(await low(), color).toBeUndefined();
  }
  for (const color of ["color(srgb 1 1 1)", "color-mix(in srgb, #fff 90%, #000)"]) {
    await plant(color);
    expect(await low(), color).toBeDefined();
  }

  // A placeholder counts while the field is empty, and a disabled field's doesn't.
  await page.evaluate(() => {
    document.querySelector("[data-planted]")?.remove();
    const input = document.createElement("input");
    input.dataset.planted = "yes";
    input.placeholder = "Planted";
    input.style.background = "#fff";
    input.style.color = "#000";
    document.body.append(input);
  });
  expect((await low())?.floor).toBe(4.5);
  await page.evaluate(() => {
    (document.querySelector("[data-planted]") as HTMLInputElement).disabled = true;
  });
  expect(await low()).toBeUndefined();
  await page.evaluate(() => document.querySelector("[data-planted]")?.remove());
});

test("the check reads a marked control's focus from its cells, not an outline", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();

  /** What a Tab finds at six planted cells, healthy unless `options` says otherwise. */
  const atPlanted = async (options: {
    control?: "input" | "button";
    marked?: boolean;
    cells?: number;
    strong?: readonly number[];
    focused?: string;
    keepsFocusLook?: boolean;
    outline?: string;
    background?: string;
    box?: string;
    hidden?: boolean;
  }) => {
    await page.evaluate(
      ({
        control = "input",
        marked = true,
        cells = 6,
        strong = [0],
        focused = "var(--ring)",
        keepsFocusLook = false,
        outline = "none",
        background = "transparent",
        box = "var(--card)",
        hidden = false,
      }) => {
        document.querySelector("[data-planted]")?.remove();
        const holder = document.createElement("div");
        holder.dataset.planted = "yes";
        holder.dataset.slot = "code-boxes";
        Object.assign(holder.style, {
          position: "fixed",
          top: "0",
          left: "0",
          width: "400px",
          height: "48px",
          padding: "8px",
          display: "flex",
          gap: "8px",
          background: box,
        });
        const looks = [...Array(cells).keys()].map((index) => {
          const cell = document.createElement("div");
          cell.dataset.slot = "code-cell";
          Object.assign(cell.style, {
            width: "40px",
            boxSizing: "border-box",
            border: "1px solid var(--input)",
          });
          if (hidden && index === 1) {
            cell.style.display = "none";
          }
          holder.append(cell);
          return { cell, index };
        });
        const field = document.createElement(control);
        field.setAttribute("aria-label", "Planted");
        if (marked) {
          field.dataset.focusIndicator = "cells";
        }
        Object.assign(field.style, {
          position: "absolute",
          inset: "0",
          background,
          outline,
          color: "var(--foreground)",
        });
        const show = () => {
          for (const { cell, index } of looks) {
            cell.style.borderColor = focused;
            cell.style.borderWidth = strong.includes(index) ? "2px" : "1px";
          }
        };
        const hide = () => {
          for (const { cell } of looks) {
            cell.style.borderColor = "var(--input)";
            cell.style.borderWidth = "1px";
          }
        };
        field.addEventListener("focus", show);
        field.addEventListener("blur", keepsFocusLook ? show : hide);
        holder.append(field);
        document.body.append(holder);
      },
      options,
    );
    return (await focusProblems(page)).filter((problem) => problem.includes("Planted"));
  };
  const fails = (text: string) => [expect.stringContaining(text)];

  expect(await atPlanted({})).toEqual([]);
  // `--input` holds 3:1 on a card, so a plain ratio would pass this.
  expect(await atPlanted({ focused: "var(--input)" })).toEqual(fails("isn't on --ring"));
  expect(await atPlanted({ strong: [] })).toEqual(fails("0 stronger cells"));
  expect(await atPlanted({ strong: [0, 1] })).toEqual(fails("2 stronger cells"));
  expect(await atPlanted({ cells: 0 })).toEqual(fails("0 cells"));
  expect(await atPlanted({ cells: 5 })).toEqual(fails("5 cells"));
  expect(await atPlanted({ outline: "2px solid var(--ring)" })).toEqual(fails("an outline"));
  expect(await atPlanted({ outline: "auto" })).toEqual(fails("an outline"));
  expect(await atPlanted({ control: "button", cells: 0 })).toEqual(fails("0 cells"));
  expect(await atPlanted({ marked: false, outline: "" })).toEqual(fails("no marker"));
  expect(await atPlanted({ keepsFocusLook: true })).toEqual(fails("the same without the focus"));
  expect(await atPlanted({ background: "#fff" })).toEqual(fails("hides its cells"));
  expect(await atPlanted({ hidden: true })).toEqual(fails("isn't shown"));
  expect(await atPlanted({ box: "var(--ring)" })).toEqual(fails("at 1:1"));
  await page.evaluate(() => document.querySelector("[data-planted]")?.remove());
});

test("the check holds each focus stop's outline, and walks the whole page", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  const removePlants = () =>
    page.evaluate(() => {
      for (const planted of document.querySelectorAll("[data-planted]")) {
        planted.remove();
      }
    });
  /** One planted button on a white box, styled by `style`, and what a Tab finds at it. */
  const atPlanted = async (style: Record<string, string>) => {
    await removePlants();
    await page.evaluate((styles) => {
      const box = document.createElement("div");
      box.dataset.planted = "yes";
      Object.assign(box.style, {
        position: "fixed",
        top: "0",
        left: "0",
        padding: "16px",
        background: "#fff",
      });
      const button = document.createElement("button");
      button.textContent = "Planted";
      button.style.color = "#000";
      button.style.background = "#fff";
      button.style.outlineOffset = "2px";
      Object.assign(button.style, styles);
      box.append(button);
      document.body.append(box);
    }, style);
    return (await focusProblems(page)).filter((problem) => problem.includes("Planted"));
  };

  expect(await atPlanted({ outline: "2px solid #000" })).toEqual([]);
  // A shadow or a border is no indicator, however strong.
  expect(await atPlanted({ outline: "none", boxShadow: "0 0 0 3px #000" })).toEqual([
    expect.stringContaining("outline none"),
  ]);
  expect(await atPlanted({ outlineStyle: "auto" })).toEqual([
    expect.stringContaining("outline auto"),
  ]);
  expect(await atPlanted({ outline: "0 solid #000" })).toEqual([expect.stringContaining("0px")]);
  // Too faint: transparent, by alpha, or by the element's opacity.
  expect(await atPlanted({ outline: "2px solid transparent" })).toEqual([
    expect.stringContaining("at 1:1"),
  ]);
  expect(await atPlanted({ outline: "2px solid rgb(0 0 0 / 50%)" })).toEqual([]);
  expect(await atPlanted({ outline: "2px solid rgb(0 0 0 / 30%)" })).toEqual([
    expect.stringContaining(":1 against"),
  ]);
  expect(await atPlanted({ outline: "2px solid #000", opacity: "0.3" })).toEqual([
    expect.stringContaining(":1 against"),
  ]);
  // An inset outline lies on the element's own fill.
  expect(
    await atPlanted({ outline: "2px solid #fff", outlineOffset: "-4px", background: "#fff" }),
  ).toEqual([expect.stringContaining("at 1:1")]);
  expect(
    await atPlanted({ outline: "2px solid #fff", outlineOffset: "-4px", background: "#000" }),
  ).toEqual([]);

  // A clipped outline fails, whatever lies beyond the clip: hit-testing can't see the cut.
  await removePlants();
  await page.evaluate(() => {
    const box = document.createElement("div");
    box.dataset.planted = "yes";
    Object.assign(box.style, {
      position: "fixed",
      top: "16px",
      left: "16px",
      overflow: "hidden",
      background: "#fff",
    });
    const button = document.createElement("button");
    button.textContent = "Planted";
    button.style.color = "#000";
    button.style.background = "#fff";
    button.style.outline = "2px solid #000";
    button.style.outlineOffset = "2px";
    box.append(button);
    document.body.append(box);
  });
  expect((await focusProblems(page)).filter((problem) => problem.includes("Planted"))).toEqual([
    expect.stringContaining("cut off"),
  ]);

  // A stop mid-transition reads as it ends.
  await removePlants();
  await page.evaluate(() => {
    const button = document.createElement("button");
    button.dataset.planted = "yes";
    button.textContent = "Planted";
    button.style.position = "fixed";
    button.style.top = "16px";
    button.style.left = "16px";
    button.style.outline = "2px solid transparent";
    button.style.transition = "outline-color 10s";
    button.addEventListener("focus", () => {
      button.style.outlineColor = "#000";
    });
    document.body.append(button);
  });
  expect((await focusProblems(page)).filter((problem) => problem.includes("Planted"))).toEqual([]);
  await removePlants();

  // A second repeat is a trap.
  await page.evaluate(() => {
    const [one, two] = ["Planted one", "Planted two"].map((name) => {
      const button = document.createElement("button");
      button.dataset.planted = "yes";
      button.textContent = name;
      document.body.append(button);
      return button;
    }) as [HTMLButtonElement, HTMLButtonElement];
    const jump = (from: HTMLButtonElement, to: HTMLButtonElement) =>
      from.addEventListener("keydown", (event) => {
        if (event.key === "Tab") {
          event.preventDefault();
          to.focus();
        }
      });
    jump(one, two);
    jump(two, one);
  });
  expect(await focusProblems(page)).toEqual(
    expect.arrayContaining([expect.stringContaining("trapped")]),
  );
  await removePlants();

  // A page that keeps adding stops hits the cap.
  await page.evaluate(() => {
    const add = () => {
      const button = document.createElement("button");
      button.dataset.planted = "yes";
      button.textContent = "Planted more";
      button.addEventListener("focus", add, { once: true });
      document.body.append(button);
    };
    add();
  });
  expect(await focusProblems(page)).toEqual(
    expect.arrayContaining([expect.stringContaining("its cap")]),
  );
  await removePlants();

  // Controls Tab can't reach fail, and a page with no controls is fine.
  await page.evaluate(() => {
    document.getElementById("root")?.setAttribute("inert", "");
  });
  expect(await focusProblems(page)).toEqual(
    expect.arrayContaining([expect.stringContaining("never reached")]),
  );
  await page.evaluate(() => {
    const root = document.getElementById("root") as HTMLElement;
    root.removeAttribute("inert");
    root.style.display = "none";
  });
  expect(await focusProblems(page)).toEqual([]);
  await page.evaluate(() => {
    (document.getElementById("root") as HTMLElement).style.display = "";
  });

  // A Tab that changes the row fails, and focus goes back.
  await page.evaluate(() => {
    const button = document.createElement("button");
    button.dataset.planted = "yes";
    button.textContent = "Planted alert";
    button.addEventListener("focus", () => {
      const alert = document.createElement("div");
      alert.dataset.planted = "yes";
      alert.setAttribute("role", "alert");
      document.body.append(alert);
    });
    document.body.append(button);
    document.querySelector<HTMLElement>("h1")?.focus();
  });
  expect(await focusProblems(page)).toEqual(
    expect.arrayContaining([expect.stringContaining("changed the row")]),
  );
  expect(await page.evaluate(() => document.activeElement?.tagName)).toBe("H1");
  await removePlants();
});

test("every native select holds the theme's tokens, its options too", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "view");
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");
  const check = async (row: string) => {
    for (const scheme of ["dark", "light"] as const) {
      expect(await selectsInTokens(page, row, scheme), row).toBeGreaterThan(0);
    }
  };

  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(main.getByLabel("Mode", { exact: true })).toBeVisible();
  await check("a bot's mode");

  await nav(page, "Accounts");
  await main.getByRole("link", { name: "AfkBot1", exact: true }).click();
  await expect(main.getByLabel(`Access of ${MEMBER}`)).toBeVisible();
  await check("who has access");

  await nav(page, "Modes");
  await main.getByRole("link", { name: "New mode" }).click();
  await main.getByLabel("Name").fill("Fishing");
  const joining = main.getByRole("group", { name: "When it joins", exact: true });
  await joining.getByRole("button", { name: "Add step" }).click();
  await joining
    .getByRole("group", { name: "Step 1", exact: true })
    .getByLabel("What it does")
    .selectOption({ label: "Select a hotbar slot" });
  const online = main.getByRole("group", { name: "While it's online", exact: true });
  await online.getByRole("button", { name: "Add step" }).click();
  await online.getByLabel("Every", { exact: true }).fill("5");
  await online.getByLabel("Plus up to, at random", { exact: true }).fill("0");
  // A saved mode is the one that offers who can see it.
  await main.getByRole("button", { name: "Save", exact: true }).click();
  await expect(main.getByRole("heading", { name: "Fishing", level: 1 })).toBeVisible();
  await expect(main.getByLabel("Who can see it")).toBeVisible();
  await expect(joining.getByLabel("Slot")).toBeVisible();
  await expect(online.getByLabel("Every: unit")).toBeVisible();
  await check("the mode editor: visibility, what it does, a step's choice, a time's unit");
});

test("before signing in", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await bothThemes(page, "login");
  await logIn(page, OWNER, "not the placeholder");
  await expect(page.getByRole("alert").first()).toBeVisible();
  await bothThemes(page, "login, refused");

  await logIn(page, OWNER);
  await enterCode(page, "000000");
  await expect(page.getByText("Wrong code. 2 tries left.")).toBeVisible();
  await bothThemes(page, "the code step, refused");
  // The digits themselves, so their colour is measured; a fifth doesn't send.
  await page.getByLabel("Code").pressSequentially("12345");
  await bothThemes(page, "the code step, digits in the boxes");
  await page.getByRole("button", { name: "Use a recovery code" }).click();
  await expect(page.getByLabel("Recovery code")).toBeVisible();
  await bothThemes(page, "the code step, a recovery code");
  await page.reload();

  await page.getByRole("link", { name: "Server settings" }).click();
  await expect(page.getByLabel(/address/i).first()).toBeVisible();
  await bothThemes(page, "server settings");
  await page.reload();

  await page.getByRole("link", { name: "Register with an invite" }).click();
  await page.getByRole("button", { name: "Register" }).click();
  await expect(page.locator("[aria-invalid=true]").first()).toBeVisible();
  await bothThemes(page, "register, refused");
  await page.reload();

  await logIn(page, NEW_USER);
  const qr = page.getByRole("img", { name: "QR code for your authenticator app" });
  await expect(qr).toBeVisible();
  await bothThemes(page, "2FA setup");
  for (const scheme of ["dark", "light"] as const) {
    await inScheme(page, scheme);
    await expect(qr).toHaveCSS("background-color", "rgb(255, 255, 255)");
  }
  await enterCode(page, backend.setupCode());
  await expect(page.getByRole("list", { name: "Recovery codes" })).toBeVisible();
  await bothThemes(page, "recovery codes");
});

test("Home, its bots in every status, and a bot's screen", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  backend.setBot("AfkBot2", {
    desiredState: "running",
    status: { kind: "reported", state: "paused", reason: "duplicate_login" },
  });
  backend.setBot("AfkBot3", {
    desiredState: "running",
    status: { kind: "reported", state: "failed" },
  });
  backend.setBot("AfkBot4", { desiredState: "running", status: { kind: "unknown" } });
  backend.setBot("AfkBot5", { status: { kind: "unplaced", why: "no_server" } });
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");

  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  await bothThemes(page, "Home, every status");

  const id = backend.bot("AfkBot1").id;
  backend.addChat("AfkBot1", {
    at: new Date().toISOString(),
    direction: "in",
    kind: "system",
    text: "Hello from the server",
    truncated: false,
    arguments_hidden: false,
    mine: false,
  });
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const chat = main.getByRole("region", { name: "Chat" });
  await expect(chat.getByText("Hello from the server")).toBeVisible();
  await expect(page.getByRole("button", { name: "Back", exact: true })).toBeVisible();
  await bothThemes(page, "a bot, its chat, with Back");

  await push(page, {
    update: "event",
    event: { type: "bot_died", bot_id: id, at: new Date().toISOString() },
  });
  await expect(main.getByRole("region", { name: "Timeline" }).getByText("Died")).toBeVisible();
  const toast = page.getByText("AfkBot1 died.");
  await toast.hover();
  await bothThemes(page, "a bot, its timeline and a toast");
  await expect(toast).toBeVisible();

  await page.getByRole("link", { name: "Home", exact: true }).first().click();
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();

  await push(page, {
    update: "event",
    event: {
      type: "bot_alert",
      bot_id: backend.bot("AfkBot3").id,
      at: new Date().toISOString(),
      state: "failed",
      reason: "crash_loop",
    },
  });
  const failed = page.locator("[data-sonner-toast][data-type=error]");
  await failed.hover();
  await bothThemes(page, "Home, its notice and an error toast");
  await expect(failed).toBeVisible();
});

test("a bot's console in every look, and its screen stacked and loading", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  const at = new Date().toISOString();
  const line = {
    at,
    direction: "in",
    truncated: false,
    arguments_hidden: false,
    mine: false,
  } as const;
  backend.addChat("AfkBot1", { ...line, kind: "system", text: "The server said something" });
  backend.addChat("AfkBot1", { ...line, kind: "chat", text: "hi", sender: { name: "Steve" } });
  backend.addChat("AfkBot1", {
    ...line,
    kind: "system",
    text: "[x] Notch : hi",
    parsed_sender: "Notch",
  });
  backend.addChat("AfkBot1", { ...line, direction: "out", kind: "mode", text: "/spawn" });
  backend.addChat("AfkBot1", { ...line, direction: "out", kind: "user", text: "mine", mine: true });
  backend.addChat("AfkBot1", {
    ...line,
    direction: "out",
    kind: "user",
    text: "/login",
    arguments_hidden: true,
    truncated: true,
  });
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  await signIn(page, backend, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const chat = page.getByRole("main").getByRole("region", { name: "Chat" });
  await expect(chat.getByText("The server said something")).toBeVisible();
  await bothThemes(page, "a bot's console, every sender's look");

  backend.chatOutcome = "pending";
  await chat.getByLabel("Message or /command").fill("Still on its way");
  await chat.getByRole("button", { name: "Send" }).click();
  await expect(chat.getByLabel("Message or /command")).toHaveValue("");
  await bothThemes(page, "a bot's console, a pending mark");

  await page.setViewportSize({ width: 800, height: 600 });
  await expect(chat.getByText("The server said something")).toBeAttached();
  await bothThemes(page, "a bot's screen, stacked");
  await page.setViewportSize({ width: 1280, height: 720 });

  // A bot this sign-in hasn't read yet, its read held.
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Home" }).click();
  backend.hold("bot");
  await page.getByRole("link", { name: "Open AfkBot2" }).click();
  await expect(page.getByText("Loading bot…")).toBeVisible();
  await bothThemes(page, "a bot, loading");
  backend.release();
  await expect(page.getByRole("heading", { name: "AfkBot2", level: 1 })).toBeVisible();
});

test("a bot waiting for its agent, as the Owner and as a Member", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "control");
  backend.setBot("AfkBot1", {
    desiredState: "stopped",
    status: { kind: "unknown", state: "online", agent_last_seen_at: new Date().toISOString() },
    agent: { id: "01900000-0000-7000-8000-000000000301", name: "agent-main", status: "offline" },
  });
  await signIn(page, backend, OWNER);
  await expect(page.getByText("Tied until it returns or is released.")).toBeVisible();
  await bothThemes(page, "Home, a bot waiting for its agent");

  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const main = page.getByRole("main");
  await expect(main.getByText("Waiting for agent")).toBeVisible();
  await expect(main.getByRole("link", { name: "Agents" })).toBeVisible();
  await bothThemes(page, "a bot waiting for its agent, with the way to the agents");

  await main.getByRole("button", { name: "Start" }).click();
  await expect(main.getByText("Saved. Agent unreachable, so nothing changes yet.")).toBeVisible();
  await bothThemes(page, "a bot waiting for its agent, saved");

  backend.setBot("AfkBot1", {
    agent: { id: "01900000-0000-7000-8000-000000000301", name: "agent-main", status: "disabled" },
  });
  await page.reload();
  await expect(page.getByText("Agent disabled.")).toBeVisible();
  await bothThemes(page, "Home, a bot waiting for a release");

  await page.getByRole("button", { name: "Log out" }).click();
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  await expect(page.getByText("Tied until it returns or an admin releases it.")).toBeVisible();
  await bothThemes(page, "Home, a bot waiting, as a Member");
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(main.getByRole("link", { name: "Agents" })).toHaveCount(0);
  await bothThemes(page, "a bot waiting, as a Member");
});

test("an account's screen: a row's question, the step-up, and below Manage", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "view");
  backend.addAccount(OWNER, "reauth_required");
  backend.addAccount(MEMBER, "pending");
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");
  await nav(page, "Accounts");
  await expect(main.getByRole("link", { name: "AfkBot1", exact: true })).toBeVisible();
  await expect(main.getByText(/no longer accepts/)).toBeVisible();
  await bothThemes(page, "accounts, a re-auth prompt and a link waiting");
  await main.getByRole("button", { name: "Approve AfkBot3" }).click();
  await expect(main.getByText(/Approve AfkBot3, linked by/)).toBeVisible();
  await bothThemes(page, "accounts, a review's question");

  await main.getByRole("link", { name: "AfkBot1", exact: true }).click();
  const table = main.getByRole("table", { name: "Who has access" });
  await table.getByRole("button", { name: `Revoke ${MEMBER}'s access` }).click();
  await expect(table.getByRole("button", { name: "Cancel" })).toBeVisible();
  await bothThemes(page, "an account, a row's question");
  await table.getByRole("button", { name: "Cancel" }).click();
  const shareField = main.getByRole("combobox", { name: "Username" });
  await shareField.pressSequentially(NEW_USER.slice(0, 2));
  await expect(main.getByRole("option", { name: NEW_USER })).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(main.getByRole("option", { name: NEW_USER })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await textsInBothThemes(page, "Share with, its list open and an option active");
  await page.keyboard.press("Escape");
  await shareField.fill("");
  await table.getByRole("button", { name: `Revoke ${MEMBER}'s access` }).click();
  await table.getByRole("button", { name: "Revoke", exact: true }).click();
  await expect(main.getByLabel("Current password")).toBeVisible();
  await bothThemes(page, "an account, a row's step-up");

  await page.getByRole("button", { name: "Log out" }).click();
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  await nav(page, "Accounts");
  await main.getByRole("link", { name: "AfkBot1", exact: true }).click();
  await expect(main.getByRole("button", { name: "Leave", exact: true })).toBeVisible();
  await bothThemes(page, "an account below Manage");
  await main.getByRole("button", { name: "Leave", exact: true }).click();
  await expect(main.getByRole("button", { name: "Leave account" })).toBeVisible();
  await bothThemes(page, "an account below Manage, the question");
});

test("accounts and an account while they load", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");
  backend.hold("accounts");
  await nav(page, "Accounts");
  await expect(main.getByText("Loading accounts…")).toBeVisible();
  await bothThemes(page, "accounts, loading");
  backend.release();
  const link = main.getByRole("link", { name: "AfkBot1", exact: true });
  await expect(link).toBeVisible();
  backend.hold("account");
  await link.click();
  await expect(main.getByText("Loading account…")).toBeVisible();
  await bothThemes(page, "an account, loading");
  backend.release();
});

test("a card's Start, its refusal and its wait", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot2", {
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  await signIn(page, backend, OWNER);
  const card = (name: string) => page.locator("[data-slot=card]").filter({ hasText: name });
  await expect(card("AfkBot1").getByRole("button", { name: "Start AfkBot1" })).toBeVisible();
  await expect(card("AfkBot2").getByRole("button", { name: "Stop AfkBot2" })).toBeVisible();
  await bothThemes(page, "Home, Start and Stop");

  backend.refuseNext("start_bot", 409, "conflict");
  await card("AfkBot1").getByRole("button", { name: "Start AfkBot1" }).click();
  await expect(card("AfkBot1").getByRole("alert")).toBeVisible();
  await bothThemes(page, "Home, a card's refusal");

  backend.refuseNext("start_bot", 429, "rate_limited", 300);
  await card("AfkBot1").getByRole("button", { name: "Start AfkBot1" }).click();
  await expect(
    card("AfkBot1")
      .getByText(/Try again in/)
      .first(),
  ).toBeVisible();
  await textsInBothThemes(page, "Home, a card's wait");
});

test("the bot screen and the empty lists of someone with nothing yet", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await signIn(page, backend, MEMBER);
  const main = page.getByRole("main");

  await bothThemes(page, "Home, empty");
  await nav(page, "Accounts");
  await expect(main.getByRole("heading", { name: "Accounts", level: 1 })).toBeVisible();
  const tile = main.getByRole("link", { name: "Offline account" });
  await expect(tile).toBeVisible();
  await bothThemes(page, "accounts, empty, the tiles");
  await tile.hover();
  await textsInBothThemes(page, "accounts, empty, a tile hovered");
  await main.getByRole("link", { name: "Microsoft account" }).hover();
  await textsInBothThemes(page, "accounts, empty, the main tile hovered");
  await page.mouse.move(0, 0);

  await tile.click();
  await expect(page.getByRole("heading", { name: "Add an offline account" })).toBeVisible();
  await bothThemes(page, "the offline screen");
  await page.getByRole("button", { name: "Add" }).click();
  await expect(page.locator("[aria-invalid=true]").first()).toBeVisible();
  await bothThemes(page, "the offline screen, refused");
  await page.getByRole("link", { name: "Back to accounts" }).click();
  await expect(main.getByRole("link", { name: "Microsoft account" })).toBeVisible();

  await page.getByRole("link", { name: "Microsoft account" }).click();
  await page.getByRole("button", { name: "Get a code" }).click();
  await expect(page.getByText("B7X4-KDLQ", { exact: true })).toBeVisible();
  await bothThemes(page, "a link's code");
  await nav(page, "Profile");
  await expect(page.getByRole("group", { name: "Theme" })).toBeVisible();
  await bothThemes(page, "profile");
});

test("modes: the list, the editor, a conflict, and a locked one", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");
  const save = main.getByRole("button", { name: "Save", exact: true });

  await nav(page, "Modes");
  await expect(main.getByRole("link", { name: "New mode" })).toBeVisible();
  await bothThemes(page, "modes");

  await main.getByRole("link", { name: "New mode" }).click();
  await main.getByLabel("Name").fill("Fishing");
  const joining = main.getByRole("group", { name: "When it joins", exact: true });
  await joining.getByRole("button", { name: "Add step" }).click();
  await bothThemes(page, "the mode editor");
  await joining
    .getByRole("group", { name: "Step 1", exact: true })
    .getByLabel("What it does")
    .selectOption({ label: "Select a hotbar slot" });
  await save.click();
  await expect(main.getByRole("heading", { name: "Fishing", level: 1 })).toBeVisible();

  backend.changeMode("Fishing", { name: "Elsewhere" });
  await main.getByLabel("Name").fill("Mine");
  await save.click();
  await expect(main.getByText(/This mode changed since you opened it/)).toBeVisible();
  await bothThemes(page, "the mode editor, a conflict");

  await page.getByRole("button", { name: "Log out" }).click();
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  await nav(page, "Modes");
  await main.getByRole("link", { name: "Open warp" }).click();
  await expect(main.getByRole("button", { name: "Make a copy" })).toBeVisible();
  await bothThemes(page, "a locked mode, its command cut");
});

test("servers, accounts with their grants, and the Owner's admin screens", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "view");
  const members = backend.addAccount(MEMBER);
  backend.share(members, NEW_USER, "control");
  backend.addAgent("agent-main");
  backend.addAgent("agent-old", { enabled: false, status: "disabled", bots: 2 });
  backend.addAgent("agent-late", { status: "expired" });
  backend.addAuditEntry({
    action: "retention.purge",
    metadata: [
      { key: "since", text: "2026-10-13T07:00:00.000Z" },
      { key: "chat_by_age", int: 12 },
    ],
  });
  backend.addAuditEntry({ action: "agent.connect", outcome: "failure", ip: "192.0.2.10" });
  backend.addUnreadableEntry();
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");

  await nav(page, "Minecraft servers");
  await expect(main.getByText("No server has settings yet.")).toBeVisible();
  await bothThemes(page, "servers, empty");
  await main.getByRole("link", { name: "Add a server" }).click();
  await page.getByLabel("Server address").fill("MC.example.com");
  await main.getByRole("button", { name: "Save" }).click();
  await expect(page.getByLabel("Current password")).toBeVisible();
  await bothThemes(page, "a server's settings, the step-up");
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(main.getByRole("heading", { name: "mc.example.com:25565", level: 1 })).toBeVisible();
  await nav(page, "Minecraft servers");
  await expect(main.getByRole("listitem").filter({ hasText: "mc.example.com" })).toBeVisible();
  await bothThemes(page, "servers");

  await nav(page, "Accounts");
  const accountLink = main.getByRole("link", { name: "AfkBot1", exact: true });
  await expect(accountLink).toBeVisible();
  await bothThemes(page, "accounts");
  await accountLink.click();
  await expect(main.getByRole("table", { name: "Who has access" })).toBeVisible();
  await bothThemes(page, "an account and who has access");

  await nav(page, "Users");
  await expect(main.getByRole("row", { name: new RegExp(MEMBER) })).toBeVisible();
  await bothThemes(page, "users");
  await main
    .getByRole("row", { name: new RegExp(MEMBER) })
    .getByRole("link", { name: /Review grants/ })
    .click();
  await main.getByRole("button", { name: /Show who has access/ }).click();
  await expect(main.getByRole("table", { name: /Who has access to/ })).toBeVisible();
  await bothThemes(page, "the grants review");

  await nav(page, "Invites");
  await expect(main.getByRole("heading", { name: "Invites", level: 1 })).toBeVisible();
  await bothThemes(page, "invites");
  await nav(page, "Agents");
  await expect(main.getByRole("row", { name: /agent-main/ })).toBeVisible();
  await bothThemes(page, "agents");
  await main
    .getByRole("row", { name: /agent-main/ })
    .getByRole("button", { name: "Disable" })
    .click();
  await expect(main.getByText(/Disabling agent-main is final/)).toBeVisible();
  await bothThemes(page, "agents, the question");
  await nav(page, "Audit log");
  await expect(main.getByText(/can't be read/)).toBeVisible();
  await bothThemes(page, "the audit log, an unreadable row");
});

test("the banner, the start screen, loading and the error panel", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await signIn(page, backend, OWNER);
  const banner = page.locator("[data-slot=connection-banner]");

  backend.refuseAll("unreachable");
  await nav(page, "Accounts");
  await expect(banner.getByText(/Server unreachable/)).toBeVisible();
  await bothThemes(page, "the banner, unreachable");
  backend.refuseAll("certificate");
  await nav(page, "Modes");
  await expect(banner.getByRole("button", { name: "Server settings" })).toBeVisible();
  await bothThemes(page, "the banner, certificate");
  backend.refuseAll(null);
  await banner.getByRole("button", { name: "Retry now" }).click();
  await expect(banner.getByRole("button", { name: "Retry now" })).toHaveCount(0);

  await page.setViewportSize({ width: 900, height: 700 });
  const logOut = page
    .getByRole("group", { name: "Account" })
    .getByRole("button", { name: "Log out" });
  await logOut.hover();
  await expect(page.locator("[data-slot=tooltip]")).toBeVisible();
  // A Tab moves the focus, which closes a tip, so this row holds its texts only.
  await textsInBothThemes(page, "the collapsed sidebar, a tooltip open");
  await page.mouse.move(600, 600);
  await expect(page.locator("[data-slot=tooltip]")).toHaveCount(0);
  await bothThemes(page, "the collapsed sidebar");
  await page.setViewportSize({ width: 1280, height: 720 });

  backend.stopWatch();
  await expect(page.getByRole("button", { name: "Reconnect" })).toBeVisible();
  await bothThemes(page, "the live hint, stopped");

  backend.refuseAll("unreachable");
  await page.reload();
  await expect(page.getByText("Waiting for the server…")).toBeVisible();
  await bothThemes(page, "the start screen, waiting");

  backend.refuseAll("unexpected_response");
  await page.reload();
  await expect(page.getByRole("button", { name: "Try again" })).toBeVisible();
  await bothThemes(page, "the start screen, failed");
  backend.refuseAll(null);

  backend.hold("bots");
  await page.reload();
  await expect(
    page.getByRole("group", { name: "Account" }).getByText(OWNER, { exact: true }),
  ).toBeVisible();
  await bothThemes(page, "Home, loading");
  backend.release();
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();

  for (const scheme of ["dark", "light"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/?panel=error");
    await expect(page.getByRole("heading", { name: /App error/ })).toBeVisible();
    await measured(page, "the error panel", scheme);
  }
});
