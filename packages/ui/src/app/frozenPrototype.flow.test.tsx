// The app's webview freezes `Object.prototype` (tauri.conf.json's `freezePrototype`,
// ADR-0017): a library that writes to it, or assigns a shadowing property such as
// `toString` on a plain object, throws there. So a whole flow runs here with it frozen:
// the router, TanStack Query, react-hook-form with zod, zustand, sonner, cn and Radix.
// Vitest runs each test file in its own worker, so the freeze stays in this file; the app's
// modules are imported only after it.
import { describe, expect, it } from "vitest";
import { onHome } from "../test/signedIn";

Object.freeze(Object.prototype);

describe("with a frozen Object.prototype", () => {
  it("runs the launch, the first login with its 2FA setup, Home, a toast and a logout", async () => {
    const { screen, waitFor } = await import("@testing-library/react");
    const { renderApp } = await import("../test/renderApp");
    const { fakeApiError } = await import("../api/FakeApiClient");
    const { me, PROFILE } = await import("../test/fixtures");
    let logins = 0;
    let live = false;
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        if (!live) {
          throw fakeApiError("no_session");
        }
        return me();
      },
      login: async () => {
        logins += 1;
        return logins === 1
          ? { step: "setup_required", expires_in: 600 }
          : { step: "mfa_required", expires_in: 300 };
      },
      mfaEnroll: async () => ({
        otpauth_uri: "otpauth://totp/afkfleet:afkbot1?secret=ABCD",
        qr_svg: "data:image/svg+xml;base64,PHN2Zy8+",
        secret: "ABCDEFGHIJKLMNOPQRST",
      }),
      mfaConfirm: async () => ({ recovery_codes: ["aaaa-bbbb-cccc-dddd-eeee"] }),
      loginMfa: async () => {
        live = true;
      },
      logout: async () => ({
        server: "unconfirmed",
        error: { code: "unreachable", message: "The server can't be reached." },
      }),
    });

    await app.user.type(await screen.findByLabelText("Username"), "afkbot1");
    await app.user.type(screen.getByLabelText("Password"), "placeholder password");
    await app.user.click(screen.getByRole("button", { name: "Log in" }));
    await app.user.type(await screen.findByLabelText("Code"), "654321");
    await app.user.click(screen.getByRole("button", { name: "Confirm" }));
    await app.user.click(await screen.findByLabelText("I've saved these codes"));
    await app.user.click(screen.getByRole("button", { name: "Continue" }));
    await app.user.type(await screen.findByLabelText("Code"), "111222");
    await app.user.click(screen.getByRole("button", { name: "Verify" }));
    await onHome("afkbot1");
    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(await screen.findByText(/You're logged out on this PC/)).toBeDefined();
    expect(Object.isFrozen(Object.prototype)).toBe(true);
  });
});
