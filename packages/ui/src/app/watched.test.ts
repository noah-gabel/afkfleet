import { describe, expect, it, vi } from "vitest";

import { ApiError } from "../api/ApiError";
import { FakeApiClient, fakeApiError, fakeWatch } from "../api/FakeApiClient";
import type { CommandErrorCode } from "../generated/CommandErrorCode";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import type { MeResponse } from "../generated/MeResponse";
import { createAppStores } from "./stores";
import { watched } from "./watched";

const ME: MeResponse = {
  id: "01900000-0000-7000-8000-000000000001",
  username: "afkbot1",
  role: "member",
  created_at: "2026-10-13T08:00:00Z",
  password_changed_at: "2026-10-13T08:00:00Z",
  recovery_codes_left: 10,
  failed_code_attempts: { count: 0 },
};

/** A promise and the functions that settle it. */
function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function setup(handlers: ConstructorParameters<typeof FakeApiClient>[0]) {
  const stores = createAppStores();
  const events = { sessionEnded: vi.fn(), recovered: vi.fn() };
  const client = watched(new FakeApiClient(handlers), stores, events);
  return { stores, events, client };
}

describe("watched", () => {
  it("answers what the client answers", async () => {
    const { client } = setup({ me: async () => ME });

    await expect(client.me()).resolves.toEqual(ME);
  });

  it("rethrows the client's ApiError itself", async () => {
    const refused = fakeApiError("api");
    const { client } = setup({
      me: async () => {
        throw refused;
      },
    });

    await expect(client.me()).rejects.toBe(refused);
  });

  it("turns a rejection that isn't an ApiError into ipc", async () => {
    const { client } = setup({
      me: async () => {
        throw "not allowed";
      },
    });

    const error = await client.me().catch((rejection: unknown) => rejection);

    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).error.code).toBe("ipc");
  });

  it.each(["unreachable", "timed_out"] as const)("puts the banner up on %s", async (code) => {
    const { client, stores } = setup({
      checkServer: async () => {
        throw fakeApiError(code);
      },
    });

    await client.checkServer().catch(() => {});

    expect(stores.connection.getState().status).toBe("down");
  });

  it("shows a refused certificate", async () => {
    const { client, stores } = setup({
      me: async () => {
        throw fakeApiError("certificate");
      },
    });

    await client.me().catch(() => {});

    expect(stores.connection.getState().status).toBe("certificate");
  });

  it.each<CommandErrorCode>(["api", "invalid_input", "store", "ipc", "already_logged_in"])(
    "leaves the banner and the session alone on %s",
    async (code) => {
      const { client, stores, events } = setup({
        me: async () => {
          throw fakeApiError(code);
        },
      });
      stores.session.getState().signIn();

      await client.me().catch(() => {});

      expect(stores.connection.getState().status).toBe("ok");
      expect(stores.session.getState().phase).toBe("signed_in");
      expect(events.sessionEnded).not.toHaveBeenCalled();
    },
  );

  it("ends a signed-in session on no_session, once for two answers", async () => {
    const { client, stores, events } = setup({
      me: async () => {
        throw fakeApiError("no_session");
      },
    });
    stores.session.getState().signIn();

    await Promise.all([client.me().catch(() => {}), client.me().catch(() => {})]);

    expect(events.sessionEnded).toHaveBeenCalledTimes(1);
    expect(stores.session.getState().phase).toBe("signed_out");
  });

  it("ends nothing for a no_session from before a re-login", async () => {
    const answer = deferred<MeResponse>();
    const { client, stores, events } = setup({ me: () => answer.promise });
    stores.session.getState().signIn();

    const call = client.me().catch(() => {});
    stores.session.getState().signOut();
    stores.session.getState().signIn();
    answer.reject(fakeApiError("no_session"));
    await call;

    expect(events.sessionEnded).not.toHaveBeenCalled();
    expect(stores.session.getState().phase).toBe("signed_in");
  });

  it("ends nothing on no_session while not signed in", async () => {
    const { client, stores, events } = setup({
      me: async () => {
        throw fakeApiError("no_session");
      },
    });

    await client.me().catch(() => {});

    expect(events.sessionEnded).not.toHaveBeenCalled();
    expect(stores.session.getState().phase).toBe("starting");
  });

  it("takes the banner down after a success that started after it went up", async () => {
    const { client, stores, events } = setup({ me: async () => ME });
    stores.connection.getState().fail("down");

    await client.me();

    expect(stores.connection.getState().status).toBe("ok");
    expect(events.recovered).toHaveBeenCalledTimes(1);
  });

  it("keeps the banner for a success that started before it went up", async () => {
    const answer = deferred<MeResponse>();
    const { client, stores, events } = setup({ me: () => answer.promise });

    const call = client.me();
    stores.connection.getState().fail("down");
    answer.resolve(ME);
    await call;

    expect(stores.connection.getState().status).toBe("down");
    expect(events.recovered).not.toHaveBeenCalled();
  });

  it("watches every method of the API", async () => {
    const { client, stores } = setup({});

    const failures = await Promise.all([
      client.serverProfile().catch(() => "failed"),
      client.revokeInvite("01900000-0000-7000-8000-000000000001").catch(() => "failed"),
    ]);

    expect(failures).toEqual(["failed", "failed"]);
    expect(stores.connection.getState().status).toBe("ok");
  });
});

/** A disconnect whose connect failed with `code`. */
function notOpened(code: CommandErrorCode): LiveUpdateMessage {
  return {
    update: "disconnected",
    disconnect: { why: "not_opened", error: { code, message: `fake ${code}` } },
    retry_in_secs: 2,
  };
}

describe("watched's live events", () => {
  it.each([
    ["unreachable", "down"],
    ["timed_out", "down"],
    ["certificate", "certificate"],
  ] as const)("an update whose connect was %s raises the banner as %s", async (code, status) => {
    const watch = fakeWatch();
    const { client, stores } = setup({ watchEvents: watch.handler });
    const got: LiveUpdateMessage[] = [];

    const watching = client.watchEvents((update) => got.push(update));
    watch.latest().push(notOpened(code));

    expect(stores.connection.getState().status).toBe(status);
    expect(got).toEqual([notOpened(code)]);
    watch.latest().end();
    await watching;
  });

  it("leaves the banner alone for an update whose connect was refused otherwise", async () => {
    const watch = fakeWatch();
    const { client, stores } = setup({ watchEvents: watch.handler });

    const watching = client.watchEvents(() => undefined);
    watch.latest().push(notOpened("api"));
    watch.latest().push({ update: "refetch", reason: "ready" });
    watch.latest().end();
    await watching;

    expect(stores.connection.getState().status).toBe("ok");
  });

  it("takes no banner down when a watch stops: that says nothing of the server", async () => {
    const watch = fakeWatch();
    const { client, stores, events } = setup({ watchEvents: watch.handler });
    stores.connection.getState().fail("down");

    const watching = client.watchEvents(() => undefined);
    watch.latest().end();
    await watching;

    expect(stores.connection.getState().status).toBe("down");
    expect(events.recovered).not.toHaveBeenCalled();
  });

  it("ends the session once for a watch that rejects with no_session, and raises the certificate banner", async () => {
    const watch = fakeWatch();
    const { client, stores, events } = setup({ watchEvents: watch.handler });
    stores.session.getState().signIn();

    const ending = client.watchEvents(() => undefined).catch((error: unknown) => error);
    watch.latest().fail(fakeApiError("no_session"));
    const refused = client.watchEvents(() => undefined).catch((error: unknown) => error);
    watch.latest().fail(fakeApiError("certificate"));

    expect(((await ending) as ApiError).error.code).toBe("no_session");
    expect(((await refused) as ApiError).error.code).toBe("certificate");
    expect(events.sessionEnded).toHaveBeenCalledTimes(1);
    expect(stores.connection.getState().status).toBe("certificate");
  });
});
