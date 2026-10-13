import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FakeApiClient, fakeApiError } from "../../api/FakeApiClient";
import { createAppRuntime } from "../../app/runtime";
import { AppErrorBoundary } from "./components/AppErrorBoundary";
import { ErrorPanel } from "./components/ErrorPanel";
import { RouteError } from "./components/RouteError";

describe("ErrorPanel", () => {
  it("shows an API error's text", () => {
    render(<ErrorPanel error={fakeApiError("store")} onReset={() => {}} />);

    expect(screen.getByRole("heading", { name: "Something went wrong in the app." })).toBeDefined();
    expect(screen.getByText("fake store")).toBeDefined();
  });

  it("shows only the name of any other error, never its message", () => {
    render(<ErrorPanel error={new TypeError("afk_rt_secret in a message")} onReset={() => {}} />);

    expect(screen.getByText("TypeError")).toBeDefined();
    expect(screen.queryByText(/afk_rt_secret/)).toBeNull();
  });

  it("tries again and restarts on request", async () => {
    const user = userEvent.setup();
    const onReset = vi.fn();
    const onRestart = vi.fn();
    render(<ErrorPanel error={new Error("x")} onReset={onReset} onRestart={onRestart} />);

    await user.click(screen.getByRole("button", { name: "Try again" }));
    await user.click(screen.getByRole("button", { name: "Restart the app" }));

    expect(onReset).toHaveBeenCalledTimes(1);
    expect(onRestart).toHaveBeenCalledTimes(1);
  });
});

describe("AppErrorBoundary", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  /** A child that throws while `broken` is set. */
  function Fragile({ broken }: { broken: { current: boolean } }) {
    const [, rerender] = useState(0);
    if (broken.current) {
      throw new RangeError("a secret detail");
    }
    return (
      <button type="button" onClick={() => rerender((n) => n + 1)}>
        Working
      </button>
    );
  }

  it("catches a render error, logs it and shows the error screen", () => {
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    const broken = { current: true };

    render(
      <AppErrorBoundary>
        <Fragile broken={broken} />
      </AppErrorBoundary>,
    );

    expect(screen.getByText("RangeError")).toBeDefined();
    expect(screen.queryByText(/a secret detail/)).toBeNull();
    expect(logged.mock.calls.some((call) => call.some((arg) => arg instanceof RangeError))).toBe(
      true,
    );
  });

  it("renders its children again after Try again", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const user = userEvent.setup();
    const broken = { current: true };
    render(
      <AppErrorBoundary>
        <Fragile broken={broken} />
      </AppErrorBoundary>,
    );

    broken.current = false;
    await user.click(screen.getByRole("button", { name: "Try again" }));

    expect(screen.getByRole("button", { name: "Working" })).toBeDefined();
  });

  it("restarts through its onRestart", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const user = userEvent.setup();
    const onRestart = vi.fn();
    render(
      <AppErrorBoundary onRestart={onRestart}>
        <Fragile broken={{ current: true }} />
      </AppErrorBoundary>,
    );

    await user.click(screen.getByRole("button", { name: "Restart the app" }));

    expect(onRestart).toHaveBeenCalledTimes(1);
  });
});

describe("the router's error component", () => {
  it("is the error screen", () => {
    const runtime = createAppRuntime(new FakeApiClient());

    expect(runtime.router.options.defaultErrorComponent).toBe(RouteError);
  });
});
