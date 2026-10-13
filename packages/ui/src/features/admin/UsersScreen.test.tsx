import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import type { MeResponse } from "../../generated/MeResponse";
import type { UserPatchRequest } from "../../generated/UserPatchRequest";
import type { UserResponse } from "../../generated/UserResponse";
import type { UserRole } from "../../generated/UserRole";
import { formatTime } from "../../lib/time";
import { me } from "../../test/fixtures";
import { atScreen } from "../../test/signedIn";
import { cacheText, PASSWORD, refusal } from "../../test/signIn";

const OWNER_ID = "01900000-0000-7000-8000-000000000001";

/** A user numbered `n` with `role`, last seen unless `seen` is false. */
function person(
  n: number,
  username: string,
  role: UserRole,
  changes: Partial<Pick<UserResponse, "disabled" | "mfa_enrolled">> = {},
  seen = true,
): UserResponse {
  const user: UserResponse = {
    id: `01900000-0000-7000-8000-0000000000${String(n).padStart(2, "0")}`,
    username,
    role,
    disabled: false,
    created_at: `2026-10-0${n}T08:00:00.000Z`,
    mfa_enrolled: true,
    ...changes,
  };
  return seen ? { ...user, last_seen_at: `2026-10-1${n}T08:00:00.000Z` } : user;
}

const SELF = person(1, "afkowner", "owner");
const ADMIN = person(2, "afkadmin", "admin");
const MEMBER = person(3, "afkbot3", "member", { mfa_enrolled: false }, false);
const GONE = person(4, "afkbot4", "member", { disabled: true });
const ALL = [GONE, MEMBER, ADMIN, SELF];

/** Who is signed in: the Owner by default. */
function actor(role: UserRole = "owner", username = "afkowner"): MeResponse {
  return { ...me(role, username), id: role === "owner" ? OWNER_ID : ADMIN.id };
}

/**
 * The users screen over a stateful fake: `patchUser` applies the change and answers the
 * user, after a step-up when `stepUpFirst` (the server's stamp, held for the test).
 */
async function atUsers(
  options: { me?: MeResponse; stepUpFirst?: boolean; handlers?: Partial<ApiClient> } = {},
) {
  let users = [...ALL];
  let stamped = options.stepUpFirst !== true;
  const app = await atScreen(
    "/admin/users",
    {
      users: async () => ({ items: users }),
      stepUp: async ({ password }) => {
        if (password !== PASSWORD) {
          throw refusal(403, "forbidden");
        }
        stamped = true;
      },
      patchUser: async (id: string, request: UserPatchRequest) => {
        if (!stamped) {
          throw refusal(403, "step_up_required");
        }
        users = users.map((user) => (user.id === id ? { ...user, ...request } : user));
        const changed = users.find((user) => user.id === id);
        if (changed === undefined) {
          throw refusal(404, "not_found");
        }
        return changed;
      },
      ...options.handlers,
    },
    { user: options.me ?? actor() },
  );
  await screen.findByRole("table", { name: "Users" });
  return app;
}

type App = Awaited<ReturnType<typeof atUsers>>;

/** The row of `username`. */
function row(username: string) {
  const cell = within(screen.getByRole("table", { name: "Users" })).getByText(
    new RegExp(`^${username}( \\(you\\))?$`),
  );
  return within(cell.closest("tr") as HTMLElement);
}

/** The buttons a row offers. */
function actions(username: string): string[] {
  return row(username)
    .queryAllByRole("button")
    .map((button) => button.textContent ?? "");
}

function patches(app: App): unknown[][] {
  return app.fake.calls.filter((call) => call.method === "patchUser").map((call) => [...call.args]);
}

describe("the users screen", () => {
  it("lists every user with their facts, marking you", async () => {
    await atUsers();

    const self = row("afkowner");
    expect(self.getByText("afkowner (you)")).toBeDefined();
    expect(self.getByText("Owner")).toBeDefined();
    const member = row("afkbot3");
    expect(member.getByText("Member")).toBeDefined();
    expect(member.getByText("Not set up")).toBeDefined();
    expect(member.getByText("Never")).toBeDefined();
    expect(member.getByText(formatTime(MEMBER.created_at))).toBeDefined();
    expect(row("afkadmin").getByText(formatTime(ADMIN.last_seen_at ?? ""))).toBeDefined();
    expect(row("afkbot4").getByText("Disabled")).toBeDefined();
    expect(row("afkadmin").getByText("Active")).toBeDefined();
  });

  it("offers the Owner a role change and disabling on Members and Admins, nothing on themselves", async () => {
    await atUsers();

    expect(actions("afkowner")).toEqual([]);
    expect(actions("afkadmin")).toEqual(["Make member", "Disable"]);
    expect(actions("afkbot3")).toEqual(["Make admin", "Disable"]);
    expect(actions("afkbot4")).toEqual(["Make admin", "Enable"]);
  });

  it("offers an Admin only disabling and enabling Members", async () => {
    await atUsers({ me: actor("admin", "afkadmin") });

    expect(actions("afkowner")).toEqual([]);
    expect(actions("afkadmin")).toEqual([]);
    expect(actions("afkbot3")).toEqual(["Disable"]);
    expect(actions("afkbot4")).toEqual(["Enable"]);
  });

  it("shows more users on request", async () => {
    const app = await atUsers({
      handlers: {
        users: async (before) =>
          before === undefined
            ? { items: [GONE, MEMBER], next: MEMBER.id }
            : { items: [ADMIN, SELF] },
      },
    });
    expect(screen.queryByText("afkadmin")).toBeNull();

    await app.user.click(screen.getByRole("button", { name: "Show more" }));

    expect(await screen.findByText("afkadmin")).toBeDefined();
  });

  it("asks before making a Member an Admin, and Cancel sends nothing", async () => {
    const app = await atUsers();

    await app.user.click(row("afkbot3").getByRole("button", { name: "Make admin" }));
    expect(screen.getByText("Making afkbot3 an Admin logs them out everywhere.")).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Cancel" }));

    expect(patches(app)).toEqual([]);
  });

  it("makes a Member an Admin once confirmed, and shows the list again", async () => {
    const app = await atUsers();

    await app.user.click(row("afkbot3").getByRole("button", { name: "Make admin" }));
    await app.user.click(
      within(screen.getByRole("alert")).getByRole("button", { name: "Make admin" }),
    );

    expect(await screen.findByText("afkbot3 is now an Admin.")).toBeDefined();
    expect(patches(app)).toEqual([[MEMBER.id, { role: "admin" }]]);
    await waitFor(() => expect(row("afkbot3").getByText("Admin")).toBeDefined());
  });

  it("names the revoked invites when making an Admin a Member", async () => {
    const app = await atUsers();

    await app.user.click(row("afkadmin").getByRole("button", { name: "Make member" }));

    expect(
      screen.getByText(
        "Making afkadmin a Member logs them out everywhere and revokes their pending invites.",
      ),
    ).toBeDefined();
  });

  it("asks before disabling, naming what it ends", async () => {
    const app = await atUsers();

    await app.user.click(row("afkbot3").getByRole("button", { name: "Disable" }));
    expect(
      screen.getByText(
        "Disabling afkbot3 logs them out everywhere and revokes their pending invites. They can't log in until they're enabled again.",
      ),
    ).toBeDefined();
    await app.user.click(
      within(screen.getByRole("alert")).getByRole("button", { name: "Disable" }),
    );

    expect(await screen.findByText("afkbot3 is disabled.")).toBeDefined();
    expect(patches(app)).toEqual([[MEMBER.id, { disabled: true }]]);
  });

  it("enables at once", async () => {
    const app = await atUsers();

    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));

    expect(await screen.findByText("afkbot4 is enabled.")).toBeDefined();
    expect(patches(app)).toEqual([[GONE.id, { disabled: false }]]);
  });

  it("shows the server's refusal and lists the users again", async () => {
    const app = await atUsers({
      handlers: {
        patchUser: async () => {
          throw refusal(404, "not_found");
        },
      },
    });
    const lists = app.fake.calls.filter((call) => call.method === "users").length;

    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));

    expect(await screen.findByText(/refused: not_found/)).toBeDefined();
    await waitFor(() =>
      expect(app.fake.calls.filter((call) => call.method === "users").length).toBeGreaterThan(
        lists,
      ),
    );
  });
});

describe("a change that needs a step-up", () => {
  it("asks for the password at the row, then sends the change once more", async () => {
    const app = await atUsers({ stepUpFirst: true });

    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));
    const prompt = await screen.findByLabelText("Current password");
    await app.user.type(prompt, PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("afkbot4 is enabled.")).toBeDefined();
    expect(app.fake.calls.find((call) => call.method === "stepUp")?.args).toEqual([
      { password: PASSWORD },
    ]);
    expect(patches(app)).toEqual([
      [GONE.id, { disabled: false }],
      [GONE.id, { disabled: false }],
    ]);
    expect(screen.queryByLabelText("Current password")).toBeNull();
  });

  it("doesn't ask again while the server's step-up holds", async () => {
    const app = await atUsers({ stepUpFirst: true });
    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByText("afkbot4 is enabled.");

    await app.user.click(row("afkbot3").getByRole("button", { name: "Disable" }));
    await app.user.click(
      within(screen.getByRole("alert")).getByRole("button", { name: "Disable" }),
    );

    expect(await screen.findByText("afkbot3 is disabled.")).toBeDefined();
    expect(app.fake.calls.filter((call) => call.method === "stepUp")).toHaveLength(1);
  });

  it("stays on a wrong password, sending no change", async () => {
    const app = await atUsers({ stepUpFirst: true });
    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));

    await app.user.type(await screen.findByLabelText("Current password"), "not the password");
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("Wrong password.")).toBeDefined();
    expect(patches(app)).toHaveLength(1);
  });

  it("shows a retry the server refuses again, and never asks twice for one change", async () => {
    const app = await atUsers({
      stepUpFirst: true,
      handlers: {
        patchUser: async () => {
          throw refusal(403, "step_up_required");
        },
      },
    });
    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);

    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText(/refused: step_up_required/)).toBeDefined();
    expect(screen.queryByLabelText("Current password")).toBeNull();
    expect(patches(app)).toHaveLength(2);
  });

  it("sends nothing more when the prompt is cancelled", async () => {
    const app = await atUsers({ stepUpFirst: true });
    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));
    await screen.findByLabelText("Current password");

    await app.user.click(screen.getByRole("button", { name: "Cancel" }));

    expect(screen.queryByLabelText("Current password")).toBeNull();
    expect(patches(app)).toHaveLength(1);
    expect(app.fake.calls.filter((call) => call.method === "stepUp")).toHaveLength(0);
  });

  it("keeps the step-up's password out of the cache", async () => {
    const app = await atUsers({ stepUpFirst: true });
    await app.user.click(row("afkbot4").getByRole("button", { name: "Enable" }));
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByText("afkbot4 is enabled.");

    expect(cacheText(app)).not.toContain(PASSWORD);
  });
});
