// The app's updates (Plan.md P12.5; ADR-0022, U27, U28, U33, S27, S28, S29): one check at each
// start, its notice under the banner on every screen, Install and restart or Later; and Profile's
// Updates section, which checks on demand. The texts are the user's, as approved.
import { act, cleanup, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ApiClient } from "../../api/ApiClient";
import { ApiError } from "../../api/ApiError";
import { fakeApiError } from "../../api/FakeApiClient";
import type { CommandErrorCode } from "../../generated/CommandErrorCode";
import type { UpdateCheck } from "../../generated/UpdateCheck";
import { PROFILE } from "../../test/fixtures";
import { renderApp } from "../../test/renderApp";
import { atScreen, onHome, signedIn } from "../../test/signedIn";
import { atLogin, enterCode, logIn, MFA } from "../../test/signIn";

const AVAILABLE: UpdateCheck = { state: "available", current: "0.1.0", version: "0.2.0" };
const UP_TO_DATE: UpdateCheck = { state: "up_to_date", current: "0.1.0" };
const OFF: UpdateCheck = { state: "off", current: "0.1.0" };

/** An updater's refusal with its approved sentence. */
function refused(code: CommandErrorCode, message: string): ApiError {
  return new ApiError({ code, message });
}

const UNREACHABLE = () =>
  refused("update_unreachable", "GitHub can't be reached to get the update.");
const INVALID = () => refused("update_invalid", "The download couldn't be verified.");
const INSTALL = () => refused("update_install", "The installer couldn't be started.");

/** The approved sentences of the codes an install answers before Rust stops the watch. */
const BEFORE_THE_STOP: [CommandErrorCode, string][] = [
  ["update_unreachable", "GitHub can't be reached to get the update."],
  ["update_invalid", "The download couldn't be verified."],
  ["no_update", "There's no update to install. Check again."],
  ["update_busy", "An update check or install is already running."],
];

/** A call that never answers: the real install ends with the app's exit. */
const never = () => new Promise<never>(() => {});

/** The notice row, when it's there. */
function notice() {
  const row = document.querySelector<HTMLElement>('[data-slot="update-notice"]');
  return row === null ? null : within(row);
}

/** How many times the fake answered `method`. */
function calls(app: { fake: { calls: { method: string }[] } }, method: keyof ApiClient): number {
  return app.fake.calls.filter((call) => call.method === method).length;
}

describe("the update notice", () => {
  it("shows a newer version at the start, on the login screen, with Install and restart and Later", async () => {
    await atLogin({ checkUpdate: async () => AVAILABLE });

    const row = await screen.findByText("afkfleet 0.2.0 is available.");
    expect(row).toBeDefined();
    expect(notice()?.getByRole("button", { name: "Install and restart" })).toBeDefined();
    expect(notice()?.getByRole("button", { name: "Later" })).toBeDefined();
  });

  it("checks once per start, not again for a login, a screen or a logout", async () => {
    const app = await signedIn({ checkUpdate: async () => AVAILABLE });
    await screen.findByText("afkfleet 0.2.0 is available.");

    await act(() => app.runtime.router.navigate({ to: "/profile" }));
    await act(() => app.runtime.router.navigate({ to: "/" }));
    await screen.findByText("afkfleet 0.2.0 is available.");

    expect(calls(app, "checkUpdate")).toBe(1);
  });

  it("goes away for Later until the next start", async () => {
    const app = await signedIn({ checkUpdate: async () => AVAILABLE });
    await screen.findByText("afkfleet 0.2.0 is available.");

    await app.user.click(notice()?.getByRole("button", { name: "Later" }) as HTMLElement);
    await act(() => app.runtime.router.navigate({ to: "/profile" }));

    expect(notice()?.queryByText("afkfleet 0.2.0 is available.") ?? null).toBeNull();
  });

  it("installs on Install and restart, and says the app restarts by itself", async () => {
    const app = await signedIn({ checkUpdate: async () => AVAILABLE, installUpdate: never });
    await screen.findByText("afkfleet 0.2.0 is available.");

    await app.user.click(
      notice()?.getByRole("button", { name: "Install and restart" }) as HTMLElement,
    );

    expect(
      await screen.findByText(
        "Downloading the update… afkfleet restarts by itself once it's installed.",
      ),
    ).toBeDefined();
    expect(notice()?.queryByRole("button") ?? null).toBeNull();
    expect(calls(app, "installUpdate")).toBe(1);
  });

  it("says why an install failed, offers Try again, which checks and installs again", async () => {
    let installs = 0;
    const app = await signedIn({
      checkUpdate: async () => AVAILABLE,
      installUpdate: async () => {
        installs += 1;
        if (installs === 1) {
          throw INVALID();
        }
        return never();
      },
    });
    await screen.findByText("afkfleet 0.2.0 is available.");
    const restarts = app.runtime.stores.live.getState().restarts;

    await app.user.click(
      notice()?.getByRole("button", { name: "Install and restart" }) as HTMLElement,
    );
    await screen.findByText("The update wasn't installed: The download couldn't be verified.");
    // The download failed its checks before Rust stopped the watch: it never stopped (S29).
    expect(app.runtime.stores.live.getState().restarts).toBe(restarts);
    expect(notice()?.getByRole("button", { name: "Later" })).toBeDefined();
    await app.user.click(notice()?.getByRole("button", { name: "Try again" }) as HTMLElement);

    expect(
      await screen.findByText(
        "Downloading the update… afkfleet restarts by itself once it's installed.",
      ),
    ).toBeDefined();
    expect(calls(app, "checkUpdate")).toBe(2);
    expect(calls(app, "installUpdate")).toBe(2);
  });

  it.each(BEFORE_THE_STOP)(
    "leaves the live watch alone when the install failed before stopping it (%s)",
    async (code, message) => {
      const app = await signedIn({
        checkUpdate: async () => AVAILABLE,
        installUpdate: async () => {
          throw refused(code, message);
        },
      });
      await screen.findByText("afkfleet 0.2.0 is available.");
      const restarts = app.runtime.stores.live.getState().restarts;
      const watches = calls(app, "watchEvents");

      await app.user.click(
        notice()?.getByRole("button", { name: "Install and restart" }) as HTMLElement,
      );
      await screen.findByText(`The update wasn't installed: ${message}`);
      await act(async () => {});

      expect(app.runtime.stores.live.getState().restarts).toBe(restarts);
      expect(calls(app, "watchEvents")).toBe(watches);
    },
  );

  it("starts the live watch again when the installer couldn't be started", async () => {
    const app = await signedIn({
      checkUpdate: async () => AVAILABLE,
      installUpdate: async () => {
        throw INSTALL();
      },
    });
    await screen.findByText("afkfleet 0.2.0 is available.");
    const restarts = app.runtime.stores.live.getState().restarts;
    const watches = calls(app, "watchEvents");

    await app.user.click(
      notice()?.getByRole("button", { name: "Install and restart" }) as HTMLElement,
    );
    await screen.findByText("The update wasn't installed: The installer couldn't be started.");
    await act(async () => {});

    expect(app.runtime.stores.live.getState().restarts).toBe(restarts + 1);
    expect(calls(app, "watchEvents")).toBe(watches + 1);
  });

  it("comes back after Later only at the next start", async () => {
    const first = await signedIn({ checkUpdate: async () => AVAILABLE });
    await screen.findByText("afkfleet 0.2.0 is available.");
    await first.user.click(notice()?.getByRole("button", { name: "Later" }) as HTMLElement);
    expect(screen.queryByText("afkfleet 0.2.0 is available.")).toBeNull();

    // A real restart: this App goes, and a new one starts with fresh stores.
    cleanup();
    await signedIn({ checkUpdate: async () => AVAILABLE });

    expect(await screen.findByText("afkfleet 0.2.0 is available.")).toBeDefined();
  });

  it("stays away after Later across a login and a logout, with no check again", async () => {
    const app = await atLogin({
      checkUpdate: async () => AVAILABLE,
      login: async () => MFA,
      logout: async () => ({ server: "ended" }),
    });
    await screen.findByText("afkfleet 0.2.0 is available.");
    await app.user.click(notice()?.getByRole("button", { name: "Later" }) as HTMLElement);

    await logIn(app);
    await enterCode(app, "123456");
    await onHome();
    const afterLogin = screen.queryByText("afkfleet 0.2.0 is available.");
    await app.user.click(screen.getByRole("button", { name: "Log out" }));
    await screen.findByRole("heading", { name: "Log in" });
    await act(async () => {});

    expect(afterLogin).toBeNull();
    expect(screen.queryByText("afkfleet 0.2.0 is available.")).toBeNull();
    expect(calls(app, "checkUpdate")).toBe(1);
  });

  it.each([
    ["up to date", async () => UP_TO_DATE],
    ["off", async () => OFF],
    [
      "unreachable",
      async () => {
        throw UNREACHABLE();
      },
    ],
  ] as const)(
    "shows nothing when the start's check is %s, and raises no banner",
    async (_, checkUpdate) => {
      const app = await signedIn({ checkUpdate });

      await act(async () => {});

      expect(notice()?.queryByRole("button") ?? null).toBeNull();
      expect(screen.queryByText(/is available/)).toBeNull();
      expect(app.runtime.stores.connection.getState().status).toBe("ok");
    },
  );
});

describe("Profile's Updates section", () => {
  /** The section. */
  async function section() {
    const heading = await screen.findByRole("heading", { name: "Updates", level: 2 });
    return within(heading.closest("section") as HTMLElement);
  }

  it("shows the installed version, and checks on demand", async () => {
    const app = await atScreen("/profile", { checkUpdate: async () => UP_TO_DATE });
    const updates = await section();
    expect(await updates.findByText("Installed: afkfleet 0.1.0.")).toBeDefined();

    await app.user.click(updates.getByRole("button", { name: "Check for updates" }));

    expect(await updates.findByText("afkfleet is up to date.")).toBeDefined();
    expect(calls(app, "checkUpdate")).toBe(2);
  });

  it("shows Checking… while a check runs", async () => {
    let answer: (check: UpdateCheck) => void = () => {};
    let checks = 0;
    const app = await atScreen("/profile", {
      checkUpdate: () => {
        checks += 1;
        return checks === 1
          ? Promise.resolve(UP_TO_DATE)
          : new Promise((resolve) => {
              answer = resolve;
            });
      },
    });
    const updates = await section();
    await updates.findByText("Installed: afkfleet 0.1.0.");

    await app.user.click(updates.getByRole("button", { name: "Check for updates" }));

    const checking = updates.getByRole("button", { name: "Checking…" }) as HTMLButtonElement;
    expect(checking.disabled).toBe(true);
    await act(async () => answer(AVAILABLE));
    expect(await updates.findByText("afkfleet 0.2.0 is available.")).toBeDefined();
  });

  it("offers Install and restart for a newer version it found", async () => {
    let checks = 0;
    const app = await atScreen("/profile", {
      checkUpdate: async () => {
        checks += 1;
        return checks === 1 ? UP_TO_DATE : AVAILABLE;
      },
      installUpdate: never,
    });
    const updates = await section();
    await updates.findByText("Installed: afkfleet 0.1.0.");

    await app.user.click(updates.getByRole("button", { name: "Check for updates" }));
    await updates.findByText("afkfleet 0.2.0 is available.");
    await app.user.click(updates.getByRole("button", { name: "Install and restart" }));

    expect(
      await screen.findByText(
        "Downloading the update… afkfleet restarts by itself once it's installed.",
      ),
    ).toBeDefined();
    expect(calls(app, "installUpdate")).toBe(1);
  });

  /** Profile with a newer version found at the start, and an install that fails as `failure`. */
  async function failingInstall(failure: () => ApiError, check = async () => AVAILABLE) {
    const app = await atScreen("/profile", {
      checkUpdate: check,
      installUpdate: async () => {
        throw failure();
      },
    });
    const updates = await section();
    await updates.findByText("afkfleet 0.2.0 is available.");
    await app.user.click(updates.getByRole("button", { name: "Install and restart" }));
    await screen.findByText(/^The update wasn't installed:/);
    return { app, updates };
  }

  // Rust keeps no release once an install failed, so Profile stops offering it (S28).
  it("stops offering Install and restart once an install failed", async () => {
    const { updates } = await failingInstall(INVALID);

    expect(updates.queryByRole("button", { name: "Install and restart" })).toBeNull();
    expect(updates.queryByText("afkfleet 0.2.0 is available.")).toBeNull();
    expect(updates.getByRole("button", { name: "Check for updates" })).toBeDefined();
  });

  it("after a failed install and Later, offers it again only once a check finds it", async () => {
    const { app, updates } = await failingInstall(INVALID);

    await app.user.click(notice()?.getByRole("button", { name: "Later" }) as HTMLElement);
    const afterLater = updates.queryByRole("button", { name: "Install and restart" });
    await app.user.click(updates.getByRole("button", { name: "Check for updates" }));

    expect(afterLater).toBeNull();
    expect(await updates.findByText("afkfleet 0.2.0 is available.")).toBeDefined();
    expect(updates.getByRole("button", { name: "Install and restart" })).toBeDefined();
  });

  it("a Try again whose check is up to date still says up to date", async () => {
    let checks = 0;
    const { app, updates } = await failingInstall(INVALID, async () => {
      checks += 1;
      return checks === 1 ? AVAILABLE : UP_TO_DATE;
    });

    await app.user.click(notice()?.getByRole("button", { name: "Try again" }) as HTMLElement);

    expect(await updates.findByText("afkfleet is up to date.")).toBeDefined();
    expect(
      await screen.findByText(
        "The update wasn't installed: There's no update to install. Check again.",
      ),
    ).toBeDefined();
  });

  it("says why a check it ran failed, but not the start's", async () => {
    const app = await atScreen("/profile", {
      checkUpdate: async () => {
        throw UNREACHABLE();
      },
    });
    const updates = await section();
    await act(async () => {});
    expect(updates.queryByText(/Couldn't check for updates/)).toBeNull();

    await app.user.click(updates.getByRole("button", { name: "Check for updates" }));

    expect(
      await updates.findByText(
        "Couldn't check for updates: GitHub can't be reached to get the update.",
      ),
    ).toBeDefined();
  });

  it("says when updates are off in this build, with no check to run", async () => {
    await atScreen("/profile", { checkUpdate: async () => OFF });
    const updates = await section();

    expect(await updates.findByText("Updates are off in this build.")).toBeDefined();
    expect(updates.queryByRole("button", { name: "Check for updates" })).toBeNull();
  });
});

describe("the app's version (U37)", () => {
  /** The app's own answer, apart from any check's `current`, so the source shows. */
  const VERSION = async () => ({ version: "0.3.1" });

  /** Profile's Updates section. */
  async function section() {
    const heading = await screen.findByRole("heading", { name: "Updates", level: 2 });
    return within(heading.closest("section") as HTMLElement);
  }

  it.each<[string, Partial<ApiClient>]>([
    ["before any check answers", { checkUpdate: never }],
    ["while updates are off", { checkUpdate: async () => OFF }],
    ["when the check is up to date", { checkUpdate: async () => UP_TO_DATE }],
    ["when a newer version is available", { checkUpdate: async () => AVAILABLE }],
    [
      "when the check failed",
      {
        checkUpdate: async () => {
          throw UNREACHABLE();
        },
      },
    ],
  ])("Profile shows the installed version %s, read from the app", async (_, handlers) => {
    await atScreen("/profile", { ...handlers, appVersion: VERSION });
    const updates = await section();

    expect(await updates.findByText("Installed: afkfleet 0.3.1.")).toBeDefined();
  });

  it("Profile shows it while a check it ran is checking", async () => {
    let checks = 0;
    const app = await atScreen("/profile", {
      appVersion: VERSION,
      checkUpdate: () => {
        checks += 1;
        return checks === 1 ? Promise.resolve(UP_TO_DATE) : never();
      },
    });
    const updates = await section();
    await updates.findByText("afkfleet is up to date.");

    await app.user.click(updates.getByRole("button", { name: "Check for updates" }));

    expect(updates.getByRole("button", { name: "Checking…" })).toBeDefined();
    expect(updates.getByText("Installed: afkfleet 0.3.1.")).toBeDefined();
  });

  it("the login screen shows it in a line of its own below the card, with no check answered", async () => {
    await atLogin({ appVersion: VERSION, checkUpdate: never });

    const line = await screen.findByText("afkfleet 0.3.1");
    const card = screen
      .getByRole("heading", { name: "Log in", level: 1 })
      .closest('[data-slot="card"]') as HTMLElement;

    expect(card.contains(line)).toBe(false);
    expect(card.compareDocumentPosition(line) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("the start screen shows it below its choices while it waits for the server", async () => {
    renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("unreachable");
      },
      appVersion: VERSION,
      checkUpdate: never,
    });
    await screen.findByText("Waiting for the server…");

    const line = await screen.findByText("afkfleet 0.3.1");
    const settings = screen.getByRole("button", { name: "Server settings" });

    expect(settings.compareDocumentPosition(line) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
});
