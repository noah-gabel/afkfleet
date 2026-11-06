// Every screen and state in both themes (Plan.md P11.10, P11.11; ADR-0020, S96, S107, S108), under
// the release CSP: a row's text holds WCAG AA against what's behind it, in the theme's
// `color-scheme`, and a Tab through it holds each focus stop's outline at 3:1. There is no list of
// known failures (S97). No screenshots: L4 adds them (U97). The not-found screen isn't here: the
// app's router lives in memory, so no click reaches it, and it holds one text and one link in the
// app's default colours.

import type { Page } from "@playwright/test";

import { type FakeBackend, MEMBER, NEW_USER, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, shooting, test } from "../fixtures";
import { NOW } from "../shots";
import { enterCode, expectHome, logIn, push } from "./steps";
import {
  AA,
  bothThemes,
  focusProblems,
  frameHolds,
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

/**
 * The updates' states (ADR-0022, U27, U33): the notice at the login and on Home, installing and
 * failed; then Profile's Updates section with a newer version, checking, up to date, failed and
 * off. Each reload is a new start, whose one check answers what the fake was told.
 */
async function updateStates(page: Page, backend: FakeBackend): Promise<void> {
  const notice = page.locator("[data-slot=update-notice]");
  backend.answerUpdate({ state: "available", current: "0.1.0", version: "0.2.0" });
  await page.getByRole("group", { name: "Account" }).getByRole("button", { name: "Log out" }).click();
  await page.reload();
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await expect(notice.getByText("afkfleet 0.2.0 is available.")).toBeVisible();
  await bothThemes(page, "the update notice, at the login");

  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await expect(notice.getByText("afkfleet 0.2.0 is available.")).toBeVisible();
  await bothThemes(page, "the update notice, on Home");

  await notice.getByRole("button", { name: "Install and restart" }).click();
  await expect(notice.getByText(/^Downloading the update…/)).toBeVisible();
  await bothThemes(page, "the update notice, installing");

  backend.answerInstall("update_invalid");
  await page.reload();
  await notice.getByRole("button", { name: "Install and restart" }).click();
  await expect(notice.getByRole("button", { name: "Try again" })).toBeVisible();
  await bothThemes(page, "the update notice, failed");
  await notice.getByRole("button", { name: "Later" }).click();
  backend.answerInstall(null);

  const updates = page.locator("section", {
    has: page.getByRole("heading", { name: "Updates", level: 2 }),
  });
  await nav(page, "Profile");
  await expect(updates.getByText("afkfleet 0.2.0 is available.")).toBeVisible();
  await bothThemes(page, "profile, updates, available", { height: 1300 });

  backend.hold("check_update");
  await updates.getByRole("button", { name: "Check for updates" }).click();
  await expect(updates.getByRole("button", { name: "Checking…" })).toBeDisabled();
  await bothThemes(page, "profile, updates, checking", { height: 1300 });
  backend.answerUpdate({ state: "up_to_date", current: "0.1.0" });
  backend.release();
  await expect(updates.getByText("afkfleet is up to date.")).toBeVisible();
  await bothThemes(page, "profile, updates, up to date", { height: 1300 });

  backend.answerUpdate("update_unreachable");
  await updates.getByRole("button", { name: "Check for updates" }).click();
  await expect(updates.getByText(/^Couldn't check for updates:/)).toBeVisible();
  await bothThemes(page, "profile, updates, failed", { height: 1300 });

  backend.answerUpdate({ state: "off", current: "0.1.0" });
  await page.reload();
  await nav(page, "Profile");
  await expect(updates.getByText("Updates are off in this build.")).toBeVisible();
  await bothThemes(page, "profile, updates, off", { height: 1300 });
  backend.answerUpdate({ state: "up_to_date", current: "0.1.0" });
}

test("the check itself reports what it should, at the numbers it should", async ({
  page,
  context,
}) => {
  test.skip(shooting(), "the check's own tests measure; they make no baseline");
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
  test.skip(shooting(), "the check's own tests measure; they make no baseline");
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
  test.skip(shooting(), "the check's own tests measure; they make no baseline");
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
  test.skip(shooting(), "the check's own tests measure; they make no baseline");
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

  // The walk leaves the page at its end and comes back, which the window hears as its focus going
  // and coming back. That's no Tab in the row: a listener there that drops an alert, as the agents'
  // read on focus does to their error (R177), changes nothing, and hears again once the walk ends
  // (R260).
  await page.evaluate(() => {
    const alert = document.createElement("div");
    alert.dataset.planted = "yes";
    alert.setAttribute("role", "alert");
    document.body.append(alert);
    const heard = { focus: 0, blur: 0 };
    const stop = new AbortController();
    Object.assign(window, { __heard: heard, __stopHearing: stop });
    addEventListener(
      "focus",
      () => {
        heard.focus += 1;
        alert.remove();
      },
      { signal: stop.signal },
    );
    addEventListener("blur", () => (heard.blur += 1), { signal: stop.signal });
  });
  const heard = () =>
    page.evaluate(
      () => (window as unknown as { __heard: { focus: number; blur: number } }).__heard,
    );
  expect(await focusProblems(page)).toEqual([]);
  expect(await heard()).toEqual({ focus: 0, blur: 0 });
  await page.evaluate(() => dispatchEvent(new FocusEvent("focus")));
  expect(await heard()).toEqual({ focus: 1, blur: 0 });
  await page.evaluate(() =>
    (window as unknown as { __stopHearing: AbortController }).__stopHearing.abort(),
  );
  await removePlants();
});

test("every native select holds the theme's tokens, its options too", async ({ page, context }) => {
  test.skip(shooting(), "the check's own tests measure; they make no baseline");
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
  await bothThemes(page, "Home, every status", { small: true });

  const id = backend.bot("AfkBot1").id;
  backend.addChat("AfkBot1", {
    at: NOW,
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
    event: { type: "bot_died", bot_id: id, at: NOW },
  });
  await expect(main.getByRole("region", { name: "Timeline" }).getByText("Died")).toBeVisible();
  const toast = page.getByText("AfkBot1 died.");
  await toast.hover();
  await bothThemes(page, "a bot, its timeline and a toast", { toasts: 1, keepPointer: true });
  await expect(toast).toBeVisible();

  await page.getByRole("link", { name: "Home", exact: true }).first().click();
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  // The died toast goes first, so the row below shows its error toast alone (S131).
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, { timeout: 10_000 });

  await push(page, {
    update: "event",
    event: {
      type: "bot_alert",
      bot_id: backend.bot("AfkBot3").id,
      at: NOW,
      state: "failed",
      reason: "crash_loop",
    },
  });
  const failed = page.locator("[data-sonner-toast][data-type=error]");
  await failed.hover();
  await bothThemes(page, "Home, its notice and an error toast", { toasts: 1, keepPointer: true });
  await expect(failed).toBeVisible();
});

test("a bot's console in every look, and its screen stacked and loading", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.addAccount(OWNER);
  const at = NOW;
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
  await bothThemes(page, "a bot's screen, stacked", { end: "all" });
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
    status: { kind: "unknown", state: "online", agent_last_seen_at: NOW },
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
  await textsInBothThemes(page, "Share with, its list open and an option active", {
    keepFocus: true,
  });
  await page.keyboard.press("Escape");
  await shareField.fill("");
  await table.getByRole("button", { name: `Revoke ${MEMBER}'s access` }).click();
  await table.getByRole("button", { name: "Revoke", exact: true }).click();
  await expect(main.getByLabel("Current password")).toBeVisible();
  await bothThemes(page, "an account, a row's step-up", { height: 800 });

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
  await bothThemes(page, "accounts, empty, the tiles", { small: true });
  await tile.hover();
  await textsInBothThemes(page, "accounts, empty, a tile hovered", { keepPointer: true });
  await main.getByRole("link", { name: "Microsoft account" }).hover();
  await textsInBothThemes(page, "accounts, empty, the main tile hovered", {
    keepPointer: true,
  });
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
  await bothThemes(page, "profile", { height: 1300 });
});

test("modes: the list, the editor, a conflict, and a locked one", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");
  const save = main.getByRole("button", { name: "Save", exact: true });

  await nav(page, "Modes");
  await expect(main.getByRole("link", { name: "New mode" })).toBeVisible();
  await bothThemes(page, "modes", { small: true });

  await main.getByRole("link", { name: "New mode" }).click();
  await main.getByLabel("Name").fill("Fishing");
  const joining = main.getByRole("group", { name: "When it joins", exact: true });
  await joining.getByRole("button", { name: "Add step" }).click();
  await bothThemes(page, "the mode editor", { small: true, smallHeight: 1000 });
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

test("modes: loading, failed, none, a long list, and sixteen steps in two columns and stacked", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const main = page.getByRole("main");
  backend.clearModes();
  // Home reads the modes too, so the read is held from the sign-in on.
  backend.hold("modes");
  await signIn(page, backend, MEMBER);
  await nav(page, "Modes");
  await expect(main.getByText("Loading modes…")).toBeVisible();
  await bothThemes(page, "modes, loading");
  backend.release();
  await expect(main.getByText("No modes.")).toBeVisible();
  await bothThemes(page, "modes, none");

  await page.getByRole("button", { name: "Log out" }).click();
  backend.refuse("modes", { status: 500, code: "internal" });
  await signIn(page, backend, MEMBER);
  await nav(page, "Modes");
  await expect(main.getByText(/Couldn't load modes/)).toBeVisible();
  await bothThemes(page, "modes, failed");
  backend.refuse("modes", null);

  for (let n = 1; n <= 24; n += 1) {
    backend.addMode(MEMBER, `Mode with a longer name ${n}`);
  }
  backend.addMode(
    MEMBER,
    "Sixteen steps",
    Array.from({ length: 16 }, (_, n) => ({
      action: { type: "send_chat" as const, message: `hello ${n}` },
      schedule:
        n < 6
          ? { type: "at_start" as const }
          : { type: "every" as const, interval_ms: 60_000, jitter_ms: 0 },
      probability: 100,
    })),
  );
  await page.getByRole("button", { name: "Log out" }).click();
  await signIn(page, backend, MEMBER);
  await nav(page, "Modes");
  await expect(main.getByRole("link", { name: "Open Mode with a longer name 24" })).toBeVisible();
  await bothThemes(page, "modes, a long list", { height: 1200, small: true, smallHeight: 2000 });
  await main.getByRole("link", { name: "Open Sixteen steps" }).click();
  await expect(main.getByRole("heading", { name: "Sixteen steps", level: 1 })).toBeVisible();
  await bothThemes(page, "a mode, sixteen steps");
  await page.setViewportSize({ width: 800, height: 600 });
  await bothThemes(page, "a mode, sixteen steps, stacked", { end: "ends" });
  await page.setViewportSize({ width: 1280, height: 720 });
});

test("servers: loading, failed, a long list, and sixteen texts with eight formats, tried", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const main = page.getByRole("main");
  await signIn(page, backend, OWNER);
  backend.hold("server_settings");
  await nav(page, "Minecraft servers");
  await expect(main.getByText("Loading servers…")).toBeVisible();
  await bothThemes(page, "servers, loading");
  backend.release();
  await expect(main.getByText("No servers.")).toBeVisible();

  await page.getByRole("button", { name: "Log out" }).click();
  backend.refuse("server_settings", { status: 500, code: "internal" });
  await signIn(page, backend, OWNER);
  await nav(page, "Minecraft servers");
  await expect(main.getByText(/Couldn't load servers/)).toBeVisible();
  await bothThemes(page, "servers, failed");
  backend.refuse("server_settings", null);

  for (let n = 1; n <= 12; n += 1) {
    backend.addServerSettings(`play-${n}.example.com`, [
      `You logged in from another location ${n}`,
    ]);
  }
  backend.addServerSettings(
    "long.example.com",
    Array.from({ length: 16 }, (_, n) => `You logged in from another location (${n + 1})`),
    Array.from({ length: 8 }, (_, n) => `[Clan ${n + 1}] {sender} : `),
  );
  await page.getByRole("button", { name: "Log out" }).click();
  await signIn(page, backend, OWNER);
  await nav(page, "Minecraft servers");
  await expect(main.getByRole("link", { name: "Open long.example.com:25565" })).toBeVisible();
  await bothThemes(page, "servers, a long list", { small: true, smallHeight: 700 });
  await main.getByRole("link", { name: "Open long.example.com:25565" }).click();
  await main.getByLabel("Try a line").fill("[Clan 1] Steve : hello");
  await main.getByRole("button", { name: "Try", exact: true }).click();
  await expect(main.getByText("Format 1: sender Steve.")).toBeVisible();
  await bothThemes(page, "a server, sixteen texts and eight formats, tried");
  await page.setViewportSize({ width: 800, height: 600 });
  await bothThemes(page, "a server, sixteen texts and eight formats, stacked", { end: "ends" });
  await page.setViewportSize({ width: 1280, height: 720 });
});

test("stacked editors: Save's step-up and a conflict, where you are", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addMode(
    OWNER,
    "Long",
    Array.from({ length: 16 }, () => ({
      action: { type: "jump" as const },
      schedule: { type: "at_start" as const },
      probability: 100,
    })),
  );
  backend.addServerSettings(
    "long.example.com",
    Array.from({ length: 16 }, (_, n) => `You logged in from another location (${n + 1})`),
    Array.from({ length: 8 }, (_, n) => `[Clan ${n + 1}] {sender} : `),
  );
  await signIn(page, backend, OWNER);
  await page.setViewportSize({ width: 800, height: 600 });
  const main = page.getByRole("main");
  const save = main.getByRole("button", { name: "Save", exact: true });
  const password = page.getByLabel("Current password");
  const toTop = () =>
    page.locator("[data-slot=content]").evaluate((column) => {
      column.scrollTop = 0;
    });
  const noToasts = () =>
    expect(page.locator("[data-sonner-toast]")).toHaveCount(0, { timeout: 15_000 });

  await nav(page, "Modes");
  await main.getByRole("link", { name: "Open Long" }).click();
  const respawn = main.getByRole("group", { name: "After it respawns", exact: true });
  await respawn.getByRole("button", { name: "Add step" }).click();
  const step = respawn.getByRole("group", { name: "Step 1", exact: true });
  await step.getByLabel("What it does").selectOption({ label: "Send chat or a command" });
  await step.getByLabel("Message").fill("/warp home");
  await toTop();
  await save.click();
  await expect(password).toBeFocused();
  await bothThemes(page, "a mode, stacked, the step-up", { end: "ends" });
  await password.fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(main.getByText("Sends a command off the allowlist.")).toBeVisible();
  await noToasts();
  backend.changeMode("Long", { name: "Elsewhere" });
  await main.getByLabel("Name").fill("Mine");
  await toTop();
  await save.click();
  await expect(main.getByText(/^This mode changed since you opened it/)).toBeVisible();
  await bothThemes(page, "a mode, stacked, a conflict", { end: "ends" });

  // A new session, as the mode's step-up still holds for 5 minutes in this one.
  await page.getByRole("button", { name: "Log out" }).click();
  await signIn(page, backend, OWNER);
  await nav(page, "Minecraft servers");
  await main.getByRole("link", { name: "Open long.example.com:25565" }).click();
  await expect(
    main.getByRole("heading", { name: "long.example.com:25565", level: 1 }),
  ).toBeVisible();
  await main.getByLabel("Conflict text 1", { exact: true }).fill("You logged in from elsewhere");
  await toTop();
  await save.click();
  await expect(password).toBeFocused();
  await bothThemes(page, "a server, stacked, the step-up", { end: "ends" });
  await password.fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText("Saved. Bots on it use the change at once.")).toBeVisible();
  await noToasts();
  backend.changeServerSettings("long.example.com");
  await main.getByLabel("Conflict text 2", { exact: true }).fill("Logged in elsewhere");
  await toTop();
  await save.click();
  await expect(main.getByText(/^These settings changed since you opened them/)).toBeVisible();
  await bothThemes(page, "a server, stacked, changed settings", { end: "ends" });
  await page.setViewportSize({ width: 1280, height: 720 });
});

test("users and the grants review: loading, failed, and long lists", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  const main = page.getByRole("main");
  await signIn(page, backend, OWNER);
  backend.hold("users");
  await nav(page, "Users");
  await expect(main.getByText("Loading users…")).toBeVisible();
  await bothThemes(page, "users, loading");
  backend.release();
  await expect(main.getByRole("table", { name: "Users" })).toBeVisible();

  await page.getByRole("button", { name: "Log out" }).click();
  backend.refuse("users", { status: 500, code: "internal" });
  await signIn(page, backend, OWNER);
  await nav(page, "Users");
  await expect(main.getByText(/Couldn't load users/)).toBeVisible();
  await bothThemes(page, "users, failed");
  backend.refuse("users", null);

  for (let n = 1; n <= 30; n += 1) {
    backend.addMember(`longusername_with_32_characte${String(n).padStart(3, "0")}`, {
      disabled: n % 3 === 0,
    });
  }
  for (let n = 1; n <= 20; n += 1) {
    backend.share(backend.addAccount(OWNER), MEMBER, "control");
  }
  const theirs = backend.addAccount(MEMBER);
  for (let n = 1; n <= 20; n += 1) {
    backend.share(theirs, `longusername_with_32_characte${String(n).padStart(3, "0")}`, "view");
  }
  await page.getByRole("button", { name: "Log out" }).click();
  await signIn(page, backend, OWNER);
  await nav(page, "Users");
  await expect(main.getByRole("row", { name: /longusername_with_32_characte030/ })).toBeVisible();
  await bothThemes(page, "users, a long list", { height: 1600, small: true, smallHeight: 3000 });

  backend.hold("user_grants");
  backend.hold("shared_accounts");
  await main
    .getByRole("row", { name: new RegExp(MEMBER) })
    .getByRole("link", { name: /Review grants/ })
    .click();
  await expect(main.getByText("Loading grants…")).toBeVisible();
  await bothThemes(page, "the grants review, loading");
  backend.release();
  await expect(main.getByRole("table", { name: "Grants they hold" })).toBeVisible();
  await main.getByRole("button", { name: /Show who has access/ }).click();
  await expect(main.getByRole("table", { name: /Who has access to/ })).toBeVisible();
  await bothThemes(page, "the grants review, long lists", {
    height: 1100,
    small: true,
    smallHeight: 3000,
  });
});

test("invites: loading, failed, a long list, and a new invite's code", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const main = page.getByRole("main");
  await signIn(page, backend, OWNER);
  backend.hold("invites");
  await nav(page, "Invites");
  await expect(main.getByText("Loading invites…")).toBeVisible();
  await bothThemes(page, "invites, loading");
  backend.release();
  await expect(main.getByRole("table", { name: "Invites" })).toBeVisible();

  await page.getByRole("button", { name: "Log out" }).click();
  backend.refuse("invites", { status: 500, code: "internal" });
  await signIn(page, backend, OWNER);
  await nav(page, "Invites");
  await expect(main.getByText(/Couldn't load invites/)).toBeVisible();
  await bothThemes(page, "invites, failed");
  backend.refuse("invites", null);

  // Eighteen more beside the fake's one, under the cap of 20 pending.
  await main.getByRole("button", { name: "New invite" }).click();
  await main.getByRole("button", { name: "Create invite" }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(main.getByRole("button", { name: "Done" })).toBeVisible();
  for (let n = 2; n <= 18; n += 1) {
    await main.getByRole("button", { name: "Done" }).click();
    await main.getByRole("button", { name: "New invite" }).click();
    await main.getByRole("button", { name: "Create invite" }).click();
    await expect(main.getByRole("button", { name: "Done" })).toBeVisible();
  }
  await expect(main.getByText(/Shown only now/)).toBeVisible();
  await bothThemes(page, "invites, a long list and a new invite's code", {
    height: 1200,
    small: true,
    smallHeight: 2000,
  });
});

test("agents: loading, none, failed, and a long list", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  const main = page.getByRole("main");
  await signIn(page, backend, OWNER);
  backend.hold("agents");
  await nav(page, "Agents");
  await expect(main.getByText("Loading agents…")).toBeVisible();
  await bothThemes(page, "agents, loading");
  backend.release();
  await expect(main.getByText("No agents.")).toBeVisible();
  await bothThemes(page, "agents, none");

  await page.getByRole("button", { name: "Log out" }).click();
  backend.refuse("agents", { status: 500, code: "internal" });
  await signIn(page, backend, OWNER);
  await nav(page, "Agents");
  await expect(main.getByText(/Couldn't load agents/)).toBeVisible();
  await bothThemes(page, "agents, failed");
  backend.refuse("agents", null);

  const statuses = ["online", "offline", "expired", "disabled"] as const;
  for (let n = 1; n <= 30; n += 1) {
    const status = statuses[n % statuses.length] ?? "online";
    backend.addAgent(`agent-on-the-host-with-a-long-name-${String(n).padStart(3, "0")}`, {
      status,
      enabled: status !== "disabled",
      bots: n % 3 === 0 ? 12 : 1,
      version: "0.11.0+0123456789abcdef",
    });
  }
  await page.getByRole("button", { name: "Log out" }).click();
  await signIn(page, backend, OWNER);
  await nav(page, "Agents");
  await expect(main.getByRole("row", { name: /long-name-030/ })).toBeVisible();
  await bothThemes(page, "agents, a long list", { height: 2100, small: true, smallHeight: 3500 });
});

test("agents: a certificate ending soon", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  // The page's clock: the shots' fixed one, or the real one here.
  const now = shooting() ? Date.parse(NOW) : Date.now();
  backend.addAgent("agent-late", { status: "expired" });
  backend.addAgent("agent-soon", { not_after: new Date(now + 3 * 86_400_000).toISOString() });
  backend.addAgent("agent-main");
  await signIn(page, backend, OWNER);
  await nav(page, "Agents");
  const main = page.getByRole("main");
  await expect(main.getByText("Expires in 3 days. Enroll a new agent before then.")).toBeVisible();
  await bothThemes(page, "agents, a certificate ending soon", { small: true });
});

test("the audit log: loading, nothing, failed, without names, narrowed, and a long list", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const main = page.getByRole("main");
  await signIn(page, backend, OWNER);
  backend.hold("audit");
  await nav(page, "Audit log");
  await expect(main.getByText("Loading audit log…")).toBeVisible();
  await bothThemes(page, "the audit log, loading");
  backend.release();
  await expect(main.getByText("Nothing recorded.")).toBeVisible();
  await bothThemes(page, "the audit log, nothing recorded");

  await page.getByRole("button", { name: "Log out" }).click();
  backend.refuse("audit", { status: 500, code: "internal" });
  await signIn(page, backend, OWNER);
  await nav(page, "Audit log");
  await expect(main.getByText(/Couldn't load audit log/)).toBeVisible();
  await bothThemes(page, "the audit log, failed");
  backend.refuse("audit", null);

  const member = backend.userId(MEMBER);
  for (const action of ["user.set_role", "user.set_disabled", "user.change_password"] as const) {
    backend.addAuditEntry({
      action,
      actor: backend.userId(OWNER),
      target: { kind: "user", id: member },
      metadata: [{ key: "user_id", text: member }],
    });
  }
  backend.addAgent("agent-without-entries");
  await page.getByRole("button", { name: "Log out" }).click();
  backend.refuse("users", { status: 500, code: "internal" });
  await signIn(page, backend, OWNER);
  await nav(page, "Audit log");
  await expect(main.getByText("Names unavailable. Users show by ID.")).toBeVisible();
  await bothThemes(page, "the audit log, names unavailable");
  backend.refuse("users", null);

  await page.getByRole("button", { name: "Log out" }).click();
  await signIn(page, backend, OWNER);
  await nav(page, "Agents");
  await main.getByRole("link", { name: "Audit of agent-without-entries" }).click();
  await expect(main.getByText("Nothing for this target.")).toBeVisible();
  await bothThemes(page, "the audit log, nothing for this target");
  await nav(page, "Users");
  await main.getByRole("link", { name: `Audit of ${MEMBER}` }).click();
  await expect(main.getByText(/What happened to this user/)).toBeVisible();
  await bothThemes(page, "the audit log, on a user", { small: true, smallHeight: 900 });

  for (let n = 1; n <= 60; n += 1) {
    backend.addAuditEntry({
      action: n % 2 === 0 ? "agent.connect" : "security.agent_identity_conflict",
      outcome: n % 3 === 0 ? "failure" : "success",
      ip: "2001:db8:ffff:ffff:ffff:ffff:ffff:fffe",
      target: {
        kind: "agent",
        id: `01900000-0000-7000-8000-0000000003${String(n).padStart(2, "0")}`,
      },
      metadata: [
        { key: "reason", text: "name_mismatch" },
        { key: "agent_name", text: "agent-on-the-host-with-a-long-name-and-more-001" },
        { key: "certificate_serial", text: "7f3a9c0e5b2d4f6a8c1e3b5d7f9a0c2e4b6d8f0a" },
      ],
    });
  }
  backend.addUnreadableEntry();
  await nav(page, "Audit log");
  await expect(main.getByRole("button", { name: "Show more" })).toBeVisible();
  await bothThemes(page, "the audit log, a long list", { end: "ends" });
  await page.setViewportSize({ width: 800, height: 600 });
  await expect(main.getByRole("table", { name: "Audit log" })).toHaveCSS("display", "block");
  await bothThemes(page, "the audit log, a long list, stacked", { end: "ends" });
  await page.setViewportSize({ width: 1280, height: 720 });
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
  await expect(main.getByText("No servers.")).toBeVisible();
  await bothThemes(page, "servers, empty");
  await main.getByRole("link", { name: "Add server" }).click();
  await page.getByLabel("Server address").fill("MC.example.com");
  await main.getByRole("button", { name: "Save" }).click();
  await expect(page.getByLabel("Current password")).toBeVisible();
  await bothThemes(page, "a server's settings, the step-up");
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(main.getByRole("heading", { name: "mc.example.com:25565", level: 1 })).toBeVisible();
  // Its toast goes first, so no row below measures it on its way out, below the window (R261).
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, { timeout: 10_000 });
  await nav(page, "Minecraft servers");
  await expect(main.getByRole("listitem").filter({ hasText: "mc.example.com" })).toBeVisible();
  await bothThemes(page, "servers", { small: true });

  await nav(page, "Accounts");
  const accountLink = main.getByRole("link", { name: "AfkBot1", exact: true });
  await expect(accountLink).toBeVisible();
  await bothThemes(page, "accounts", { small: true });
  await accountLink.click();
  await expect(main.getByRole("table", { name: "Who has access" })).toBeVisible();
  await bothThemes(page, "an account and who has access", { small: true, smallHeight: 800 });

  await nav(page, "Users");
  await expect(main.getByRole("row", { name: new RegExp(MEMBER) })).toBeVisible();
  await bothThemes(page, "users", { small: true });
  await main
    .getByRole("row", { name: new RegExp(MEMBER) })
    .getByRole("link", { name: /Review grants/ })
    .click();
  await main.getByRole("button", { name: /Show who has access/ }).click();
  await expect(main.getByRole("table", { name: /Who has access to/ })).toBeVisible();
  await bothThemes(page, "the grants review", { small: true });

  await nav(page, "Invites");
  await expect(main.getByRole("heading", { name: "Invites", level: 1 })).toBeVisible();
  await bothThemes(page, "invites", { small: true });
  await nav(page, "Agents");
  await expect(main.getByRole("row", { name: /agent-main/ })).toBeVisible();
  await bothThemes(page, "agents", { small: true });
  await main
    .getByRole("row", { name: /agent-main/ })
    .getByRole("button", { name: "Disable" })
    .click();
  await expect(main.getByText(/Disable agent-main for good/)).toBeVisible();
  await bothThemes(page, "agents, the question");
  await nav(page, "Audit log");
  await expect(main.getByText(/unreadable\./)).toBeVisible();
  await bothThemes(page, "the audit log, an unreadable row", { small: true });
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
  await textsInBothThemes(page, "the collapsed sidebar, a tooltip open", { skip: true });
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

  await updateStates(page, backend);

  for (const scheme of ["dark", "light"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/?panel=error");
    await expect(page.getByRole("heading", { name: /App error/ })).toBeVisible();
    await measured(page, "the error panel", scheme);
    await frameHolds(page, `the error panel, ${scheme}`);
  }
});
