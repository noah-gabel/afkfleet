import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError } from "../../api/FakeApiClient";
import { SESSION_ENDED } from "../../app/leave";
import { me } from "../../test/fixtures";
import { fakeTime } from "../../test/renderApp";
import { expectWait, limited } from "../../test/retryWait";
import {
  atLogin,
  CODES,
  cacheText,
  ENROLLMENT,
  enterCode,
  logIn,
  MFA,
  PASSWORD,
  refusal,
  SECRET,
  SETUP,
  WRONG,
} from "../../test/signIn";
import { NEXT_CODE, SETUP_DONE, STEP_ENDED } from "./authFlowReducer";

describe("logging in", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("names the server and links to registering and the server settings", async () => {
    await atLogin();

    expect(screen.getByText("Logging in to https://example.com")).toBeDefined();
    expect(screen.getByRole("link", { name: "Register with an invite" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Server settings" })).toBeDefined();
  });

  it("checks the form before sending anything", async () => {
    const app = await atLogin({ login: async () => MFA });

    await app.user.type(screen.getByLabelText("Username"), "a");
    await app.user.type(screen.getByLabelText("Password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Log in" }));

    expect(await screen.findByText(/must be 3 to 32 letters/)).toBeDefined();
    expect(screen.getByLabelText("Username").getAttribute("aria-invalid")).toBe("true");
    expect(app.fake.calls.map((call) => call.method)).not.toContain("login");
  });

  it("counts a refusal's wait down with Log in disabled, then says to try again", async () => {
    const time = fakeTime();
    const app = await atLogin(
      {
        login: async () => {
          throw limited();
        },
      },
      time.advanceTimers,
    );

    await logIn(app);

    await expectWait(app, time, { button: "Log in", method: "login" });
  });

  it("counts a refusal's wait down at the code step with Verify disabled", async () => {
    const time = fakeTime();
    const app = await atLogin(
      {
        login: async () => MFA,
        loginMfa: async () => {
          throw limited();
        },
      },
      time.advanceTimers,
    );
    await logIn(app);

    await enterCode(app, "123456");

    await expectWait(app, time, { button: "Verify", method: "loginMfa" });
  });

  it("logs in with a code, without its spaces, and opens Home in a new sign-in", async () => {
    const app = await atLogin({ login: async () => MFA });

    await logIn(app);
    expect(await screen.findByText("This step ends in 5 minutes.")).toBeDefined();
    await enterCode(app, " 123 456\t");

    await waitFor(() => expect(app.path()).toBe("/home"));
    expect(app.fake.calls.find((call) => call.method === "loginMfa")?.args).toEqual(["123456"]);
    expect(app.runtime.stores.session.getState()).toMatchObject({
      phase: "signed_in",
      generation: 1,
    });
    expect(await screen.findByText(/Signed in as afkbot1/)).toBeDefined();
  });

  it("counts wrong codes and sends the third back to the password step", async () => {
    const app = await atLogin({
      login: async () => MFA,
      loginMfa: async () => {
        throw WRONG();
      },
    });
    await logIn(app);

    await enterCode(app, "111111");
    expect(await screen.findByText("Wrong code. 2 tries left.")).toBeDefined();
    await enterCode(app, "222222");
    expect(await screen.findByText("Wrong code. 1 try left.")).toBeDefined();
    await enterCode(app, "333333");

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("afkbot1");
    expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe("");
  });

  it("shows a code the server refused as malformed next to it, without counting it", async () => {
    let calls = 0;
    const app = await atLogin({
      login: async () => MFA,
      loginMfa: async () => {
        calls += 1;
        throw calls === 1
          ? refusal(422, "validation_failed", {
              fields: [{ path: "code", message: "must be 6 digits or a recovery code" }],
            })
          : WRONG();
      },
    });
    await logIn(app);

    await enterCode(app, "12345");
    expect(await screen.findByText("must be 6 digits or a recovery code")).toBeDefined();
    await enterCode(app, "123456");

    expect(await screen.findByText("Wrong code. 2 tries left.")).toBeDefined();
  });

  it("ends a step the client no longer holds", async () => {
    const app = await atLogin({
      login: async () => MFA,
      loginMfa: async () => {
        throw fakeApiError("no_pending_step");
      },
    });
    await logIn(app);

    await enterCode(app, "123456");

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
  });

  it("warns in the last minute, then ends the step on its deadline", async () => {
    const time = fakeTime();
    const app = await atLogin({ login: async () => MFA }, time.advanceTimers);
    await logIn(app);
    await screen.findByLabelText("Code");

    await time.advance(238_000);
    const early = screen.queryByText("This step ends in under a minute.");
    await time.advance(4_000);
    expect(await screen.findByText("This step ends in under a minute.")).toBeDefined();
    await time.advance(60_000);

    expect(early).toBeNull();
    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect(app.fake.calls.map((call) => call.method)).not.toContain("loginMfa");
  });

  it("sends nothing for a code entered after the deadline", async () => {
    const time = fakeTime();
    const app = await atLogin({ login: async () => MFA }, time.advanceTimers);
    await logIn(app);
    await screen.findByLabelText("Code");

    vi.setSystemTime(Date.now() + 301_000);
    await enterCode(app, "123456");

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect(app.fake.calls.map((call) => call.method)).not.toContain("loginMfa");
  });

  it("goes back from the code step on Cancel, holding no secret", async () => {
    const app = await atLogin({ login: async () => MFA });
    await logIn(app);
    await screen.findByLabelText("Code");

    await app.user.click(screen.getByRole("button", { name: "Cancel" }));

    expect(await screen.findByRole("heading", { name: "Log in" })).toBeDefined();
    expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe("");
    expect(cacheText(app)).not.toContain(PASSWORD);
  });
});

describe("the first login's 2FA setup", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  function setupHandlers(extra: Partial<ApiClient> = {}): Partial<ApiClient> {
    let logins = 0;
    return {
      login: async () => {
        logins += 1;
        return logins === 1 ? SETUP : MFA;
      },
      mfaEnroll: async () => ENROLLMENT,
      mfaConfirm: async () => ({ recovery_codes: CODES }),
      ...extra,
    };
  }

  it("sets up the authenticator, shows the codes once, then asks for the next code", async () => {
    const app = await atLogin(setupHandlers());
    await logIn(app);

    const qr = (await screen.findByRole("img", {
      name: "QR code for your authenticator app",
    })) as HTMLImageElement;
    expect(qr.getAttribute("src")).toBe(ENROLLMENT.qr_svg);
    expect(screen.getByText("ABCD EFGH IJKL MNOP QRST")).toBeDefined();
    expect(screen.getByText("This step ends in 10 minutes.")).toBeDefined();
    expect(
      screen.getByText(
        "Only the newest QR code works. Delete any entry you scanned from an earlier one in your authenticator app.",
      ),
    ).toBeDefined();
    await enterCode(app, "654 321");
    const list = await screen.findByRole("list", { name: "Recovery codes" });
    expect(
      within(list)
        .getAllByRole("listitem")
        .map((item) => item.textContent),
    ).toEqual(CODES);
    const proceed = screen.getByRole("button", { name: "Continue" });
    expect(proceed.hasAttribute("disabled")).toBe(true);
    await app.user.click(screen.getByLabelText("I've saved these codes"));
    await app.user.click(proceed);
    expect(await screen.findByText(NEXT_CODE)).toBeDefined();
    await enterCode(app, "111222");

    await waitFor(() => expect(app.path()).toBe("/home"));
    const logins = app.fake.calls.filter((call) => call.method === "login");
    expect(logins.map((call) => call.args)).toEqual([
      [{ username: "afkbot1", password: PASSWORD }],
      [{ username: "afkbot1", password: PASSWORD }],
    ]);
    expect(app.fake.calls.find((call) => call.method === "mfaConfirm")?.args).toEqual(["654321"]);
  });

  it("holds no password, TOTP secret or recovery code in the cache after a finished flow", async () => {
    const app = await atLogin(setupHandlers());
    await logIn(app);
    await enterCode(app, "654321");
    await app.user.click(await screen.findByLabelText("I've saved these codes"));
    await app.user.click(screen.getByRole("button", { name: "Continue" }));
    await enterCode(app, "111222");
    await screen.findByText(/Signed in as afkbot1/);

    const cached = cacheText(app);
    for (const secret of [PASSWORD, SECRET, ENROLLMENT.qr_svg, ...CODES]) {
      expect(cached).not.toContain(secret);
    }
  });

  it("holds none after a cancelled setup either", async () => {
    const app = await atLogin(setupHandlers());
    await logIn(app);
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    await app.user.click(screen.getByRole("button", { name: "Cancel" }));
    await screen.findByRole("heading", { name: "Log in" });

    const cached = cacheText(app);
    for (const secret of [PASSWORD, SECRET, ENROLLMENT.qr_svg]) {
      expect(cached).not.toContain(secret);
    }
  });

  it("gets a new QR code on request", async () => {
    let enrolls = 0;
    const app = await atLogin(
      setupHandlers({
        mfaEnroll: async () => {
          enrolls += 1;
          return enrolls === 1 ? ENROLLMENT : { ...ENROLLMENT, secret: "ZYXWVUTSRQPONMLKJIHG" };
        },
      }),
    );
    await logIn(app);
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    await app.user.click(screen.getByRole("button", { name: "New QR code" }));

    expect(await screen.findByText("ZYXW VUTS RQPO NMLK JIHG")).toBeDefined();
  });

  it("refuses to show a QR code that isn't an SVG data URI", async () => {
    const app = await atLogin(
      setupHandlers({
        mfaEnroll: async () => ({ ...ENROLLMENT, qr_svg: "https://example.com/qr.svg" }),
      }),
    );
    await logIn(app);

    expect(
      await screen.findByText(
        "The server's QR code can't be shown. Type the key below into your authenticator app instead.",
      ),
    ).toBeDefined();
    expect(screen.queryByRole("img", { name: "QR code for your authenticator app" })).toBeNull();
  });

  it("keeps the setup step after a wrong confirming code", async () => {
    let confirms = 0;
    const app = await atLogin(
      setupHandlers({
        mfaConfirm: async () => {
          confirms += 1;
          if (confirms === 1) {
            throw refusal(422, "validation_failed", {
              fields: [{ path: "code", message: "doesn't match the authenticator" }],
            });
          }
          return { recovery_codes: CODES };
        },
      }),
    );
    await logIn(app);

    await enterCode(app, "000000");
    expect(await screen.findByText("doesn't match the authenticator")).toBeDefined();
    await enterCode(app, "654321");

    expect(await screen.findByRole("list", { name: "Recovery codes" })).toBeDefined();
  });

  it("ends the setup step on its 10-minute deadline", async () => {
    const time = fakeTime();
    const app = await atLogin(setupHandlers(), time.advanceTimers);
    await logIn(app);
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    await time.advance(600_500);

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
  });

  it("copies all codes and says where the copy may linger", async () => {
    const app = await atLogin(setupHandlers());
    await logIn(app);
    await enterCode(app, "654321");

    await app.user.click(await screen.findByRole("button", { name: "Copy all" }));

    expect(await navigator.clipboard.readText()).toBe(CODES.join("\n"));
    expect(
      await screen.findByText(
        "Recovery codes copied. Paste them into a password manager or another safe place; Windows' clipboard history may keep a copy.",
      ),
    ).toBeDefined();
  });

  it("says so when copying fails", async () => {
    const app = await atLogin(setupHandlers());
    await logIn(app);
    await enterCode(app, "654321");
    vi.spyOn(navigator.clipboard, "writeText").mockRejectedValue(new Error("denied"));

    await app.user.click(await screen.findByRole("button", { name: "Copy all" }));

    expect(
      await screen.findByText("Couldn't copy. Select the codes and copy them by hand."),
    ).toBeDefined();
  });

  it("goes back to the password step when the login after the codes fails", async () => {
    const time = fakeTime();
    let logins = 0;
    const app = await atLogin(
      setupHandlers({
        login: async () => {
          logins += 1;
          if (logins === 1) {
            return SETUP;
          }
          throw limited();
        },
      }),
      time.advanceTimers,
    );
    await logIn(app);
    await enterCode(app, "654321");
    await app.user.click(await screen.findByLabelText("I've saved these codes"));

    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText(SETUP_DONE)).toBeDefined();
    expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("afkbot1");
    expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe("");
    await expectWait(app, time, { button: "Log in", method: "login" });
  });

  it("counts a refusal's wait down at the setup step with Confirm disabled", async () => {
    const time = fakeTime();
    const app = await atLogin(
      setupHandlers({
        mfaConfirm: async () => {
          throw limited();
        },
      }),
      time.advanceTimers,
    );
    await logIn(app);

    await enterCode(app, "654321");

    await expectWait(app, time, { button: "Confirm", method: "mfaConfirm" });
  });
});

describe("a login while a session is still held", () => {
  const HELD = async (): Promise<never> => {
    throw fakeApiError("already_logged_in");
  };

  it("opens Home when it's the same user, in a new sign-in", async () => {
    const app = await atLogin({ login: HELD });
    app.session.live = true;

    await logIn(app, "AfkBot1");

    await waitFor(() => expect(app.path()).toBe("/home"));
    expect(app.runtime.stores.session.getState()).toMatchObject({ phase: "signed_in" });
  });

  it("offers to continue as the other user, whose session then ends like any other", async () => {
    const app = await atLogin({
      login: HELD,
      sessions: async () => {
        throw fakeApiError("no_session");
      },
    });
    app.session.live = true;
    Object.assign(app.fake, { me: async () => me("member", "otherbot") });

    await logIn(app);
    expect(await screen.findByText("You're still signed in as otherbot.")).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Continue as otherbot" }));
    await waitFor(() => expect(app.path()).toBe("/home"));
    await act(() => app.runtime.apiClient.sessions().catch(() => {}));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(await screen.findByText(SESSION_ENDED)).toBeDefined();
  });

  it("logs the other session out on request, and the form works again", async () => {
    const app = await atLogin({ login: HELD, logout: async () => ({ server: "ended" }) });
    app.session.live = true;
    Object.assign(app.fake, { me: async () => me("member", "otherbot") });
    await logIn(app);

    await app.user.click(await screen.findByRole("button", { name: "Log out" }));

    expect(await screen.findByRole("button", { name: "Log in" })).toBeDefined();
    expect(app.fake.calls.map((call) => call.method)).toContain("logout");
    expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe("");
  });

  it("offers a logout when the held session can't be checked", async () => {
    const app = await atLogin({ login: HELD });
    Object.assign(app.fake, {
      me: async () => {
        throw fakeApiError("store");
      },
    });

    await logIn(app);

    expect(await screen.findByText("fake store")).toBeDefined();
    expect(screen.getByRole("button", { name: "Log out" })).toBeDefined();
    expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe("");
  });
});
