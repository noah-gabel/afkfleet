import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApiError } from "../../api/FakeApiClient";
import { ME_STALE_MS } from "../../api/queries";
import type { CommandErrorCode } from "../../generated/CommandErrorCode";
import { me, PROFILE } from "../../test/fixtures";
import { renderApp } from "../../test/renderApp";
import { onHome } from "../../test/signedIn";
import { CERTIFICATE_HELP } from "../server/certificateHelp";

describe("the launch", () => {
  it("opens server setup when no server is set up", async () => {
    const app = renderApp({ serverProfile: async () => ({ state: "none" }) });

    await waitFor(() => expect(app.path()).toBe("/setup"));
    expect(app.fake.calls.map((call) => call.method)).toEqual(["serverProfile"]);
  });

  it("opens server setup when the saved profile is unreadable", async () => {
    const app = renderApp({
      serverProfile: async () => ({ state: "unreadable", reason: "json" }),
    });

    await waitFor(() => expect(app.path()).toBe("/setup"));
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
  });

  it("opens Home for a live session, signed in with a new generation", async () => {
    const app = renderApp({ serverProfile: async () => PROFILE, me: async () => me() });

    await waitFor(() => expect(app.path()).toBe("/home"));
    expect(app.runtime.stores.session.getState()).toMatchObject({
      phase: "signed_in",
      generation: 1,
    });
    await onHome("afkbot1");
  });

  it("caches the user it found, so Home doesn't ask again", async () => {
    const app = renderApp({ serverProfile: async () => PROFILE, me: async () => me() });

    await onHome("afkbot1");

    expect(app.fake.calls.filter((call) => call.method === "me")).toHaveLength(1);
    expect(app.runtime.queryClient.getQueryState(["me"])?.dataUpdatedAt).toBeGreaterThan(
      Date.now() - ME_STALE_MS,
    );
  });

  it("opens login without a session, and says nothing about a session ending", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("no_session");
      },
    });

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
    expect(screen.queryByText("Your session ended. Log in again.")).toBeNull();
  });

  it.each<CommandErrorCode>(["unreachable", "timed_out"])(
    "waits on the start screen while the server is %s, then goes on once it answers",
    async (code) => {
      let reachable = false;
      const app = renderApp({
        serverProfile: async () => PROFILE,
        me: async () => {
          if (!reachable) {
            throw fakeApiError(code);
          }
          return me();
        },
        checkServer: async () => {},
      });

      expect(await screen.findByText("Waiting for the server…")).toBeDefined();
      expect(await screen.findByText(/Can't reach the server/)).toBeDefined();
      reachable = true;
      await app.user.click(screen.getByRole("button", { name: "Retry now" }));

      await waitFor(() => expect(app.path()).toBe("/home"));
      expect(app.runtime.stores.session.getState().phase).toBe("signed_in");
    },
  );

  it("explains a refused certificate on the start screen", async () => {
    renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("certificate");
      },
    });

    expect(await screen.findAllByText(CERTIFICATE_HELP)).not.toHaveLength(0);
    const start = screen.getByRole("main");
    expect(within(start).getByText(CERTIFICATE_HELP)).toBeDefined();
    expect(within(start).getByRole("button", { name: "Server settings" })).toBeDefined();
  });

  it("offers a way out of a session the app can't use", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("store");
      },
      logout: async () => ({ server: "ended" }),
    });

    expect(await screen.findByText("fake store")).toBeDefined();
    expect(screen.getByRole("button", { name: "Try again" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Server settings" })).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(app.fake.calls.map((call) => call.method)).toContain("logout");
  });

  it("tries the session again from the start screen", async () => {
    let attempts = 0;
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        attempts += 1;
        if (attempts === 1) {
          throw fakeApiError("interrupted");
        }
        return me();
      },
    });

    await screen.findByText("fake interrupted");
    await app.user.click(screen.getByRole("button", { name: "Try again" }));

    await waitFor(() => expect(app.path()).toBe("/home"));
  });

  it("goes to server setup from the start screen", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("store");
      },
    });

    await screen.findByText("fake store");
    await app.user.click(screen.getByRole("button", { name: "Server settings" }));

    await waitFor(() => expect(app.path()).toBe("/setup"));
  });

  it("stays on the start screen when the logout itself fails", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("store");
      },
      logout: async () => {
        throw fakeApiError("ipc");
      },
    });

    await screen.findByText("fake store");
    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    expect(await screen.findByText("Couldn't log out: fake ipc Try again.")).toBeDefined();
    expect(app.path()).toBe("/");
  });

  it("shows why the web build can't start", async () => {
    renderApp({
      serverProfile: async () => {
        throw fakeApiError("ipc");
      },
    });

    expect(await screen.findByRole("heading", { level: 1, name: "afkfleet" })).toBeDefined();
    expect(screen.getByText("fake ipc")).toBeDefined();
  });
});
