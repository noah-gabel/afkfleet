// Renders the whole App over a FakeApiClient, the way the platform does, and hands the
// test its runtime (stores, cache, router) and a user-event session.
import { act, render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StrictMode } from "react";
import { vi } from "vitest";

import { AppRoot } from "../App";
import type { ApiClient } from "../api/ApiClient";
import { FakeApiClient } from "../api/FakeApiClient";
import { createAppRuntime } from "../app/runtime";

/** What `renderApp` hands a test. */
export interface RenderedApp {
  readonly fake: FakeApiClient;
  readonly runtime: ReturnType<typeof createAppRuntime>;
  readonly user: ReturnType<typeof userEvent.setup>;
  /** The router's current path. */
  path(): string;
  /** The `onRestart` the error boundary calls. */
  readonly restart: ReturnType<typeof vi.fn>;
}

/** The App over a FakeApiClient scripted with `handlers`. */
export function renderApp(
  handlers: Partial<ApiClient> = {},
  options: { advanceTimers?: (ms: number) => unknown; strict?: boolean } = {},
): RenderedApp {
  const fake = new FakeApiClient(handlers);
  const runtime = createAppRuntime(fake);
  const restart = vi.fn();
  const user = userEvent.setup(
    options.advanceTimers === undefined ? {} : { advanceTimers: options.advanceTimers },
  );
  const root = <AppRoot runtime={runtime} onRestart={restart} />;
  // StrictMode as in development: React runs every effect twice.
  render(options.strict === true ? <StrictMode>{root}</StrictMode> : root);
  return { fake, runtime, user, restart, path: () => runtime.router.state.location.pathname };
}

/**
 * Fake timers that React and Testing Library still work with: only timeouts, intervals
 * and `Date` are faked (React's scheduler uses `setImmediate`), and fake time also moves
 * with real time, so `findBy*`'s polling keeps going. Call `vi.useRealTimers()` after.
 */
export function fakeTime() {
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"],
    shouldAdvanceTime: true,
  });
  return {
    advanceTimers: (ms: number) => vi.advanceTimersByTime(ms),
    /** Moves fake time on by `ms`, letting React settle. */
    advance: (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms)),
  };
}
