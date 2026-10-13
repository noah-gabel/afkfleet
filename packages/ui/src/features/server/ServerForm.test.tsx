import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError } from "../../api/FakeApiClient";
import { CA_PEM_MAX_BYTES } from "../../api/limits";
import { SESSION_ENDED } from "../../app/leave";
import type { ProfileUnreadable } from "../../generated/ProfileUnreadable";
import type { ServerProfileResponse } from "../../generated/ServerProfileResponse";
import type { ServerProfileState } from "../../generated/ServerProfileState";
import type { SessionPage } from "../../generated/SessionPage";
import { deferred, me, ORIGIN, PROFILE } from "../../test/fixtures";
import { renderApp } from "../../test/renderApp";
import { CERTIFICATE_HELP } from "./certificateHelp";

const NEW_URL = "https://afk.example.com";
const NEW_ORIGIN = "https://afk.example.com/";
const PEM = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
const WITH_CA: ServerProfileState = {
  state: "set",
  profile: {
    url: ORIGIN,
    custom_ca: { certificates: 1, fingerprints: ["AB:CD:EF:01"] },
  },
};
const SWITCHED = {
  change: "switched",
  profile: { url: NEW_ORIGIN },
  logout: { server: "ended" },
} as const;

/** The App on server setup, without a profile (or with `state`). */
async function atSetup(
  handlers: Partial<ApiClient> = {},
  state: ServerProfileState = { state: "none" },
) {
  const app = renderApp({ serverProfile: async () => state, ...handlers });
  await screen.findByRole("heading", { name: "Server setup" });
  return app;
}

/** The App signed in, on the Server screen. */
async function atServer(handlers: Partial<ApiClient> = {}, state: ServerProfileState = PROFILE) {
  const app = renderApp({ serverProfile: async () => state, me: async () => me(), ...handlers });
  await screen.findByText(/Signed in as afkbot1/);
  await app.user.click(screen.getByRole("link", { name: "Server" }));
  await screen.findByRole("heading", { name: "Server" });
  return app;
}

async function typeUrl(app: Awaited<ReturnType<typeof atSetup>>, url: string) {
  const field = screen.getByLabelText("Server address");
  await app.user.clear(field);
  await app.user.type(field, url);
}

const sent = (app: Awaited<ReturnType<typeof atSetup>>) =>
  app.fake.calls.filter((call) => call.method === "setServerProfile").map((call) => call.args[0]);

describe("server setup, signed out", () => {
  it("saves the first server and goes on to the login", async () => {
    const app = await atSetup({ setServerProfile: async () => SWITCHED });

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(sent(app)).toEqual([{ url: NEW_URL }]);
  });

  it.each<[ProfileUnreadable, string]>([
    ["io", "The saved server settings couldn't be read."],
    ["json", "The saved server settings are damaged."],
    ["version", "The saved server settings are from another version of the app."],
    ["url", "The saved server address is no longer valid."],
    ["ca", "The saved custom CA is no longer valid."],
    ["client", "The app can't connect with the saved settings."],
  ])("says why an unreadable profile (%s) needs the server again", async (reason, text) => {
    await atSetup({}, { state: "unreadable", reason });

    expect(screen.getByText(`${text} Enter the server again.`)).toBeDefined();
  });

  it("shows a refused address's rule next to it", async () => {
    await atSetup({
      setServerProfile: async () => {
        throw fakeApiError("invalid_server_url", {
          fields: [{ path: "url", message: "must use https" }],
        });
      },
    }).then(async (app) => {
      await typeUrl(app, "http://example.com");
      await app.user.click(screen.getByRole("button", { name: "Save" }));
    });

    expect(await screen.findByText("must use https")).toBeDefined();
    expect(screen.getByLabelText("Server address").getAttribute("aria-invalid")).toBe("true");
  });

  it("explains a refused certificate", async () => {
    const app = await atSetup({
      setServerProfile: async () => {
        throw fakeApiError("certificate");
      },
    });

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    const form = await screen.findByRole("main");
    expect(await within(form).findByText(CERTIFICATE_HELP)).toBeDefined();
  });

  it("says nothing changed when the server can't be reached", async () => {
    const app = await atSetup({
      setServerProfile: async () => {
        throw fakeApiError("unreachable");
      },
    });

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("fake unreachable Nothing changed.")).toBeDefined();
    expect(app.path()).toBe("/setup");
  });

  it("asks for an address before sending anything", async () => {
    const app = await atSetup();

    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("Enter the server's address.")).toBeDefined();
    expect(sent(app)).toEqual([]);
  });

  it("sends a custom CA pasted into its field", async () => {
    const app = await atSetup({ setServerProfile: async () => SWITCHED });

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByLabelText("Use a custom CA"));
    await app.user.click(screen.getByLabelText("CA certificates (PEM)"));
    await app.user.paste(PEM);
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(sent(app)).toEqual([{ url: NEW_URL, ca_pem: PEM }]));
  });

  it("refuses an empty custom CA", async () => {
    const app = await atSetup();

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByLabelText("Use a custom CA"));
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("Paste or choose the CA's certificates.")).toBeDefined();
    expect(sent(app)).toEqual([]);
  });

  it("fills the CA field from a chosen file", async () => {
    const app = await atSetup();
    await app.user.click(screen.getByLabelText("Use a custom CA"));

    await app.user.upload(screen.getByLabelText("CA file"), new File([PEM], "ca.pem"));

    await waitFor(() =>
      expect((screen.getByLabelText("CA certificates (PEM)") as HTMLTextAreaElement).value).toBe(
        PEM,
      ),
    );
  });

  it("refuses a file larger than a CA can be, without reading it", async () => {
    const app = await atSetup();
    await app.user.click(screen.getByLabelText("Use a custom CA"));
    const large = new File(["x".repeat(CA_PEM_MAX_BYTES + 1)], "huge.pem");
    let read = false;
    Object.defineProperty(large, "text", {
      value: async () => {
        read = true;
        return "";
      },
    });

    fireEvent.change(screen.getByLabelText("CA file"), { target: { files: [large] } });

    expect(
      await screen.findByText("The file is too large for a CA: at most 64 KiB."),
    ).toBeDefined();
    expect(read).toBe(false);
    expect((screen.getByLabelText("CA certificates (PEM)") as HTMLTextAreaElement).value).toBe("");
  });

  it("is reached from the login form and leads back to it", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("no_session");
      },
    });
    await screen.findByRole("heading", { name: "Log in" });

    await app.user.click(screen.getByRole("link", { name: "Server settings" }));
    expect(await screen.findByRole("heading", { name: "Server setup" })).toBeDefined();
    expect((screen.getByLabelText("Server address") as HTMLInputElement).value).toBe(ORIGIN);
    await app.user.click(screen.getByRole("link", { name: "Back to login" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
  });
});

describe("the server settings, signed in", () => {
  it("warns before a save logs out, and Cancel sends nothing", async () => {
    const app = await atServer();

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    expect(
      await screen.findByText(
        "Saving a different server or CA logs you out of https://example.com.",
      ),
    ).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Cancel" }));

    expect(screen.queryByText(/logs you out of/)).toBeNull();
    expect(sent(app)).toEqual([]);
  });

  it("keeps the session when nothing changed", async () => {
    const app = await atServer({
      setServerProfile: async () => ({ change: "none", profile: { url: ORIGIN } }),
    });

    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await app.user.click(await screen.findByRole("button", { name: "Save and log out" }));

    expect(await screen.findByText("Nothing changed; you're still logged in.")).toBeDefined();
    expect(app.path()).toBe("/server");
    expect(app.runtime.stores.session.getState().phase).toBe("signed_in");
  });

  it("logs out quietly after a switch and goes to the login", async () => {
    const app = await atServer({ setServerProfile: async () => SWITCHED });

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await app.user.click(await screen.findByRole("button", { name: "Save and log out" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
    expect(app.runtime.queryClient.getQueryData(["me"])).toBeUndefined();
    expect(screen.queryByText(SESSION_ENDED)).toBeNull();
  });

  it.each(["before", "after"] as const)(
    "is quiet for a call that starts during the switch and answers no_session %s it",
    async (order) => {
      const answer = deferred<ServerProfileResponse>();
      const call = deferred<SessionPage>();
      const app = await atServer({
        setServerProfile: () => answer.promise,
        sessions: () => call.promise,
      });
      await typeUrl(app, NEW_URL);
      await app.user.click(screen.getByRole("button", { name: "Save" }));
      await app.user.click(await screen.findByRole("button", { name: "Save and log out" }));

      const during = app.runtime.apiClient.sessions().catch(() => {});
      await act(async () => {
        if (order === "before") {
          call.reject(fakeApiError("no_session"));
          answer.resolve(SWITCHED);
        } else {
          answer.resolve(SWITCHED);
          call.reject(fakeApiError("no_session"));
        }
        await during;
      });

      await waitFor(() => expect(app.path()).toBe("/login"));
      expect(screen.queryByText(SESSION_ENDED)).toBeNull();
    },
  );

  it("says when the old server couldn't be told", async () => {
    const app = await atServer({
      setServerProfile: async () => ({
        ...SWITCHED,
        logout: {
          server: "unconfirmed",
          error: { code: "unreachable", message: "The server can't be reached." },
        },
      }),
    });

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await app.user.click(await screen.findByRole("button", { name: "Save and log out" }));

    expect(
      await screen.findByText(
        "You're logged out on this PC, but the server couldn't be told (The server can't be reached.), so your session there may stay valid until it expires.",
      ),
    ).toBeDefined();
  });

  it.each(["profile_save", "session_discard"] as const)(
    "goes to the login after %s, which already logged out",
    async (code) => {
      const app = await atServer({
        setServerProfile: async () => {
          throw fakeApiError(code);
        },
      });

      await typeUrl(app, NEW_URL);
      await app.user.click(screen.getByRole("button", { name: "Save" }));
      await app.user.click(await screen.findByRole("button", { name: "Save and log out" }));

      await waitFor(() => expect(app.path()).toBe("/login"));
      expect(
        await screen.findByText(
          `You were logged out, but the new server couldn't be saved: fake ${code} The previous server is still set.`,
        ),
      ).toBeDefined();
      expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
    },
  );

  it("stays signed in when the keychain refuses the new server before anything changed", async () => {
    const app = await atServer({
      setServerProfile: async () => {
        throw fakeApiError("store");
      },
    });

    await typeUrl(app, NEW_URL);
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await app.user.click(await screen.findByRole("button", { name: "Save and log out" }));

    expect(await screen.findByText("fake store")).toBeDefined();
    expect(app.path()).toBe("/server");
    expect(app.runtime.stores.session.getState().phase).toBe("signed_in");
  });

  it("shows the current custom CA and won't save without it again", async () => {
    const app = await atServer({}, WITH_CA);

    expect((screen.getByLabelText("Use a custom CA") as HTMLInputElement).checked).toBe(true);
    expect(screen.getByText("Current custom CA: 1 certificate")).toBeDefined();
    expect(screen.getByText("AB:CD:EF:01")).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("Paste or choose the CA again to keep it")).toBeDefined();
    expect(screen.queryByText(/logs you out of/)).toBeNull();
    expect(sent(app)).toEqual([]);
  });

  it("switches to the system's roots when the custom CA is unchecked", async () => {
    const app = await atServer(
      { setServerProfile: async () => ({ ...SWITCHED, profile: { url: ORIGIN } }) },
      WITH_CA,
    );

    await app.user.click(screen.getByLabelText("Use a custom CA"));
    await app.user.click(screen.getByRole("button", { name: "Save" }));
    await app.user.click(await screen.findByRole("button", { name: "Save and log out" }));

    await waitFor(() => expect(sent(app)).toEqual([{ url: ORIGIN }]));
  });
});
