import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { MeResponse } from "../../generated/MeResponse";
import type { SessionPage } from "../../generated/SessionPage";
import { formatTime } from "../../lib/time";
import { me } from "../../test/fixtures";
import { atScreen, signedIn } from "../../test/signedIn";
import { failedCodesWarning, recoveryCodesWarning } from "./components/SecurityWarnings";

/** The current session alone. */
const ONE_SESSION: SessionPage = {
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

const LAST_WRONG = "2026-10-12T21:15:00.000Z";

/** A Member whose account has `changes`. */
function user(changes: Partial<MeResponse> = {}): MeResponse {
  return { ...me(), ...changes };
}

describe("the profile", () => {
  it("shows the account's facts and its two flows", async () => {
    await atScreen("/profile", { sessions: async () => ONE_SESSION });

    expect(await screen.findByRole("heading", { name: "Profile", level: 1 })).toBeDefined();
    const facts = screen.getByRole("region", { name: "Account" });
    expect(within(facts).getByText("afkbot1")).toBeDefined();
    expect(within(facts).getByText("Member")).toBeDefined();
    expect(within(facts).getByText(formatTime("2026-10-13T08:00:00Z"))).toBeDefined();
    expect(within(facts).getByText(`Set up ${formatTime("2026-10-13T08:05:00Z")}`)).toBeDefined();
    expect(screen.getByText("10 recovery codes left.")).toBeDefined();
    expect(screen.getByRole("link", { name: "Change password" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Replace authenticator" })).toBeDefined();
  });

  it("is in everyone's navigation", async () => {
    const app = await signedIn({ sessions: async () => ONE_SESSION });

    await app.user.click(screen.getByRole("link", { name: "Profile" }));

    await waitFor(() => expect(app.path()).toBe("/profile"));
  });

  it("shows no warning for an account without wrong codes and with enough recovery codes", async () => {
    await atScreen("/profile", { sessions: async () => ONE_SESSION });
    await screen.findByRole("heading", { name: "Profile", level: 1 });

    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("warns about wrong codes since the previous login, naming the last, and links to the password", async () => {
    const app = await atScreen(
      "/profile",
      { sessions: async () => ONE_SESSION },
      { user: user({ failed_code_attempts: { count: 2, last_at: LAST_WRONG } }) },
    );

    const warning = await screen.findByText(failedCodesWarning(2, LAST_WRONG));
    expect(warning.textContent).toContain(formatTime(LAST_WRONG));
    expect(warning.textContent).not.toMatch(/typo/i);
    const alert = warning.closest("[role=alert]") as HTMLElement;
    await app.user.click(within(alert).getByRole("link", { name: "Change password" }));
    await waitFor(() => expect(app.path()).toBe("/profile/password"));
  });

  it("warns when 3 or fewer recovery codes are left, and links to the replacement", async () => {
    const app = await atScreen(
      "/profile",
      { sessions: async () => ONE_SESSION },
      { user: user({ recovery_codes_left: 3 }) },
    );

    const warning = await screen.findByText(recoveryCodesWarning(3));
    const alert = warning.closest("[role=alert]") as HTMLElement;
    await app.user.click(within(alert).getByRole("link", { name: "Replace authenticator" }));
    await waitFor(() => expect(app.path()).toBe("/profile/2fa"));
  });

  it.each([
    [4, false],
    [3, true],
    [1, true],
    [0, true],
  ])("with %i recovery codes left warns: %s", async (left, warns) => {
    await atScreen(
      "/profile",
      { sessions: async () => ONE_SESSION },
      { user: user({ recovery_codes_left: left }) },
    );
    await screen.findByRole("heading", { name: "Profile", level: 1 });

    expect(screen.queryByText(recoveryCodesWarning(left)) !== null).toBe(warns);
  });
});

describe("Home's warnings", () => {
  const WARNED = user({
    failed_code_attempts: { count: 1, last_at: LAST_WRONG },
    recovery_codes_left: 2,
  });

  it("shows both, each linking to the profile", async () => {
    const app = await signedIn({ sessions: async () => ONE_SESSION }, { user: WARNED });

    const failed = screen.getByText(failedCodesWarning(1, LAST_WRONG)).closest("[role=alert]");
    const codes = screen.getByText(recoveryCodesWarning(2)).closest("[role=alert]");
    expect(failed).not.toBeNull();
    expect(codes).not.toBeNull();
    await app.user.click(
      within(failed as HTMLElement).getByRole("link", { name: "Open your profile" }),
    );
    await waitFor(() => expect(app.path()).toBe("/profile"));
  });

  it("hides a dismissed one until the next sign-in, and the profile still shows it", async () => {
    const app = await signedIn({ sessions: async () => ONE_SESSION }, { user: WARNED });
    const failed = screen.getByText(failedCodesWarning(1, LAST_WRONG)).closest("[role=alert]");

    await app.user.click(within(failed as HTMLElement).getByRole("button", { name: "Dismiss" }));

    expect(screen.queryByText(failedCodesWarning(1, LAST_WRONG))).toBeNull();
    expect(screen.getByText(recoveryCodesWarning(2))).toBeDefined();
    await act(() => app.runtime.router.navigate({ to: "/profile" }));
    expect(await screen.findByText(failedCodesWarning(1, LAST_WRONG))).toBeDefined();
    await act(() => app.runtime.router.navigate({ to: "/home" }));
    await screen.findByText(/Signed in as afkbot1/);
    expect(screen.queryByText(failedCodesWarning(1, LAST_WRONG))).toBeNull();

    act(() => app.runtime.stores.session.getState().signIn());

    expect(await screen.findByText(failedCodesWarning(1, LAST_WRONG))).toBeDefined();
  });
});
