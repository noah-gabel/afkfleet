import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import type { CreatedInviteResponse } from "../../generated/CreatedInviteResponse";
import type { InviteResponse } from "../../generated/InviteResponse";
import type { InviteState } from "../../generated/InviteState";
import type { MeResponse } from "../../generated/MeResponse";
import type { UserRole } from "../../generated/UserRole";
import { formatTime, formatTimeWithZone } from "../../lib/time";
import { deferred, me } from "../../test/fixtures";
import { atScreen } from "../../test/signedIn";
import { cacheText, refusal } from "../../test/signIn";
import { CODE_COPIED, INVITATION_COPIED, INVITE_CAP } from "./components/InvitesScreen";
import { invitationText } from "./invitation";

const OWNER = { id: "01900000-0000-7000-8000-000000000001", username: "afkowner" };
const CODE = "afk_iv_placeholderplaceholderplaceholder00";

/** An invite numbered `n` for `role` in `state`. */
function invite(n: number, role: UserRole, state: InviteState): InviteResponse {
  const base: InviteResponse = {
    id: `01900000-0000-7000-8000-0000000001${String(n).padStart(2, "0")}`,
    role,
    state,
    created_by: OWNER,
    created_at: `2026-10-1${n}T08:00:00.000Z`,
    expires_at: `2026-10-2${n}T08:00:00.000Z`,
  };
  return state === "used"
    ? {
        ...base,
        used_by: { id: "01900000-0000-7000-8000-000000000003", username: "afkbot3" },
        used_at: `2026-10-1${n}T09:00:00.000Z`,
      }
    : base;
}

const PENDING_MEMBER = invite(1, "member", "pending");
const PENDING_ADMIN = invite(2, "admin", "pending");
const USED = invite(3, "member", "used");
const EXPIRED = invite(4, "member", "expired");
const CREATED: CreatedInviteResponse = { code: CODE, invite: invite(5, "admin", "pending") };

function actor(role: UserRole = "owner"): MeResponse {
  return { ...me(role, role === "owner" ? "afkowner" : "afkadmin"), id: OWNER.id };
}

/** The invites screen, the Owner signed in. It has no `stepUp`: a call to it would fail the test. */
async function atInvites(options: { me?: MeResponse; handlers?: Partial<ApiClient> } = {}) {
  const app = await atScreen(
    "/admin/invites",
    {
      invites: async () => ({ items: [EXPIRED, USED, PENDING_ADMIN, PENDING_MEMBER] }),
      createInvite: async () => CREATED,
      revokeInvite: async () => {},
      ...options.handlers,
    },
    { user: options.me ?? actor() },
  );
  await screen.findByRole("table", { name: "Invites" });
  return app;
}

type App = Awaited<ReturnType<typeof atInvites>>;

/** The row of the invite created at `created`. */
function row(created: InviteResponse) {
  const cell = within(screen.getByRole("table", { name: "Invites" })).getByText(
    formatTime(created.created_at),
  );
  return within(cell.closest("tr") as HTMLElement);
}

function called(app: App, method: keyof ApiClient): unknown[][] {
  return app.fake.calls.filter((call) => call.method === method).map((call) => [...call.args]);
}

/** Opens the new invite's box, unless it's open (U123). */
async function openBox(app: App) {
  if (screen.queryByRole("button", { name: "Create invite" }) === null) {
    await app.user.click(screen.getByRole("button", { name: "New invite" }));
  }
}

async function create(app: App, role?: "Member" | "Admin") {
  await openBox(app);
  if (role !== undefined) {
    await app.user.click(screen.getByLabelText(role));
  }
  await app.user.click(screen.getByRole("button", { name: "Create invite" }));
}

describe("the invites screen", () => {
  it("lists every invite in its state, with its creator, its times and who used it", async () => {
    await atInvites();

    expect(row(PENDING_MEMBER).getByText("Pending")).toBeDefined();
    expect(row(PENDING_MEMBER).getByText("Member")).toBeDefined();
    expect(row(PENDING_ADMIN).getByText("Admin")).toBeDefined();
    expect(row(PENDING_MEMBER).getByText("afkowner")).toBeDefined();
    expect(row(PENDING_MEMBER).getByText(formatTime(PENDING_MEMBER.expires_at))).toBeDefined();
    expect(row(USED).getByText("Used")).toBeDefined();
    expect(row(USED).getByText("afkbot3")).toBeDefined();
    expect(row(EXPIRED).getByText("Expired")).toBeDefined();
  });

  it("opens the new invite's box from New invite, at the top right, and Cancel closes it (U123)", async () => {
    const app = await atInvites();
    const button = screen.getByRole("button", { name: "New invite" });
    expect(screen.queryByRole("button", { name: "Create invite" })).toBeNull();
    expect(button.getAttribute("aria-expanded")).toBe("false");

    await app.user.click(button);

    const box = screen.getByRole("region", { name: "New invite" });
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(within(box).getByRole("button", { name: "Create invite" })).toBeDefined();
    await app.user.click(within(box).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("region", { name: "New invite" })).toBeNull();
    expect(called(app, "createInvite")).toEqual([]);
  });

  it("shows each state as a status line in its tone (R229)", async () => {
    await atInvites();

    const line = (created: InviteResponse, word: string) =>
      row(created).getByText(word).closest("[data-slot=status-line]")?.className;
    expect(line(PENDING_MEMBER, "Pending")).toContain("text-info");
    expect(line(USED, "Used")).toContain("text-success");
    expect(line(EXPIRED, "Expired")).toContain("text-muted-foreground");
  });

  it("says so while the invites load, as a status", async () => {
    await atScreen("/admin/invites", { invites: () => new Promise(() => {}) }, { user: actor() });
    expect((await screen.findByText("Loading invites…")).getAttribute("role")).toBe("status");
  });

  it("says there are no invites", async () => {
    await atScreen("/admin/invites", { invites: async () => ({ items: [] }) }, { user: actor() });
    expect(await screen.findByText("No invites.")).toBeDefined();
  });

  it("says why the invites couldn't be read", async () => {
    await atScreen(
      "/admin/invites",
      {
        invites: async () => {
          throw refusal(500, "internal");
        },
      },
      { user: actor() },
    );
    expect((await screen.findByRole("alert")).textContent).toMatch(/^Couldn't load invites: /);
  });

  it("offers Revoke only on the pending invites the actor may revoke", async () => {
    await atInvites({ me: actor("admin") });

    expect(row(PENDING_MEMBER).queryByRole("button", { name: "Revoke" })).not.toBeNull();
    expect(row(PENDING_ADMIN).queryByRole("button", { name: "Revoke" })).toBeNull();
    expect(row(USED).queryByRole("button", { name: "Revoke" })).toBeNull();
    expect(row(EXPIRED).queryByRole("button", { name: "Revoke" })).toBeNull();
  });

  it("asks before revoking, then revokes and lists again", async () => {
    const app = await atInvites();

    await app.user.click(row(PENDING_MEMBER).getByRole("button", { name: "Revoke" }));
    expect(screen.getByText("Revoke this invite? Its code then can't register.")).toBeDefined();
    await app.user.click(within(screen.getByRole("alert")).getByRole("button", { name: "Revoke" }));

    expect(await screen.findByText("Invite revoked.")).toBeDefined();
    expect(called(app, "revokeInvite")).toEqual([[PENDING_MEMBER.id]]);
  });

  it("lets the Owner choose Member or Admin", async () => {
    const app = await atInvites();

    await create(app, "Admin");

    await screen.findByText(CODE);
    expect(called(app, "createInvite")).toEqual([[{ role: "admin" }]]);
  });

  it("gives an Admin no choice: the invite is for a Member", async () => {
    const app = await atInvites({ me: actor("admin") });
    await openBox(app);

    expect(screen.getByText("For a Member.")).toBeDefined();
    expect(screen.queryByLabelText("Admin")).toBeNull();
    await create(app);

    await screen.findByText(CODE);
    expect(called(app, "createInvite")).toEqual([[{ role: "member" }]]);
  });

  it("never asks for the password (ADR-0028), and shows the code once", async () => {
    const app = await atInvites();
    await create(app);

    expect(await screen.findByText(CODE)).toBeDefined();
    expect(screen.queryByLabelText("Current password")).toBeNull();
    expect(screen.getByText(/Shown only now/)).toBeDefined();
    expect(
      screen.getByText(`Works once, until ${formatTimeWithZone(CREATED.invite.expires_at)}.`),
    ).toBeDefined();
    expect(called(app, "createInvite")).toHaveLength(1);
    expect(called(app, "stepUp")).toHaveLength(0);
  });

  it("copies the invitation and the code, each with the clipboard note", async () => {
    const app = await atInvites();
    await create(app);
    await screen.findByText(CODE);

    await app.user.click(screen.getByRole("button", { name: "Copy invitation" }));
    expect(await screen.findByText(INVITATION_COPIED)).toBeDefined();
    expect(await navigator.clipboard.readText()).toBe(
      invitationText({
        origin: "https://example.com",
        code: CODE,
        expiresAt: CREATED.invite.expires_at,
      }),
    );
    await app.user.click(screen.getByRole("button", { name: "Copy code" }));
    // Both say the same (U96), so the second is a second toast.
    await waitFor(() => expect(screen.getAllByText(CODE_COPIED)).toHaveLength(2));
    expect(await navigator.clipboard.readText()).toBe(CODE);
  });

  it("drops the code on Done, and when the screen is left", async () => {
    const app = await atInvites();
    await create(app);
    await screen.findByText(CODE);

    await app.user.click(screen.getByRole("button", { name: "Done" }));
    expect(screen.queryByText(CODE)).toBeNull();
    await create(app);
    await screen.findByText(CODE);
    await act(() => app.runtime.router.navigate({ to: "/home" }));
    await act(() => app.runtime.router.navigate({ to: "/admin/invites" }));
    await screen.findByRole("table", { name: "Invites" });

    expect(screen.queryByText(CODE)).toBeNull();
  });

  it("keeps the code out of the cache", async () => {
    const app = await atInvites();
    await create(app);
    await screen.findByText(CODE);
    await waitFor(() => expect(called(app, "invites").length).toBeGreaterThan(1));

    expect(cacheText(app)).not.toContain(CODE);
  });

  it("says why when 20 invites are pending", async () => {
    const app = await atInvites({
      handlers: {
        createInvite: async () => {
          throw refusal(409, "conflict");
        },
      },
    });

    await create(app);

    expect(await screen.findByText(INVITE_CAP)).toBeDefined();
  });

  it("keeps Cancel waiting while a create runs (PR #92's review)", async () => {
    const answer = deferred<CreatedInviteResponse>();
    const app = await atInvites({ handlers: { createInvite: () => answer.promise } });

    await create(app);
    const cancel = screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement;
    await waitFor(() => expect(cancel.disabled).toBe(true));
    await act(async () => answer.resolve(CREATED));

    expect(await screen.findByText(CODE)).toBeDefined();
  });

  it("drops an earlier failure when Cancel closes the box (PR #92's review)", async () => {
    const app = await atInvites({
      handlers: {
        createInvite: async () => {
          throw refusal(409, "conflict");
        },
      },
    });
    await create(app);
    await screen.findByText(INVITE_CAP);

    await app.user.click(screen.getByRole("button", { name: "Cancel" }));
    await openBox(app);

    expect(screen.queryByText(INVITE_CAP)).toBeNull();
  });

  it("names the new invite's role with its article: an Admin, a Member (PR #92's review)", async () => {
    const app = await atInvites();
    await create(app, "Admin");

    expect(await screen.findByText("New invite for an Admin")).toBeDefined();
  });

  it("shows more invites on request", async () => {
    const app = await atInvites({
      handlers: {
        invites: async (before) =>
          before === undefined
            ? { items: [EXPIRED, USED], next: USED.id }
            : { items: [PENDING_ADMIN, PENDING_MEMBER] },
      },
    });

    await app.user.click(screen.getByRole("button", { name: "Show more" }));

    expect(await screen.findByText(formatTime(PENDING_MEMBER.created_at))).toBeDefined();
  });
});
