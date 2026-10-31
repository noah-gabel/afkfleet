// The audit log's screen (Plan.md P11.9; ADR-0020, U78, U80, U81, S52, S85, S89, R173–R175):
// entries a page at a time, each with who, what, on what, how it ended, from where and its
// details; the server's own entries; unreadable rows by ID; names from the users' list; and the
// filter by target, from an entry and from the users' screen.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { fakeApiError } from "../../api/FakeApiClient";
import type { AuditPage } from "../../generated/AuditPage";
import { formatTime } from "../../lib/time";
import { auditId, entry, flag, int, listedUser, retentionMetadata, text } from "../../test/audit";
import { deferred, me } from "../../test/fixtures";
import { atScreen } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ADMIN = me("admin", "afkadmin");
const USER = auditId(1);
const BOT = auditId(2);

/** The screen, as an Admin, over `audit` and a users' list that names `USER`. */
async function auditScreen(
  audit: () => Promise<AuditPage>,
  users = async () => ({ items: [listedUser(USER, "afkbot1")] }),
) {
  return atScreen("/admin/audit", { audit, users }, { user: ADMIN });
}

/** Every row of the table's body. */
function rows(): HTMLElement[] {
  const table = screen.getByRole("table", { name: "Audit log" });
  return within(table).getAllByRole("row").slice(1);
}

describe("the audit log's screen", () => {
  it("shows an entry's time, who, what, target, result, address and details", async () => {
    await auditScreen(async () => ({
      items: [
        entry(5, {
          actor: USER,
          ip: "192.0.2.10",
          action: "user.set_disabled",
          target: { kind: "bot", id: BOT },
          metadata: [flag("disabled", true), int("sessions_revoked", 2)],
        }),
      ],
      unreadable: [],
    }));

    expect(await screen.findByRole("heading", { name: "Audit log", level: 1 })).toBeDefined();
    expect(screen.getByText("Everything the server recorded, newest first.")).toBeDefined();
    const [row] = await waitFor(() => {
      const found = rows();
      expect(found).toHaveLength(1);
      return found;
    });
    const inRow = within(row as HTMLElement);
    expect(inRow.getByText(formatTime("2026-10-13T08:00:00.000Z"))).toBeDefined();
    expect(await inRow.findByText("afkbot1")).toBeDefined();
    expect(inRow.getByText("User disabled or enabled")).toBeDefined();
    expect(inRow.getByText("Bot")).toBeDefined();
    expect(inRow.getByText(BOT)).toBeDefined();
    expect(inRow.getByText("Done")).toBeDefined();
    expect(inRow.getByText("192.0.2.10")).toBeDefined();
    expect(inRow.getByText("disabled: yes")).toBeDefined();
    expect(inRow.getByText("sessions revoked: 2")).toBeDefined();
  });

  it("shows retention.purge as the server's own, with its counts, and never as a name", async () => {
    await auditScreen(async () => ({
      items: [entry(5, { action: "retention.purge", metadata: retentionMetadata() })],
      unreadable: [],
    }));

    const row = await screen.findByRole("row", { name: /Old data deleted/ });
    expect(within(row).getByText("The server")).toBeDefined();
    expect(
      within(row).getByText(
        "Chat lines: 12 by age, 3 by size · Sessions: 2 · Replaced login tokens: 4 · Audit entries: 7 · Invites: 1 · Enrollment tokens: 5",
      ),
    ).toBeDefined();
    expect(within(row).queryByRole("link", { name: /Only/ })).toBeNull();
  });

  it("says No user for another entry without an actor", async () => {
    await auditScreen(async () => ({
      items: [entry(5, { action: "bot.assign", target: { kind: "bot", id: BOT } })],
      unreadable: [],
    }));

    const row = await screen.findByRole("row", { name: /Bot placed on an agent/ });
    expect(within(row).getByText("No user")).toBeDefined();
    expect(within(row).queryByText("The server")).toBeNull();
  });

  it("shows an agent's connect failure with its reason and address", async () => {
    await auditScreen(async () => ({
      items: [
        entry(5, {
          action: "agent.connect",
          outcome: "failure",
          ip: "192.0.2.20",
          target: { kind: "agent", id: BOT },
          metadata: [text("reason", "revoked"), text("fingerprint", "AB:CD")],
        }),
      ],
      unreadable: [],
    }));

    const row = await screen.findByRole("row", { name: /Agent connection/ });
    expect(within(row).getByText("Failed")).toBeDefined();
    expect(within(row).getByText("192.0.2.20")).toBeDefined();
    expect(within(row).getByText("reason: Its certificate was revoked")).toBeDefined();
    expect(within(row).getByText("fingerprint: AB:CD")).toBeDefined();
  });

  it("shows names, kinds and results only a newer server records as sent", async () => {
    await auditScreen(async () => ({
      items: [
        entry(5, {
          action: "agent.teleport" as never,
          outcome: "partly" as never,
          target: { kind: "planet" as never, id: BOT },
        }),
      ],
      unreadable: [],
    }));

    const row = await screen.findByRole("row", { name: /agent\.teleport/ });
    expect(within(row).getByText("partly")).toBeDefined();
    expect(within(row).getByText("planet")).toBeDefined();
  });

  it("marks an entry that doesn't read in its place, by its ID alone (S52)", async () => {
    await auditScreen(async () => ({
      items: [entry(9), entry(7)],
      unreadable: [8],
    }));

    await screen.findByText("Entry 8 can't be read. The server's log says why.");
    const shown = rows();
    expect(shown).toHaveLength(3);
    const [, unreadable] = shown;
    expect(within(unreadable as HTMLElement).getAllByRole("cell")).toHaveLength(1);
    expect(unreadable?.textContent).toBe("Entry 8 can't be read. The server's log says why.");
  });

  it("names a user by their list, with the whole ID on hover and for screen readers (U80)", async () => {
    await auditScreen(async () => ({
      items: [
        entry(5, {
          actor: USER,
          target: { kind: "user", id: USER },
          metadata: [text("grantee", USER), text("name", USER)],
        }),
      ],
      unreadable: [],
    }));

    const row = await screen.findByRole("row", { name: /Bot started/ });
    const names = await within(row).findAllByText("afkbot1");
    expect(names).toHaveLength(3);
    for (const name of names) {
      expect(name.getAttribute("title")).toBe(USER);
      expect(within(name).getByText(`(${USER})`)).toBeDefined();
    }
    // `name` is typed or reported, so it stays what it is though it holds a user's ID.
    expect(within(row).getByText(`name: ${USER}`)).toBeDefined();
  });

  it("shows a user the list doesn't hold by the whole ID, claiming nothing", async () => {
    const stranger = auditId(9);
    await auditScreen(async () => ({
      items: [entry(5, { actor: stranger })],
      unreadable: [],
    }));

    const row = await screen.findByRole("row", { name: /Bot started/ });
    expect(await within(row).findByText(stranger)).toBeDefined();
    expect(screen.queryByText(/unknown|deleted/i)).toBeNull();
  });

  it("shows the entries while the names load, and says so when they couldn't be read", async () => {
    const users = deferred<{ items: [] }>();
    await auditScreen(
      async () => ({ items: [entry(5, { actor: USER })], unreadable: [] }),
      () => users.promise,
    );

    const row = await screen.findByRole("row", { name: /Bot started/ });
    expect(within(row).getByText(USER)).toBeDefined();
    expect(screen.queryByText("Names couldn't be loaded, so users show by ID.")).toBeNull();
    await act(async () => users.reject(refusal(500, "internal_error")));
    expect(await screen.findByText("Names couldn't be loaded, so users show by ID.")).toBeDefined();
    expect(within(row).getByText(USER)).toBeDefined();
  });

  it("pages with Show more, following next, and hides it on the last page", async () => {
    const audit = vi.fn(
      async (before?: number): Promise<AuditPage> =>
        before === undefined
          ? { items: [entry(9), entry(8)], unreadable: [], next: 8 }
          : { items: [entry(7)], unreadable: [] },
    );
    const app = await atScreen(
      "/admin/audit",
      { audit, users: async () => ({ items: [] }) },
      { user: ADMIN },
    );

    await app.user.click(await screen.findByRole("button", { name: "Show more" }));

    await waitFor(() => expect(rows()).toHaveLength(3));
    expect(audit).toHaveBeenLastCalledWith(8, undefined, undefined, undefined);
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("reads again with Load again", async () => {
    let times = 0;
    const app = await auditScreen(async () => {
      times += 1;
      return { items: times === 1 ? [entry(5)] : [entry(6), entry(5)], unreadable: [] };
    });
    await waitFor(() => expect(rows()).toHaveLength(1));

    await app.user.click(screen.getByRole("button", { name: "Load again" }));

    await waitFor(() => expect(rows()).toHaveLength(2));
  });

  it("says when the log is empty, and when it couldn't be read", async () => {
    await auditScreen(async () => ({ items: [], unreadable: [] }));
    expect(await screen.findByText("Nothing recorded yet.")).toBeDefined();
  });

  it("shows why a read failed, with its request ID, and keeps nothing it doesn't have", async () => {
    await auditScreen(async () => {
      throw refusal(500, "internal_error", { message: "Something went wrong." });
    });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Couldn't load the audit log: Something went wrong.");
    expect(screen.queryByText("Nothing recorded yet.")).toBeNull();
  });

  describe("the filter by target", () => {
    const bothTargets: AuditPage = {
      items: [
        entry(6, { target: { kind: "bot", id: BOT } }),
        entry(5, { target: { kind: "user", id: USER } }),
      ],
      unreadable: [],
    };

    it("narrows to an entry's target from the entry, and Show everything goes back", async () => {
      const audit = vi.fn(
        async (_before?: number, _limit?: number, kind?: string): Promise<AuditPage> =>
          kind === undefined
            ? bothTargets
            : { items: [entry(6, { target: { kind: "bot", id: BOT } })], unreadable: [] },
      );
      const app = await atScreen(
        "/admin/audit",
        { audit, users: async () => ({ items: [] }) },
        { user: ADMIN },
      );

      await app.user.click(await screen.findByRole("link", { name: `Only bot ${BOT}` }));

      expect(await screen.findByText(`Showing only bot ${BOT}.`)).toBeDefined();
      await waitFor(() => expect(rows()).toHaveLength(1));
      expect(audit).toHaveBeenLastCalledWith(undefined, undefined, "bot", BOT);
      expect(screen.queryByRole("link", { name: `Only bot ${BOT}` })).toBeNull();
      expect(
        screen.queryByText("This shows what happened to this user, not everything they did."),
      ).toBeNull();

      await app.user.click(screen.getByRole("link", { name: "Show everything" }));

      await waitFor(() => expect(rows()).toHaveLength(2));
      expect(screen.queryByText(`Showing only bot ${BOT}.`)).toBeNull();
    });

    it("names a user's target and says what the filter on them shows", async () => {
      const app = await auditScreen(async () => bothTargets);

      await app.user.click(await screen.findByRole("link", { name: "Only user afkbot1" }));

      expect(await screen.findByText("Showing only user afkbot1.")).toBeDefined();
      expect(
        screen.getByText("This shows what happened to this user, not everything they did."),
      ).toBeDefined();
    });

    it("says so when the target has no entries", async () => {
      const app = await auditScreen(async () => ({ items: [], unreadable: [] }));

      await act(() =>
        app.runtime.router.navigate({
          to: "/admin/audit",
          search: { kind: "bot", id: BOT },
        }),
      );

      expect(await screen.findByText("Nothing recorded for this target.")).toBeDefined();
      expect(screen.queryByText("Nothing recorded yet.")).toBeNull();
    });

    it("reads a search that's half given or malformed as the full log (R175)", async () => {
      const audit = vi.fn(async (): Promise<AuditPage> => ({ items: [entry(5)], unreadable: [] }));
      const app = await atScreen(
        "/admin/audit",
        { audit, users: async () => ({ items: [] }) },
        { user: ADMIN },
      );
      await waitFor(() => expect(rows()).toHaveLength(1));

      for (const search of [
        { kind: "bot" },
        { id: BOT },
        { kind: "Not-A-Kind", id: BOT },
        { kind: "bot", id: auditId(0xab).toUpperCase() },
      ]) {
        await act(() =>
          app.runtime.router.navigate({ to: "/admin/audit", search: search as never }),
        );

        expect(screen.queryByText(/Showing only/)).toBeNull();
      }
      expect(audit).not.toHaveBeenCalledWith(undefined, undefined, "bot", expect.anything());
    });

    it("opens from a user's row on the users' screen, for every row", async () => {
      const audit = vi.fn(async (): Promise<AuditPage> => ({ items: [], unreadable: [] }));
      const app = await atScreen(
        "/admin/users",
        {
          audit,
          users: async () => ({ items: [listedUser(USER, "afkbot1")] }),
        },
        { user: ADMIN },
      );

      await app.user.click(await screen.findByRole("link", { name: "Audit of afkbot1" }));

      expect(await screen.findByText("Showing only user afkbot1.")).toBeDefined();
      expect(audit).toHaveBeenCalledWith(undefined, undefined, "user", USER);
    });
  });

  it("answers fleet-client's no_session by ending the session, like any read", async () => {
    const app = await auditScreen(async () => {
      throw fakeApiError("no_session");
    });

    await waitFor(() => expect(app.path()).toBe("/login"));
  });
});
