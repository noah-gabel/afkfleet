// The code step's six boxes (Plan.md P11.11; ADR-0020, U86, U99, S100), through the sign-in flow,
// counting what reaches the API: a wrong code costs one of three tries, so what's sent matters.
import { act, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeApiError } from "../../api/FakeApiClient";
import { fakeTime } from "../../test/renderApp";
import { limited } from "../../test/retryWait";
import {
  atLogin,
  cacheText,
  ENROLLMENT,
  logIn,
  MFA,
  PASSWORD,
  refusal,
  SETUP,
  WRONG,
} from "../../test/signIn";
import { ALL_SIX, DIGITS_ONLY, NOT_A_CODE } from "./schemas";

const RECOVERY = "abcd-efgh-ijkl-mnop-qrst";

/** The App at the code step, its `loginMfa` never answering unless given. */
async function atCode(loginMfa: () => Promise<void> = () => new Promise(() => {}), strict = false) {
  const app = await atLogin({ login: async () => MFA, loginMfa }, undefined, strict);
  await logIn(app);
  const field = (await screen.findByLabelText("Code")) as HTMLInputElement;
  const sent = () =>
    app.fake.calls.filter((call) => call.method === "loginMfa").map((call) => call.args[0]);
  return { app, field, sent };
}

/** The status region that says what the boxes left out. */
function refusalStatus(): HTMLElement {
  const status = document.querySelector<HTMLElement>("[data-slot=code-refusal]");
  if (status === null) {
    throw new Error("no refusal status");
  }
  expect(status.getAttribute("role")).toBe("status");
  return status;
}

/** Counts what changes inside `status` from now on, as a screen reader would hear it. */
function changesTo(status: HTMLElement): () => number {
  let changes = 0;
  const observer = new MutationObserver((records) => {
    changes += records.length;
  });
  observer.observe(status, { childList: true, subtree: true, characterData: true });
  return () => {
    changes += observer.takeRecords().length;
    return changes;
  };
}

describe("the code step's boxes", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("are one field with one name, and take the focus on arrival", async () => {
    const { field } = await atCode();

    expect(screen.getAllByLabelText("Code")).toHaveLength(1);
    expect(field.getAttribute("autocomplete")).toBe("one-time-code");
    expect(field.getAttribute("inputmode")).toBe("numeric");
    expect(document.activeElement).toBe(field);
  });

  it("say they send when full, and how many tries are left", async () => {
    const { field } = await atCode();

    const described = (field.getAttribute("aria-describedby") ?? "")
      .split(" ")
      .map((id) => document.getElementById(id)?.textContent);
    expect(described).toContain("6 digits from your app. Sends when full.");
    expect(described).toContain("3 tries left.");
  });

  it("wait for the sixth digit, and send once", async () => {
    const { app, field, sent } = await atCode();

    await app.user.type(field, "12345");
    expect(sent()).toEqual([]);
    await app.user.type(field, "6");

    expect(field.value).toBe("123456");
    expect(sent()).toEqual(["123456"]);
  });

  it("send once under StrictMode", async () => {
    const { app, field, sent } = await atCode(undefined, true);

    await app.user.type(field, "123456");

    expect(sent()).toEqual(["123456"]);
  });

  it("ignore a letter, and point to the recovery link", async () => {
    const { app, field, sent } = await atCode();

    await app.user.type(field, "12a");

    expect(field.value).toBe("12");
    expect(screen.getByText(DIGITS_ONLY).closest("[role=status]")).not.toBeNull();
    expect(sent()).toEqual([]);
    await app.user.type(field, "3");
    expect(screen.queryByText(DIGITS_ONLY)).toBeNull();
  });

  it("keep the refusal's status mounted before there's anything to say", async () => {
    const { app, field } = await atCode();
    const status = refusalStatus();
    expect(status.textContent).toBe("");

    await app.user.type(field, "a");

    expect(status.isConnected).toBe(true);
    expect(status.textContent).toBe(DIGITS_ONLY);
    await app.user.type(field, "1");
    expect(status.isConnected).toBe(true);
    expect(status.textContent).toBe("");
  });

  it("say the same refusal again at a second letter", async () => {
    const { app, field } = await atCode();
    const status = refusalStatus();
    await app.user.type(field, "a");
    const changes = changesTo(status);

    await app.user.type(field, "b");

    expect(status.textContent).toBe(DIGITS_ONLY);
    expect(changes()).toBeGreaterThan(0);
  });

  it.each([" ", "-"])("ignore %j without a word", async (typed) => {
    const { app, field } = await atCode();

    await app.user.type(field, `12${typed}`);

    expect(field.value).toBe("12");
    expect(screen.queryByText(DIGITS_ONLY)).toBeNull();
    expect(screen.queryByText(NOT_A_CODE)).toBeNull();
  });

  it.each(["123456", "123 456", "123-456", " 123456\n"])(
    "fill from a pasted %j and send once",
    async (pasted) => {
      const { app, field, sent } = await atCode();

      await app.user.click(field);
      await app.user.paste(pasted);

      expect(field.value).toBe("123456");
      expect(sent()).toEqual(["123456"]);
    },
  );

  it("replace what's there with a whole pasted code", async () => {
    const { app, field, sent } = await atCode();

    await app.user.type(field, "98");
    await app.user.paste("123456");

    expect(field.value).toBe("123456");
    expect(sent()).toEqual(["123456"]);
  });

  it("add a part pasted at the caret, and send if that fills them", async () => {
    const { app, field, sent } = await atCode();

    await app.user.type(field, "12");
    await app.user.paste("34");
    expect(field.value).toBe("1234");
    expect(sent()).toEqual([]);
    await app.user.paste("56");

    expect(sent()).toEqual(["123456"]);
  });

  it.each(["1234567", "12.34.56", "code: 123456"])(
    "leave a pasted %j out, sending nothing, as filtering it could cost a try",
    async (pasted) => {
      const { app, field, sent } = await atCode();

      await app.user.type(field, "12");
      await app.user.paste(pasted);

      expect(field.value).toBe("12");
      expect(screen.getByText(NOT_A_CODE)).toBeDefined();
      expect(sent()).toEqual([]);
    },
  );

  it("swap to the text field for a pasted recovery code, filled, sending nothing", async () => {
    const { app, field, sent } = await atCode();

    await app.user.click(field);
    await app.user.paste(RECOVERY);

    const recovery = (await screen.findByLabelText("Recovery code")) as HTMLInputElement;
    expect(recovery.value).toBe(RECOVERY);
    expect(document.activeElement).toBe(recovery);
    expect(sent()).toEqual([]);
  });

  it("clear after a wrong code, keep the focus, say the tries, and send the next", async () => {
    const { app, field, sent } = await atCode(async () => {
      throw WRONG();
    });

    await app.user.type(field, "111111");

    expect((await screen.findByText("Wrong code. 2 tries left.")).getAttribute("role")).toBe(
      "alert",
    );
    await waitFor(() => expect(field.value).toBe(""));
    expect(document.activeElement).toBe(field);
    expect(screen.queryByText("3 tries left.")).toBeNull();

    await app.user.type(field, "222222");

    expect(await screen.findByText("Wrong code. 1 try left.")).toBeDefined();
    expect(sent()).toEqual(["111111", "222222"]);
  });

  it("keep the digits after a connection failure, as no try was spent", async () => {
    let calls = 0;
    const { app, field, sent } = await atCode(async () => {
      calls += 1;
      if (calls === 1) {
        throw fakeApiError("unreachable");
      }
      throw WRONG();
    });

    await app.user.type(field, "123456");

    expect(await screen.findByText(/fake unreachable/)).toBeDefined();
    expect(field.value).toBe("123456");
    expect(sent()).toEqual(["123456"]);

    await app.user.click(screen.getByRole("button", { name: "Verify" }));

    expect(await screen.findByText("Wrong code. 2 tries left.")).toBeDefined();
    expect(sent()).toEqual(["123456", "123456"]);
  });

  it("send again on Enter after a connection failure", async () => {
    let calls = 0;
    const { app, field, sent } = await atCode(async () => {
      calls += 1;
      if (calls === 1) {
        throw fakeApiError("timed_out");
      }
      throw WRONG();
    });

    await app.user.type(field, "123456");
    await screen.findByText(/fake timed_out/);
    await app.user.click(field);
    await app.user.keyboard("{Enter}");

    await screen.findByText("Wrong code. 2 tries left.");
    expect(sent()).toHaveLength(2);
  });

  it("take the focus back after Verify's resend fails", async () => {
    const { app, field, sent } = await atCode(async () => {
      throw fakeApiError("unreachable");
    });
    await app.user.type(field, "123456");
    await screen.findByText(/fake unreachable/);

    await app.user.click(screen.getByRole("button", { name: "Verify" }));

    await waitFor(() => expect(sent()).toHaveLength(2));
    await waitFor(() => expect(document.activeElement).toBe(field));
  });

  it("take the focus back after Verify's resend is limited", async () => {
    let calls = 0;
    const { app, field } = await atCode(async () => {
      calls += 1;
      if (calls === 1) {
        throw fakeApiError("unreachable");
      }
      throw limited(30);
    });
    await app.user.type(field, "123456");
    await screen.findByText(/fake unreachable/);

    await app.user.click(screen.getByRole("button", { name: "Verify" }));

    await screen.findByText("Try again in 30 seconds.");
    expect(screen.getByRole("button", { name: "Verify" }).hasAttribute("disabled")).toBe(true);
    expect(document.activeElement).toBe(field);
  });

  it("take the focus back to the recovery field after Verify's send fails", async () => {
    const { app } = await atCode(async () => {
      throw fakeApiError("unreachable");
    });
    await app.user.click(screen.getByRole("button", { name: "Use a recovery code" }));
    const recovery = await screen.findByLabelText("Recovery code");
    await app.user.type(recovery, RECOVERY);

    await app.user.click(screen.getByRole("button", { name: "Verify" }));

    await screen.findByText(/fake unreachable/);
    await waitFor(() => expect(document.activeElement).toBe(recovery));
  });

  it("ask for all six when Verify comes early, sending nothing", async () => {
    const { app, field, sent } = await atCode();

    await app.user.type(field, "123");
    await app.user.click(screen.getByRole("button", { name: "Verify" }));

    expect((await screen.findByRole("alert")).textContent).toBe(ALL_SIX);
    expect(sent()).toEqual([]);
    await app.user.type(field, "4");
    expect(screen.queryByText(ALL_SIX)).toBeNull();
  });

  it("show a code the server refuses as malformed, without counting it", async () => {
    const { app, field } = await atCode(async () => {
      throw refusal(422, "validation_failed", {
        fields: [{ path: "code", message: "must be 6 digits or a recovery code" }],
      });
    });

    await app.user.type(field, "123456");

    expect((await screen.findByRole("alert")).textContent).toBe(
      "must be 6 digits or a recovery code",
    );
    expect(screen.getByText("3 tries left.")).toBeDefined();
  });

  it("clear and lock through a 429's wait, keep the focus, and send nothing at its end", async () => {
    const time = fakeTime();
    let calls = 0;
    const app = await atLogin(
      {
        login: async () => MFA,
        loginMfa: async () => {
          calls += 1;
          throw limited(2);
        },
      },
      time.advanceTimers,
    );
    await logIn(app);
    const field = (await screen.findByLabelText("Code")) as HTMLInputElement;

    await app.user.type(field, "123456");

    await screen.findByText(/Try again in 2 s\./);
    expect(field.value).toBe("");
    expect(field.readOnly).toBe(true);
    expect(document.activeElement).toBe(field);
    expect(calls).toBe(1);

    await act(() => vi.advanceTimersByTimeAsync(3000));

    expect(await screen.findByText(/You can try again now/)).toBeDefined();
    expect(field.readOnly).toBe(false);
    expect(calls).toBe(1);

    await app.user.type(field, "654321");
    expect(calls).toBe(2);
  });

  it("take no paste during a 429's wait: the value stays and nothing is sent", async () => {
    const time = fakeTime();
    let calls = 0;
    const app = await atLogin(
      {
        login: async () => MFA,
        loginMfa: async () => {
          calls += 1;
          throw limited(30);
        },
      },
      time.advanceTimers,
    );
    await logIn(app);
    const field = (await screen.findByLabelText("Code")) as HTMLInputElement;
    await app.user.type(field, "123456");
    await screen.findByText("Try again in 30 seconds.");

    await app.user.paste("654321");
    await act(() => vi.advanceTimersByTimeAsync(0));

    expect(field.value).toBe("");
    expect(calls).toBe(1);
  });

  it("take no paste while a code is on its way: the value stays and nothing is sent", async () => {
    const { app, field, sent } = await atCode();
    await app.user.type(field, "123456");
    expect(sent()).toEqual(["123456"]);

    await app.user.paste("654321");

    expect(field.value).toBe("123456");
    expect(sent()).toEqual(["123456"]);
  });

  it("swap to a text field by the link, and back, dropping what was typed", async () => {
    const { app, field, sent } = await atCode();

    await app.user.type(field, "12");
    await app.user.click(screen.getByRole("button", { name: "Use a recovery code" }));

    const recovery = (await screen.findByLabelText("Recovery code")) as HTMLInputElement;
    expect(recovery.value).toBe("");
    expect(document.activeElement).toBe(recovery);
    expect(recovery.getAttribute("autocomplete")).toBe("off");
    await app.user.type(recovery, "abcd");

    await app.user.click(screen.getByRole("button", { name: "Use the 6-digit code" }));

    const back = (await screen.findByLabelText("Code")) as HTMLInputElement;
    expect(back.value).toBe("");
    expect(document.activeElement).toBe(back);
    expect(sent()).toEqual([]);
  });

  it("send a recovery code on Verify only, without its spaces", async () => {
    const { app, sent } = await atCode();

    await app.user.click(screen.getByRole("button", { name: "Use a recovery code" }));
    const recovery = await screen.findByLabelText("Recovery code");
    await app.user.type(recovery, "abcd efgh ijkl mnop qrst");
    expect(sent()).toEqual([]);

    await app.user.click(screen.getByRole("button", { name: "Verify" }));

    expect(sent()).toEqual(["abcdefghijklmnopqrst"]);
  });

  it("count a wrong recovery code as a try, and clear it", async () => {
    const { app, sent } = await atCode(async () => {
      throw WRONG();
    });

    await app.user.click(screen.getByRole("button", { name: "Use a recovery code" }));
    const recovery = (await screen.findByLabelText("Recovery code")) as HTMLInputElement;
    await app.user.type(recovery, RECOVERY);
    await app.user.keyboard("{Enter}");

    expect(await screen.findByText("Wrong code. 2 tries left.")).toBeDefined();
    await waitFor(() => expect(recovery.value).toBe(""));
    expect(document.activeElement).toBe(recovery);
    expect(sent()).toHaveLength(1);
  });

  it("keep the code out of every cache", async () => {
    const { app, field } = await atCode(async () => {
      throw WRONG();
    });

    await app.user.type(field, "987654");
    await screen.findByText("Wrong code. 2 tries left.");

    expect(cacheText(app)).not.toContain("987654");
    expect(cacheText(app)).not.toContain(PASSWORD);
  });
});

describe("the setup step's boxes (U99)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  /** The App at the 2FA setup, its secret loaded unless `mfaEnroll` is given. */
  async function atSetup(handlers: Parameters<typeof atLogin>[0] = {}) {
    const app = await atLogin({
      login: async () => SETUP,
      mfaEnroll: async () => ENROLLMENT,
      mfaConfirm: () => new Promise(() => {}),
      ...handlers,
    });
    await logIn(app);
    const field = (await screen.findByLabelText("Code")) as HTMLInputElement;
    const sent = () =>
      app.fake.calls.filter((call) => call.method === "mfaConfirm").map((call) => call.args[0]);
    return { app, field, sent };
  }

  it("send at the sixth digit, once, and have no tries or recovery link", async () => {
    const { app, field, sent } = await atSetup();
    await screen.findByRole("img", { name: /QR code/ });

    await app.user.type(field, "654321");

    expect(sent()).toEqual(["654321"]);
    expect(screen.queryByText(/tries left/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Use a recovery code" })).toBeNull();
  });

  it("leave the focus on the step's heading, as the QR code comes first", async () => {
    await atSetup();

    expect(document.activeElement).toBe(screen.getByRole("heading", { level: 1 }));
  });

  it("refuse a pasted recovery code, sending nothing and swapping nothing", async () => {
    const { app, field, sent } = await atSetup();
    await screen.findByRole("img", { name: /QR code/ });

    await app.user.click(field);
    await app.user.paste(RECOVERY);

    expect(screen.getByText(NOT_A_CODE)).toBeDefined();
    expect(field.value).toBe("");
    expect(screen.queryByLabelText("Recovery code")).toBeNull();
    expect(sent()).toEqual([]);
  });

  it("keep the refusal's status mounted, and say the same refusal again", async () => {
    const { app, field } = await atSetup();
    await screen.findByRole("img", { name: /QR code/ });
    const status = refusalStatus();
    expect(status.textContent).toBe("");

    await app.user.type(field, "a");
    expect(status.textContent).toBe(NOT_A_CODE);
    const changes = changesTo(status);
    await app.user.type(field, "b");

    expect(status.textContent).toBe(NOT_A_CODE);
    expect(changes()).toBeGreaterThan(0);
  });

  it("take the focus back after Confirm's resend fails", async () => {
    const { app, field, sent } = await atSetup({
      mfaConfirm: async () => {
        throw fakeApiError("unreachable");
      },
    });
    await screen.findByRole("img", { name: /QR code/ });
    await app.user.type(field, "654321");
    await screen.findByText(/fake unreachable/);

    await app.user.click(screen.getByRole("button", { name: "Confirm" }));

    await waitFor(() => expect(sent()).toHaveLength(2));
    await waitFor(() => expect(document.activeElement).toBe(field));
  });

  it("take the focus back after Confirm's resend is limited", async () => {
    let calls = 0;
    const { app, field } = await atSetup({
      mfaConfirm: async () => {
        calls += 1;
        if (calls === 1) {
          throw fakeApiError("unreachable");
        }
        throw limited(30);
      },
    });
    await screen.findByRole("img", { name: /QR code/ });
    await app.user.type(field, "654321");
    await screen.findByText(/fake unreachable/);

    await app.user.click(screen.getByRole("button", { name: "Confirm" }));

    await screen.findByText("Try again in 30 seconds.");
    expect(document.activeElement).toBe(field);
  });

  it("wait while a new key loads", async () => {
    const { field } = await atSetup({ mfaEnroll: () => new Promise(() => {}) });

    expect(field.readOnly).toBe(true);
  });

  it("clear after a code the server refuses, and send the next", async () => {
    let calls = 0;
    const { app, field, sent } = await atSetup({
      mfaConfirm: async () => {
        calls += 1;
        throw refusal(422, "validation_failed", {
          fields: [{ path: "code", message: "wrong code" }],
        });
      },
    });
    await screen.findByRole("img", { name: /QR code/ });

    await app.user.type(field, "000000");

    expect((await screen.findByRole("alert")).textContent).toBe("wrong code");
    await waitFor(() => expect(field.value).toBe(""));
    await app.user.type(field, "111111");

    expect(sent()).toEqual(["000000", "111111"]);
    expect(calls).toBe(2);
  });

  it("ask for all six when Confirm comes early, as an alert, sending nothing", async () => {
    const { app, field, sent } = await atSetup();
    await screen.findByRole("img", { name: /QR code/ });

    await app.user.type(field, "123");
    await app.user.click(screen.getByRole("button", { name: "Confirm" }));

    expect((await screen.findByRole("alert")).textContent).toBe(ALL_SIX);
    expect(sent()).toEqual([]);
  });

  it("take no paste during a 429's wait: the value stays and nothing is sent", async () => {
    const time = fakeTime();
    let calls = 0;
    const app = await atLogin(
      {
        login: async () => SETUP,
        mfaEnroll: async () => ENROLLMENT,
        mfaConfirm: async () => {
          calls += 1;
          throw limited(30);
        },
      },
      time.advanceTimers,
    );
    await logIn(app);
    const field = (await screen.findByLabelText("Code")) as HTMLInputElement;
    await screen.findByRole("img", { name: /QR code/ });
    await app.user.type(field, "123456");
    await screen.findByText("Try again in 30 seconds.");

    await app.user.paste("654321");
    await act(() => vi.advanceTimersByTimeAsync(0));

    expect(field.value).toBe("");
    expect(calls).toBe(1);
  });

  it("take no paste while a code is on its way: the value stays and nothing is sent", async () => {
    const { app, field, sent } = await atSetup();
    await screen.findByRole("img", { name: /QR code/ });
    await app.user.type(field, "123456");
    expect(sent()).toEqual(["123456"]);

    await app.user.paste("654321");

    expect(field.value).toBe("123456");
    expect(sent()).toEqual(["123456"]);
  });

  it("keep the digits after a connection failure, Confirm resending them", async () => {
    let calls = 0;
    const { app, field, sent } = await atSetup({
      mfaConfirm: async () => {
        calls += 1;
        if (calls === 1) {
          throw fakeApiError("unreachable");
        }
        return { recovery_codes: ["aaaa"] };
      },
    });
    await screen.findByRole("img", { name: /QR code/ });

    await app.user.type(field, "654321");
    await screen.findByText(/fake unreachable/);
    expect(field.value).toBe("654321");

    await app.user.click(screen.getByRole("button", { name: "Confirm" }));

    expect(await screen.findByRole("list", { name: "Recovery codes" })).toBeDefined();
    expect(sent()).toEqual(["654321", "654321"]);
  });
});
