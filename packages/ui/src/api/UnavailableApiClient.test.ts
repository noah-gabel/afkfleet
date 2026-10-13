import { describe, expect, it } from "vitest";

import { API_METHODS } from "./ApiClient";
import { ApiError } from "./ApiError";
import { UnavailableApiClient, WEB_BUILD_MESSAGE } from "./UnavailableApiClient";

describe("UnavailableApiClient", () => {
  it("rejects every method with ipc", async () => {
    const client = new UnavailableApiClient();

    for (const method of API_METHODS) {
      const call = (client[method] as () => Promise<unknown>).call(client);

      await expect(call).rejects.toBeInstanceOf(ApiError);
      await expect(call).rejects.toMatchObject({
        error: { code: "ipc", message: WEB_BUILD_MESSAGE },
      });
    }
  });
});
