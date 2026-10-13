import { describe, expect, it } from "vitest";

import { createQueryClient } from "./queryClient";

describe("createQueryClient", () => {
  it("never retries and never refetches on focus or reconnect", () => {
    const defaults = createQueryClient().getDefaultOptions();

    expect(defaults.queries?.retry).toBe(false);
    expect(defaults.queries?.refetchOnWindowFocus).toBe(false);
    expect(defaults.queries?.refetchOnReconnect).toBe(false);
    expect(defaults.mutations?.retry).toBe(false);
  });

  it("gives every App its own cache", () => {
    const one = createQueryClient();
    const two = createQueryClient();

    one.setQueryData(["me"], "cached");

    expect(two.getQueryData(["me"])).toBeUndefined();
  });
});
