// The forms' wait after a 429, checked the same way in every form's tests (Plan.md P8.7).
import { screen } from "@testing-library/react";
import { expect } from "vitest";

import type { ApiMethod } from "../api/ApiClient";
import { TRY_AGAIN } from "../components/RetryWaitMessage";
import type { RenderedApp } from "./renderApp";
import { refusal } from "./signIn";

/** A 429 that asks to wait `seconds`. */
export function limited(seconds = 3) {
  return refusal(429, "rate_limited", { retry_after_secs: seconds });
}

/**
 * Checks a form's wait after a refusal that asked for `seconds`: it counts down with the
 * submit `button` disabled, a click on it sends nothing more of `method`, and at zero the
 * button is back and the form says once that the user can try again.
 */
export async function expectWait(
  app: Pick<RenderedApp, "fake" | "user">,
  time: { advance(ms: number): Promise<unknown> },
  options: { button: string; method: ApiMethod; seconds?: number },
): Promise<void> {
  const seconds = options.seconds ?? 3;
  const button = () => screen.getByRole("button", { name: options.button }) as HTMLButtonElement;
  const sent = () => app.fake.calls.filter((call) => call.method === options.method).length;

  expect(await screen.findByText(`Try again in ${seconds} s.`)).toBeDefined();
  expect(button().disabled).toBe(true);
  const before = sent();
  await app.user.click(button());
  await time.advance(1000);
  expect(screen.getByText(`Try again in ${seconds - 1} s.`)).toBeDefined();
  expect(button().disabled).toBe(true);
  await time.advance((seconds - 1) * 1000);

  expect(sent()).toBe(before);
  expect(screen.queryByText(/Try again in \d+ s\./)).toBeNull();
  expect(screen.getByText(TRY_AGAIN)).toBeDefined();
  expect(button().disabled).toBe(false);
}
