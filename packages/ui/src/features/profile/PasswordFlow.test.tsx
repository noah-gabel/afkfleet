import { act, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { deferred } from "../../test/fixtures";
import { fakeTime } from "../../test/renderApp";
import { expectWait, limited } from "../../test/retryWait";
import { atScreen, onHome, type SignedInOptions } from "../../test/signedIn";
import { cacheText, PASSWORD, refusal } from "../../test/signIn";
import { PASSWORD_CHANGED } from "./components/ProfileFlow";
import { STEP_ENDED } from "./profileFlowReducer";

const NEW = "another placeholder passphrase";
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

/** The password flow over `handlers`. */
async function atPassword(handlers: Partial<ApiClient> = {}, options: SignedInOptions = {}) {
  const app = await atScreen(
    "/profile/password",
    {
      sessions: async () => ONE_SESSION,
      stepUp: async () => {},
      changePassword: async () => {},
      ...handlers,
    },
    options,
  );
  await screen.findByRole("heading", { name: "Change password" });
  return app;
}

type App = Awaited<ReturnType<typeof atPassword>>;

async function confirmPassword(app: App, password = PASSWORD) {
  await app.user.type(screen.getByLabelText("Current password"), password);
  await app.user.click(screen.getByRole("button", { name: "Continue" }));
}

async function enterNew(app: App, password = NEW, repeat = password) {
  await app.user.type(await screen.findByLabelText("New password"), password);
  await app.user.type(screen.getByLabelText("Repeat the new password"), repeat);
  await app.user.click(screen.getByRole("button", { name: "Change password" }));
}

function called(app: App, method: keyof ApiClient): readonly (readonly unknown[])[] {
  return app.fake.calls.filter((call) => call.method === method).map((call) => call.args);
}

describe("changing the password", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("asks for the current password first", async () => {
    await atPassword();

    expect(screen.getByLabelText("Current password")).toBeDefined();
    expect(screen.queryByLabelText("New password")).toBeNull();
  });

  it("stays on a wrong password, with the field emptied", async () => {
    const app = await atPassword({
      stepUp: async () => {
        throw refusal(403, "forbidden");
      },
    });

    await confirmPassword(app);

    expect(await screen.findByText("Wrong password.")).toBeDefined();
    expect((screen.getByLabelText("Current password") as HTMLInputElement).value).toBe("");
    expect(screen.queryByLabelText("New password")).toBeNull();
  });

  it("counts a refusal's wait down at the step-up with Continue disabled", async () => {
    const time = fakeTime();
    const app = await atPassword(
      {
        stepUp: async () => {
          throw limited();
        },
      },
      { advanceTimers: time.advanceTimers },
    );

    await confirmPassword(app);

    await expectWait(app, time, { button: "Continue", method: "stepUp" });
  });

  it("counts a refusal's wait down at the new password with its button disabled", async () => {
    const time = fakeTime();
    const app = await atPassword(
      {
        changePassword: async () => {
          throw limited();
        },
      },
      { advanceTimers: time.advanceTimers },
    );
    await confirmPassword(app);

    await enterNew(app);

    await expectWait(app, time, { button: "Change password", method: "changePassword" });
  });

  it("then asks for the new password, before the step-up ends", async () => {
    const app = await atPassword();

    await confirmPassword(app);

    expect(await screen.findByLabelText("New password")).toBeDefined();
    expect(screen.getByText("This step ends in 5 minutes.")).toBeDefined();
    expect(called(app, "stepUp")).toEqual([[{ password: PASSWORD }]]);
  });

  it("sends nothing when the repeat doesn't match", async () => {
    const app = await atPassword();
    await confirmPassword(app);

    await enterNew(app, NEW, `${NEW}!`);

    expect(await screen.findByText("The passwords don't match.")).toBeDefined();
    expect(called(app, "changePassword")).toEqual([]);
  });

  it("shows every reason the server gives for a weak password at the field", async () => {
    const app = await atPassword({
      changePassword: async () => {
        throw refusal(422, "validation_failed", {
          fields: [
            { path: "new_password", message: "is too weak" },
            { path: "new_password", message: "Add another word or two." },
          ],
        });
      },
    });
    await confirmPassword(app);

    await enterNew(app);

    const field = await screen.findByText(/is too weak/);
    expect(field.textContent).toBe("is too weak\nAdd another word or two.");
    expect(screen.getByLabelText("New password")).toBeDefined();
  });

  it("goes back to the password when the server wants a new step-up", async () => {
    const app = await atPassword({
      changePassword: async () => {
        throw refusal(403, "step_up_required");
      },
    });
    await confirmPassword(app);

    await enterNew(app);

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect(screen.getByLabelText("Current password")).toBeDefined();
  });

  it("counts the step's 5 minutes from when the step-up was sent", async () => {
    const time = fakeTime();
    const answer = deferred<void>();
    const app = await atPassword(
      { stepUp: () => answer.promise },
      {
        advanceTimers: time.advanceTimers,
      },
    );
    await confirmPassword(app);

    await time.advance(120_000);
    await act(async () => answer.resolve());
    await screen.findByLabelText("New password");
    await time.advance(181_000);

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
  });

  it("sends no new password after the step's deadline", async () => {
    const time = fakeTime();
    const app = await atPassword({}, { advanceTimers: time.advanceTimers });
    await confirmPassword(app);
    await screen.findByLabelText("New password");

    vi.setSystemTime(Date.now() + 301_000);
    await enterNew(app);

    expect(await screen.findByText(STEP_ENDED)).toBeDefined();
    expect(called(app, "changePassword")).toEqual([]);
  });

  it("changes it, says the other sessions ended, and goes back to the profile", async () => {
    const app = await atPassword();
    await confirmPassword(app);
    const mes = called(app, "me").length;

    await enterNew(app);

    expect(await screen.findByText(PASSWORD_CHANGED)).toBeDefined();
    await waitFor(() => expect(app.path()).toBe("/profile"));
    expect(called(app, "changePassword")).toEqual([[{ new_password: NEW }]]);
    expect(called(app, "me").length).toBeGreaterThan(mes);
  });

  it("goes back to the profile on Cancel, sending nothing", async () => {
    const app = await atPassword();

    await app.user.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(app.path()).toBe("/profile"));
    expect(called(app, "stepUp")).toEqual([]);
  });

  it("changes nothing when the step-up answers after the user left", async () => {
    const answer = deferred<void>();
    const app = await atPassword({ stepUp: () => answer.promise });
    await confirmPassword(app);

    await act(() => app.runtime.router.navigate({ to: "/home" }));
    await act(async () => answer.resolve());

    await onHome("afkbot1");
    expect(app.path()).toBe("/home");
    expect(screen.queryByLabelText("New password")).toBeNull();
  });

  it("sends one step-up for a double click", async () => {
    const answer = deferred<void>();
    const app = await atPassword({ stepUp: () => answer.promise });
    await app.user.type(screen.getByLabelText("Current password"), PASSWORD);
    const button = screen.getByRole("button", { name: "Continue" });

    await app.user.dblClick(button);
    await act(async () => answer.resolve());

    await screen.findByLabelText("New password");
    expect(called(app, "stepUp")).toHaveLength(1);
  });

  it("holds neither password in the cache", async () => {
    const app = await atPassword();
    await confirmPassword(app);
    await enterNew(app);
    await waitFor(() => expect(app.path()).toBe("/profile"));

    const cached = cacheText(app);
    expect(cached).not.toContain(PASSWORD);
    expect(cached).not.toContain(NEW);
  });
});
