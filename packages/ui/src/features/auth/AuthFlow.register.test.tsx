import { screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError } from "../../api/FakeApiClient";
import type { ApiErrorDetails } from "../../generated/ApiErrorDetails";
import { PROFILE } from "../../test/fixtures";
import { fakeTime, renderApp } from "../../test/renderApp";
import { expectWait, limited } from "../../test/retryWait";
import { REGISTERED } from "./authFlowReducer";

const PASSWORD = "placeholder password";
const INVITE = "afk_iv_0123456789abcdefghijklmnopqrstuvwxyzABCDE";

function refusal(status: number, code: string, extra: Partial<ApiErrorDetails> = {}) {
  return fakeApiError("api", {
    api: {
      status,
      code: code as ApiErrorDetails["code"],
      message: `refused: ${code}`,
      request_id: "01900000-0000-7000-8000-000000000009",
      ...extra,
    },
  });
}

/** The App on the register form, reached from the login form. */
async function atRegister(
  handlers: Partial<ApiClient> = {},
  advanceTimers?: (ms: number) => unknown,
) {
  const app = renderApp(
    {
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("no_session");
      },
      register: async () => ({
        id: "01900000-0000-7000-8000-000000000002",
        username: "newbot",
        role: "member",
      }),
      login: async () => ({ step: "setup_required", expires_in: 600 }),
      mfaEnroll: async () => ({
        otpauth_uri: "otpauth://totp/afkfleet:newbot?secret=ABCD",
        qr_svg: "data:image/svg+xml;base64,PHN2Zy8+",
        secret: "ABCDEFGHIJKLMNOPQRST",
      }),
      ...handlers,
    },
    advanceTimers === undefined ? {} : { advanceTimers },
  );
  await screen.findByRole("heading", { name: "Log in" });
  await app.user.click(screen.getByRole("link", { name: "Register with an invite" }));
  await screen.findByRole("heading", { name: "Register" });
  return app;
}

async function fill(
  app: Awaited<ReturnType<typeof atRegister>>,
  invite: string,
  repeat = PASSWORD,
) {
  await app.user.click(screen.getByLabelText("Invite code"));
  await app.user.paste(invite);
  await app.user.type(screen.getByLabelText("Username"), "NewBot");
  await app.user.type(screen.getByLabelText("Password"), PASSWORD);
  await app.user.type(screen.getByLabelText("Repeat password"), repeat);
  await app.user.click(screen.getByRole("button", { name: "Register" }));
}

describe("registering with an invite", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("names the server and links back to the login", async () => {
    await atRegister();

    expect(screen.getByText("Registering on https://example.com")).toBeDefined();
    expect(screen.getByRole("link", { name: "Back to login" })).toBeDefined();
  });

  it("trims a pasted invite, registers, logs in and goes straight to the 2FA setup", async () => {
    const app = await atRegister();

    await fill(app, `  ${INVITE}\r\n`);

    expect(await screen.findByText("ABCD EFGH IJKL MNOP QRST")).toBeDefined();
    expect(app.fake.calls.find((call) => call.method === "register")?.args).toEqual([
      { invite: INVITE, username: "NewBot", password: PASSWORD },
    ]);
    expect(app.fake.calls.find((call) => call.method === "login")?.args).toEqual([
      { username: "NewBot", password: PASSWORD },
    ]);
  });

  it("refuses a repeat that doesn't match, sending nothing", async () => {
    const app = await atRegister();

    await fill(app, INVITE, "another placeholder");

    expect(await screen.findByText("The passwords don't match.")).toBeDefined();
    expect(app.fake.calls.map((call) => call.method)).not.toContain("register");
  });

  it("checks the new password's length before sending", async () => {
    const app = await atRegister();

    await app.user.click(screen.getByLabelText("Invite code"));
    await app.user.paste(INVITE);
    await app.user.type(screen.getByLabelText("Username"), "newbot");
    await app.user.type(screen.getByLabelText("Password"), "short");
    await app.user.type(screen.getByLabelText("Repeat password"), "short");
    await app.user.click(screen.getByRole("button", { name: "Register" }));

    expect(await screen.findByText("must be 12 to 128 characters")).toBeDefined();
    expect(app.fake.calls.map((call) => call.method)).not.toContain("register");
  });

  it("shows the server's refusals next to their fields", async () => {
    const app = await atRegister({
      register: async () => {
        throw refusal(422, "validation_failed", {
          fields: [
            { path: "invite", message: "isn't a usable invite" },
            { path: "password", message: "is too easy to guess" },
          ],
        });
      },
    });

    await fill(app, INVITE);

    expect(await screen.findByText("isn't a usable invite")).toBeDefined();
    expect(screen.getByText("is too easy to guess")).toBeDefined();
    expect(screen.getByLabelText("Password").getAttribute("aria-invalid")).toBe("true");
  });

  it("counts a refusal's wait down with Register disabled", async () => {
    const time = fakeTime();
    const app = await atRegister(
      {
        register: async () => {
          throw limited();
        },
      },
      time.advanceTimers,
    );

    await fill(app, INVITE);

    await expectWait(app, time, { button: "Register", method: "register" });
  });

  it("counts the wait down on the login form after a rate limit on the login that follows", async () => {
    const time = fakeTime();
    const app = await atRegister(
      {
        login: async () => {
          throw limited();
        },
      },
      time.advanceTimers,
    );

    await fill(app, INVITE);

    expect(await screen.findByText(REGISTERED)).toBeDefined();
    await waitFor(() => expect(app.path()).toBe("/login"));
    await expectWait(app, time, { button: "Log in", method: "login" });
  });

  it.each([["an unreachable server", () => fakeApiError("unreachable"), "fake unreachable"]])(
    "sends the user to the login form after %s on the login that follows",
    async (_, failure, reason) => {
      const app = await atRegister({
        login: async () => {
          throw failure();
        },
      });

      await fill(app, INVITE);

      expect(await screen.findByText(REGISTERED)).toBeDefined();
      expect(screen.getByText(reason)).toBeDefined();
      expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("NewBot");
      expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe("");
      await waitFor(() => expect(app.path()).toBe("/login"));
    },
  );

  it("shows both passwords on request, each with its own toggle", async () => {
    const app = await atRegister();

    await app.user.click(screen.getByRole("button", { name: "Show password" }));
    await app.user.click(screen.getByRole("button", { name: "Show repeated password" }));

    expect(screen.getByLabelText("Password").getAttribute("type")).toBe("text");
    expect(screen.getByLabelText("Repeat password").getAttribute("type")).toBe("text");
  });
});
