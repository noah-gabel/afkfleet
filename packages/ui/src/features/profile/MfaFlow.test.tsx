import { act, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError } from "../../api/FakeApiClient";
import type { SetupStepResponse } from "../../generated/SetupStepResponse";
import { deferred } from "../../test/fixtures";
import { fakeTime } from "../../test/renderApp";
import { atScreen, type SignedInOptions } from "../../test/signedIn";
import {
  CODES,
  cacheText,
  ENROLLMENT,
  enterCode,
  PASSWORD,
  refusal,
  SECRET,
} from "../../test/signIn";
import { MFA_REPLACED } from "./components/ProfileFlow";
import { STEP_ENDED } from "./profileFlowReducer";

const ONE_SESSION = {
  items: [
    {
      id: "01900000-0000-7000-8000-000000000011",
      created_at: "2026-10-13T08:00:00.000Z",
      last_used_at: "2026-10-13T09:00:00.000Z",
      expires_at: "2026-10-20T09:00:00.000Z",
      current: true,
    },
  ],
};
const RESET: SetupStepResponse = { expires_in: 600 };

/** The replacement over `handlers`. */
async function atReplace(handlers: Partial<ApiClient> = {}, options: SignedInOptions = {}) {
  const app = await atScreen(
    "/profile/2fa",
    {
      sessions: async () => ONE_SESSION,
      stepUp: async () => {},
      mfaReset: async () => RESET,
      mfaEnroll: async () => ENROLLMENT,
      mfaConfirm: async () => ({ recovery_codes: CODES }),
      ...handlers,
    },
    options,
  );
  await screen.findByRole("heading", { name: "Replace your authenticator" });
  return app;
}

type App = Awaited<ReturnType<typeof atReplace>>;

async function confirmPassword(app: App) {
  await app.user.type(screen.getByLabelText("Current password"), PASSWORD);
  await app.user.click(screen.getByRole("button", { name: "Continue" }));
}

function called(app: App, method: keyof ApiClient): readonly (readonly unknown[])[] {
  return app.fake.calls.filter((call) => call.method === method).map((call) => call.args);
}

describe("replacing the authenticator", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("asks for the password, then starts the replacement and shows the new key", async () => {
    const app = await atReplace();

    await confirmPassword(app);

    expect(await screen.findByText("ABCD EFGH IJKL MNOP QRST")).toBeDefined();
    expect(screen.getByRole("heading", { name: "Set up your new authenticator" })).toBeDefined();
    expect(screen.getByText(/current authenticator keeps working until you confirm/)).toBeDefined();
    const flow = ["stepUp", "mfaReset", "mfaEnroll", "mfaConfirm", "login"];
    expect(app.fake.calls.map((call) => call.method).filter((m) => flow.includes(m))).toEqual([
      "stepUp",
      "mfaReset",
      "mfaEnroll",
    ]);
  });

  it("requests the new secret once, also under StrictMode", async () => {
    const app = await atReplace({}, { strict: true });

    await confirmPassword(app);
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    expect(called(app, "mfaEnroll")).toHaveLength(1);
  });

  it("counts the setup's deadline from when the reset was sent", async () => {
    const time = fakeTime();
    const answer = deferred<SetupStepResponse>();
    const app = await atReplace(
      { mfaReset: () => answer.promise },
      {
        advanceTimers: time.advanceTimers,
      },
    );
    await confirmPassword(app);

    await time.advance(120_000);
    await act(async () => answer.resolve(RESET));
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");
    await time.advance(481_000);

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
  });

  it("shows the new recovery codes, and finishes without logging in again", async () => {
    const app = await atReplace();
    await confirmPassword(app);
    await enterCode(app, "654321");
    expect(await screen.findByRole("heading", { name: "New recovery codes" })).toBeDefined();
    const finish = screen.getByRole("button", { name: "Finish" });
    expect((finish as HTMLButtonElement).disabled).toBe(true);
    const mes = called(app, "me").length;

    await app.user.click(screen.getByLabelText("I've saved these codes"));
    await app.user.click(finish);

    expect(await screen.findByText(MFA_REPLACED)).toBeDefined();
    await waitFor(() => expect(app.path()).toBe("/profile"));
    expect(called(app, "mfaConfirm")).toEqual([["654321"]]);
    expect(called(app, "login")).toEqual([]);
    expect(called(app, "me").length).toBeGreaterThan(mes);
  });

  it("goes back to the password when the reset wants a new step-up", async () => {
    const app = await atReplace({
      mfaReset: async () => {
        throw refusal(403, "step_up_required");
      },
    });

    await confirmPassword(app);

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect(called(app, "mfaEnroll")).toEqual([]);
  });

  it("goes back to the password when the setup step is gone", async () => {
    const app = await atReplace({
      mfaConfirm: async () => {
        throw fakeApiError("no_pending_step");
      },
    });
    await confirmPassword(app);

    await enterCode(app, "654321");

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect(screen.getByLabelText("Current password")).toBeDefined();
  });

  it("holds no password, secret, QR code or recovery code in the cache, finished or cancelled", async () => {
    const finished = await atReplace();
    await confirmPassword(finished);
    await enterCode(finished, "654321");
    await finished.user.click(await screen.findByLabelText("I've saved these codes"));
    await finished.user.click(screen.getByRole("button", { name: "Finish" }));
    await waitFor(() => expect(finished.path()).toBe("/profile"));

    const cached = cacheText(finished);
    for (const secret of [PASSWORD, SECRET, ENROLLMENT.qr_svg, ...CODES]) {
      expect(cached).not.toContain(secret);
    }
  });

  it("holds none after a cancelled setup either", async () => {
    const app = await atReplace();
    await confirmPassword(app);
    await screen.findByText("ABCD EFGH IJKL MNOP QRST");

    await app.user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(app.path()).toBe("/profile"));

    const cached = cacheText(app);
    for (const secret of [PASSWORD, SECRET, ENROLLMENT.qr_svg]) {
      expect(cached).not.toContain(secret);
    }
  });
});
