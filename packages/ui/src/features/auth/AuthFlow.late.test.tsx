// The sign-in flow against slow and late answers: an answer counts only for the step that
// asked, the step's controls wait while a call runs, and every way in starts a new
// sign-in with a fresh `me`.
import { act, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeApiError } from "../../api/FakeApiClient";
import { SESSION_ENDED } from "../../app/leave";
import type { LoginStepResponse } from "../../generated/LoginStepResponse";
import type { LogoutResponse } from "../../generated/LogoutResponse";
import type { MeResponse } from "../../generated/MeResponse";
import type { MfaConfirmResponse } from "../../generated/MfaConfirmResponse";
import type { MfaEnrollResponse } from "../../generated/MfaEnrollResponse";
import type { SessionPage } from "../../generated/SessionPage";
import { deferred, me, PROFILE } from "../../test/fixtures";
import { fakeTime, renderApp } from "../../test/renderApp";
import {
  atLogin,
  CODES,
  ENROLLMENT,
  enterCode,
  logIn,
  MFA,
  PASSWORD,
  SETUP,
  WRONG,
} from "../../test/signIn";
import { REGISTERED, STEP_ENDED } from "./authFlowReducer";

const HELD = async (): Promise<never> => {
  throw fakeApiError("already_logged_in");
};

afterEach(() => {
  vi.useRealTimers();
});

describe("a late answer", () => {
  it("never signs in after its code step ended", async () => {
    const time = fakeTime();
    const answer = deferred<void>();
    const app = await atLogin(
      { login: async () => MFA, loginMfa: () => answer.promise },
      time.advanceTimers,
    );
    await logIn(app);
    await enterCode(app, "123456");

    await time.advance(301_000);
    await screen.findByText(STEP_ENDED);
    // The Rust side adopted the session the late answer started.
    app.session.live = true;
    await act(async () => answer.resolve());
    await time.advance(1_000);

    expect(app.path()).toBe("/login");
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
  });

  it("never signs in after the user left the route", async () => {
    const answer = deferred<void>();
    const app = await atLogin({ login: async () => MFA, loginMfa: () => answer.promise });
    await logIn(app);
    await enterCode(app, "123456");

    await act(() => app.runtime.router.navigate({ to: "/setup" }));
    await act(async () => answer.resolve());

    await waitFor(() => expect(app.path()).toBe("/setup"));
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
  });

  it("shows no recovery codes after its setup step ended", async () => {
    const time = fakeTime();
    const answer = deferred<MfaConfirmResponse>();
    const app = await atLogin(
      {
        login: async () => SETUP,
        mfaEnroll: async () => ENROLLMENT,
        mfaConfirm: () => answer.promise,
      },
      time.advanceTimers,
    );
    await logIn(app);
    await enterCode(app, "654321");

    await time.advance(601_000);
    await screen.findByText(STEP_ENDED);
    await act(async () => answer.resolve({ recovery_codes: CODES }));
    await time.advance(1_000);

    expect(screen.queryByRole("list", { name: "Recovery codes" })).toBeNull();
    expect(screen.getByRole("heading", { name: "Log in" })).toBeDefined();
  });

  it("doesn't count a wrong code in a newer code step", async () => {
    const time = fakeTime();
    const first = deferred<void>();
    let calls = 0;
    const app = await atLogin(
      {
        login: async () => MFA,
        loginMfa: () => {
          calls += 1;
          return calls === 1 ? first.promise : Promise.reject(WRONG());
        },
      },
      time.advanceTimers,
    );
    await logIn(app);
    await enterCode(app, "111111");
    await time.advance(301_000);
    await screen.findByText(STEP_ENDED);
    await logIn(app);
    await screen.findByLabelText("Code");

    await act(async () => first.reject(WRONG()));
    await time.advance(1_000);

    expect(screen.queryByText(/Wrong code/)).toBeNull();
  });

  it("doesn't navigate when a me() check answers after the user went to the server settings", async () => {
    const check = deferred<MeResponse>();
    const app = await atLogin({ login: HELD });
    Object.assign(app.fake, { me: () => check.promise });
    await logIn(app);

    await act(() => app.runtime.router.navigate({ to: "/setup" }));
    await act(async () => check.resolve(me()));

    await waitFor(() => expect(app.path()).toBe("/setup"));
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
  });
});

describe("while a call runs", () => {
  it("keeps the password step's links from leaving", async () => {
    const answer = deferred<LoginStepResponse>();
    const app = await atLogin({ login: () => answer.promise });
    await logIn(app);

    await app.user.click(screen.getByRole("link", { name: "Server settings" }));
    await app.user.click(screen.getByRole("link", { name: "Register with an invite" }));

    expect(app.path()).toBe("/login");
    expect(
      screen.getByRole("link", { name: "Server settings" }).getAttribute("aria-disabled"),
    ).toBe("true");
  });

  it("disables Cancel while a code is checked", async () => {
    const app = await atLogin({ login: async () => MFA, loginMfa: () => new Promise(() => {}) });
    await logIn(app);

    await enterCode(app, "123456");

    expect(screen.getByRole("button", { name: "Cancel" }).hasAttribute("disabled")).toBe(true);
  });

  it("sends one code for a double click", async () => {
    const app = await atLogin({ login: async () => MFA, loginMfa: () => new Promise(() => {}) });
    await logIn(app);
    await app.user.type(await screen.findByLabelText("Code"), "123456");

    await app.user.dblClick(screen.getByRole("button", { name: "Verify" }));

    expect(app.fake.calls.filter((call) => call.method === "loginMfa")).toHaveLength(1);
  });

  it("logs in once for a double click on the recovery codes' Continue", async () => {
    let logins = 0;
    const app = await atLogin({
      login: () => {
        logins += 1;
        return logins === 1 ? Promise.resolve(SETUP) : new Promise(() => {});
      },
      mfaEnroll: async () => ENROLLMENT,
      mfaConfirm: async () => ({ recovery_codes: CODES }),
    });
    await logIn(app);
    await enterCode(app, "654321");
    await app.user.click(await screen.findByLabelText("I've saved these codes"));

    await app.user.dblClick(screen.getByRole("button", { name: "Continue" }));

    expect(logins).toBe(2);
  });

  it("disables Log out and Continue as while a logout runs", async () => {
    const logout = deferred<LogoutResponse>();
    const app = await atLogin({ login: HELD, logout: () => logout.promise });
    app.session.live = true;
    Object.assign(app.fake, { me: async () => me("member", "otherbot") });
    await logIn(app);

    await app.user.click(await screen.findByRole("button", { name: "Log out" }));
    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    expect(screen.getByRole("button", { name: "Log out" }).hasAttribute("disabled")).toBe(true);
    expect(
      screen.getByRole("button", { name: "Continue as otherbot" }).hasAttribute("disabled"),
    ).toBe(true);
    expect(app.fake.calls.filter((call) => call.method === "logout")).toHaveLength(1);
  });

  it("disables the notice's Log out while it runs", async () => {
    const logout = deferred<LogoutResponse>();
    const app = await atLogin({ login: HELD, logout: () => logout.promise });
    Object.assign(app.fake, {
      me: async () => {
        throw fakeApiError("store");
      },
    });
    await logIn(app);

    await app.user.click(await screen.findByRole("button", { name: "Log out" }));
    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    expect(screen.getByRole("button", { name: "Log out" }).hasAttribute("disabled")).toBe(true);
    expect(app.fake.calls.filter((call) => call.method === "logout")).toHaveLength(1);
  });

  it("disables Continue as while it signs in, so a second click does nothing", async () => {
    const app = await atLogin({ login: HELD });
    app.session.live = true;
    Object.assign(app.fake, { me: async () => me("member", "otherbot") });
    await logIn(app);
    const refresh = deferred<void>();
    const invalidate = vi
      .spyOn(app.runtime.queryClient, "invalidateQueries")
      .mockImplementation(() => refresh.promise);
    const button = await screen.findByRole("button", { name: "Continue as otherbot" });

    await app.user.click(button);
    await app.user.click(button);
    const disabled = button.hasAttribute("disabled");
    await act(async () => refresh.resolve());

    await waitFor(() => expect(app.path()).toBe("/home"));
    expect(disabled).toBe(true);
    expect(invalidate).toHaveBeenCalledTimes(1);
    expect(app.runtime.stores.session.getState().generation).toBe(1);
  });

  it("hides the old secret and disables New QR code while a new one loads", async () => {
    const second = deferred<MfaEnrollResponse>();
    let enrolls = 0;
    const app = await atLogin({
      login: async () => SETUP,
      mfaEnroll: () => {
        enrolls += 1;
        return enrolls === 1 ? Promise.resolve(ENROLLMENT) : second.promise;
      },
    });
    await logIn(app);
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    await app.user.click(screen.getByRole("button", { name: "New QR code" }));

    expect(screen.queryByText("ABCD EFGH IJKL MNOP QRST")).toBeNull();
    expect(screen.queryByRole("img", { name: "QR code for your authenticator app" })).toBeNull();
    expect(screen.getByRole("button", { name: "New QR code" }).hasAttribute("disabled")).toBe(true);
    await act(async () => second.resolve({ ...ENROLLMENT, secret: "ZYXWVUTSRQPONMLKJIHG" }));
    expect(await screen.findByText("ZYXW VUTS RQPO NMLK JIHG")).toBeDefined();
  });
});

describe("every way in", () => {
  it("can't be ended by a call that started before Continue as", async () => {
    const early = deferred<SessionPage>();
    const app = await atLogin({ login: HELD, sessions: () => early.promise });
    app.session.live = true;
    Object.assign(app.fake, { me: async () => me("member", "otherbot") });
    await logIn(app);
    await screen.findByRole("button", { name: "Continue as otherbot" });

    const call = app.runtime.apiClient.sessions().catch(() => {});
    await app.user.click(screen.getByRole("button", { name: "Continue as otherbot" }));
    await waitFor(() => expect(app.path()).toBe("/home"));
    early.reject(fakeApiError("no_session"));
    await act(() => call);

    expect(app.path()).toBe("/home");
    expect(screen.queryByText(SESSION_ENDED)).toBeNull();
  });

  it.each([
    ["the code step", "code"],
    ["the same user's held session", "same"],
    ["Continue as", "other"],
  ] as const)("invalidates me after %s", async (_, way) => {
    const app = await atLogin({ login: way === "code" ? async () => MFA : HELD });
    if (way !== "code") {
      app.session.live = true;
      Object.assign(app.fake, {
        me: async () => me("member", way === "same" ? "afkbot1" : "otherbot"),
      });
    }
    const invalidate = vi.spyOn(app.runtime.queryClient, "invalidateQueries");
    await logIn(app);
    if (way === "code") {
      await enterCode(app, "123456");
    } else if (way === "other") {
      await app.user.click(await screen.findByRole("button", { name: "Continue as otherbot" }));
    }

    await waitFor(() => expect(app.path()).toBe("/home"));
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ["me"] });
  });

  it("sends no setup code after the setup step's deadline", async () => {
    const time = fakeTime();
    const app = await atLogin(
      { login: async () => SETUP, mfaEnroll: async () => ENROLLMENT },
      time.advanceTimers,
    );
    await logIn(app);
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    vi.setSystemTime(Date.now() + 601_000);
    await enterCode(app, "654321");

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect(app.fake.calls.map((call) => call.method)).not.toContain("mfaConfirm");
  });

  it("starts a step's deadline when its request was sent", async () => {
    const time = fakeTime();
    const answer = deferred<LoginStepResponse>();
    const app = await atLogin({ login: () => answer.promise }, time.advanceTimers);
    await logIn(app);

    await time.advance(100_000);
    await act(async () => answer.resolve(MFA));
    await screen.findByLabelText("Code");
    await time.advance(201_000);

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
  });

  it("requests the setup secret once per step, also under StrictMode", async () => {
    const app = await atLogin(
      { login: async () => SETUP, mfaEnroll: async () => ENROLLMENT },
      undefined,
      true,
    );
    await logIn(app);

    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    expect(app.fake.calls.filter((call) => call.method === "mfaEnroll")).toHaveLength(1);
  });
});

describe("a failed login after registering", () => {
  it("lands on /login, whose Register link works", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("no_session");
      },
      register: async () => ({
        id: "01900000-0000-7000-8000-000000000002",
        username: "newbot",
        role: "member",
      }),
      login: async () => {
        throw fakeApiError("unreachable");
      },
    });
    await screen.findByRole("heading", { name: "Log in" });
    await app.user.click(screen.getByRole("link", { name: "Register with an invite" }));
    await app.user.click(await screen.findByLabelText("Invite code"));
    await app.user.paste("afk_iv_0123456789abcdefghijklmnopqrstuvwxyzABCDE");
    await app.user.type(screen.getByLabelText("Username"), "NewBot");
    await app.user.type(screen.getByLabelText("Password"), PASSWORD);
    await app.user.type(screen.getByLabelText("Repeat password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Register" }));

    expect(await screen.findByText(REGISTERED)).toBeDefined();
    await waitFor(() => expect(app.path()).toBe("/login"));
    expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("NewBot");
    await app.user.click(screen.getByRole("link", { name: "Register with an invite" }));

    expect(await screen.findByRole("heading", { name: "Register" })).toBeDefined();
  });
});
