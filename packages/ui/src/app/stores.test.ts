import { describe, expect, it } from "vitest";

import { createAppStores } from "./stores";

describe("the session store", () => {
  it("starts in the starting phase, generation 0", () => {
    const { session } = createAppStores();

    expect(session.getState().phase).toBe("starting");
    expect(session.getState().generation).toBe(0);
  });

  it("bumps the generation at every sign-in", () => {
    const { session } = createAppStores();

    session.getState().signIn();
    const first = session.getState().generation;
    session.getState().signOut();
    session.getState().signIn();

    expect(first).toBe(1);
    expect(session.getState().phase).toBe("signed_in");
    expect(session.getState().generation).toBe(2);
  });

  it("ends a signed-in session once, for a call of its generation", () => {
    const { session } = createAppStores();
    session.getState().signIn();
    const generation = session.getState().generation;

    const first = session.getState().endIfSignedIn(generation);
    const second = session.getState().endIfSignedIn(generation);

    expect(first).toBe(true);
    expect(second).toBe(false);
    expect(session.getState().phase).toBe("signed_out");
  });

  it("never ends a session for a call of an earlier generation", () => {
    const { session } = createAppStores();
    session.getState().signIn();
    const stale = session.getState().generation;
    session.getState().signOut();
    session.getState().signIn();

    expect(session.getState().endIfSignedIn(stale)).toBe(false);
    expect(session.getState().phase).toBe("signed_in");
  });

  it.each(["starting", "signed_out", "logging_out"] as const)("ends nothing while %s", (phase) => {
    const { session } = createAppStores();
    session.getState().signIn();
    const generation = session.getState().generation;
    if (phase === "logging_out") {
      session.getState().beginLogout();
    } else if (phase === "signed_out") {
      session.getState().signOut();
    } else {
      session.setState({ phase: "starting" });
    }

    expect(session.getState().endIfSignedIn(generation)).toBe(false);
    expect(session.getState().phase).toBe(phase);
  });

  it("goes back to signed in, same generation, when a logout fails", () => {
    const { session } = createAppStores();
    session.getState().signIn();
    session.getState().beginLogout();

    session.getState().cancelLogout();

    expect(session.getState().phase).toBe("signed_in");
    expect(session.getState().generation).toBe(1);
  });
});

describe("the connection store", () => {
  it("starts with no banner", () => {
    const { connection } = createAppStores();

    expect(connection.getState().status).toBe("ok");
  });

  it("bumps the epoch when the banner goes up, not while it stays up", () => {
    const { connection } = createAppStores();

    connection.getState().fail("down");
    const up = connection.getState().epoch;
    connection.getState().fail("down");
    connection.getState().busy(30);

    expect(up).toBe(1);
    expect(connection.getState().epoch).toBe(1);
    expect(connection.getState().status).toBe("busy");
    expect(connection.getState().retryAfterSecs).toBe(30);
  });

  it("lets only a call that started after the banner went up take it down", () => {
    const { connection } = createAppStores();
    const before = connection.getState().epoch;
    connection.getState().fail("down");
    const after = connection.getState().epoch;

    const early = connection.getState().recover(before);
    const late = connection.getState().recover(after);

    expect(early).toBe(false);
    expect(late).toBe(true);
    expect(connection.getState().status).toBe("ok");
    expect(connection.getState().detail).toBeUndefined();
  });

  it("recovers nothing without a banner", () => {
    const { connection } = createAppStores();

    expect(connection.getState().recover(0)).toBe(false);
  });

  it("keeps the banner with a check's other failure", () => {
    const { connection } = createAppStores();
    connection.getState().fail("down");

    connection.getState().checkFailed("The server's answer isn't one this app understands.");

    expect(connection.getState().status).toBe("down");
    expect(connection.getState().detail).toBe(
      "The server's answer isn't one this app understands.",
    );
  });

  it("shows a refused certificate as its own state", () => {
    const { connection } = createAppStores();

    connection.getState().fail("certificate");

    expect(connection.getState().status).toBe("certificate");
    expect(connection.getState().epoch).toBe(1);
  });

  it("shares nothing between two Apps' stores", () => {
    const one = createAppStores();
    const two = createAppStores();

    one.session.getState().signIn();
    one.connection.getState().fail("down");

    expect(two.session.getState().phase).toBe("starting");
    expect(two.connection.getState().status).toBe("ok");
  });
});
