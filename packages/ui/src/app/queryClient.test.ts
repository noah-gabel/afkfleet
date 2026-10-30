import { onlineManager } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createQueryClient } from "./queryClient";

describe("createQueryClient", () => {
  afterEach(() => {
    onlineManager.setOnline(true);
  });

  it("reads while Windows reports no network", async () => {
    onlineManager.setOnline(false);
    const read = vi.fn(async () => "read");

    const answer = createQueryClient().fetchQuery({ queryKey: ["me"], queryFn: read });

    await vi.waitFor(() => expect(read).toHaveBeenCalledOnce());
    await expect(answer).resolves.toBe("read");
  });

  it("writes while Windows reports no network", async () => {
    onlineManager.setOnline(false);
    const write = vi.fn(async () => "written");
    const client = createQueryClient();

    const answer = client
      .getMutationCache()
      .build(client, { mutationFn: write })
      .execute(undefined);

    await vi.waitFor(() => expect(write).toHaveBeenCalledOnce());
    await expect(answer).resolves.toBe("written");
  });

  it("sets networkMode always for reads and writes", () => {
    const defaults = createQueryClient().getDefaultOptions();

    expect(defaults.queries?.networkMode).toBe("always");
    expect(defaults.mutations?.networkMode).toBe("always");
  });

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
