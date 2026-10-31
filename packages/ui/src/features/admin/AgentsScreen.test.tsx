// The agents' screen (Plan.md P11.9; ADR-0019, ADR-0020, U76, U81, S49, R172, R177): every agent
// with its status, bots and certificate; disabling through the step-up with a text saying it's
// final; releasing a disabled agent's bots on the operator's word; `agent_not_disabled`; and no
// way to create an enrollment token.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { AGENTS_KEY } from "../../api/queries";
import type { AgentResponse } from "../../generated/AgentResponse";
import type { AuditPage } from "../../generated/AuditPage";
import { formatTime } from "../../lib/time";
import { deferred, me } from "../../test/fixtures";
import { atScreen } from "../../test/signedIn";
import { PASSWORD, refusal } from "../../test/signIn";

const ADMIN = me("admin", "afkadmin");

/** Agent `n`, enabled and online unless `overrides` say otherwise. */
function agent(n: number, overrides: Partial<AgentResponse> = {}): AgentResponse {
  return {
    id: `01900000-0000-7000-8000-0000000003${String(n).padStart(2, "0")}`,
    name: `agent-${n}`,
    enabled: true,
    status: "online",
    bots: 0,
    created_at: "2026-10-01T08:00:00.000Z",
    ...overrides,
  };
}

/** The screen over `agents`, as an Admin unless `handlers` change it. */
async function atAgents(agents: AgentResponse[], handlers: Partial<ApiClient> = {}) {
  return atScreen(
    "/admin/agents",
    { agents: async () => ({ items: agents }), ...handlers },
    { user: ADMIN },
  );
}

/** The row of the agent named `name`. */
async function row(name: string) {
  return within(await screen.findByRole("row", { name: new RegExp(name) }));
}

describe("the agents' screen", () => {
  it("shows each agent's name, status, bots, capacity, version and times", async () => {
    await atAgents([
      agent(1, {
        bots: 3,
        capacity: 10,
        version: "0.1.0",
        last_seen_at: "2026-10-02T08:00:00.000Z",
        not_after: "2026-12-01T08:00:00.000Z",
      }),
    ]);

    expect(await screen.findByRole("heading", { name: "Agents", level: 1 })).toBeDefined();
    const inRow = await row("agent-1");
    expect(inRow.getByText("Online")).toBeDefined();
    expect(inRow.getByText("3")).toBeDefined();
    expect(inRow.getByText("10")).toBeDefined();
    expect(inRow.getByText("0.1.0")).toBeDefined();
    expect(inRow.getByText(formatTime("2026-10-02T08:00:00.000Z"))).toBeDefined();
    expect(inRow.getByText(formatTime("2026-12-01T08:00:00.000Z"))).toBeDefined();
    expect(inRow.getByText(formatTime("2026-10-01T08:00:00.000Z"))).toBeDefined();
  });

  it("shows what the agent never said as nothing, and that it was never seen", async () => {
    await atAgents([agent(1, { status: "offline" })]);

    const inRow = await row("agent-1");
    expect(inRow.getByText("Offline")).toBeDefined();
    expect(inRow.getByText("Never")).toBeDefined();
  });

  it("shows expired apart from disabled, saying what it must do", async () => {
    await atAgents([
      agent(2, { status: "expired" }),
      agent(1, { status: "disabled", enabled: false }),
    ]);

    const expired = await row("agent-2");
    expect(expired.getByText("Certificate expired")).toBeDefined();
    expect(expired.getByText("It must enroll again before it can connect.")).toBeDefined();
    expect(expired.getByRole("button", { name: "Disable" })).toBeDefined();
    const disabled = await row("agent-1");
    expect(disabled.getByText("Disabled")).toBeDefined();
    expect(disabled.queryByText("It must enroll again before it can connect.")).toBeNull();
  });

  it("shows a status only a newer server sends as it was sent, and offers from enabled", async () => {
    await atAgents([
      agent(2, { status: "sleeping" as never }),
      agent(1, { status: "sleeping" as never, enabled: false, bots: 1 }),
    ]);

    const on = await row("agent-2");
    expect(on.getByText("sleeping")).toBeDefined();
    expect(on.getByRole("button", { name: "Disable" })).toBeDefined();
    expect(on.queryByRole("button", { name: "Release bots" })).toBeNull();
    const off = await row("agent-1");
    expect(off.queryByRole("button", { name: "Disable" })).toBeNull();
    expect(off.getByRole("button", { name: "Release bots" })).toBeDefined();
  });

  it("offers neither on a disabled agent with no bots", async () => {
    await atAgents([agent(1, { status: "disabled", enabled: false })]);

    const inRow = await row("agent-1");
    expect(inRow.queryByRole("button", { name: "Disable" })).toBeNull();
    expect(inRow.queryByRole("button", { name: "Release bots" })).toBeNull();
  });

  it("says how a disabled agent's bots wait, one or many", async () => {
    await atAgents([
      agent(2, { status: "disabled", enabled: false, bots: 2 }),
      agent(1, { status: "disabled", enabled: false, bots: 1 }),
    ]);

    expect((await row("agent-2")).getByText("2 bots wait here until released.")).toBeDefined();
    expect((await row("agent-1")).getByText("1 bot waits here until released.")).toBeDefined();
  });

  it("says how an agent is added, and offers no way to create a token (U76)", async () => {
    const app = await atAgents([]);

    expect(await screen.findByText("No agent has enrolled yet.")).toBeDefined();
    expect(
      screen.getByText(
        "To add an agent, run afkfleet-server agent create-enrollment-token on the server's host, and pipe the bundle into afkfleet-agent enroll on the agent's host.",
      ),
    ).toBeDefined();
    const main = within(screen.getByRole("main"));
    expect(main.queryByRole("button")).toBeNull();
    expect(app.fake.calls.map((call) => call.method)).toContain("agents");
  });

  it("shows why a read failed", async () => {
    await atAgents([], {
      agents: async () => {
        throw refusal(500, "internal_error", { message: "Something went wrong." });
      },
    });

    expect((await screen.findByRole("alert")).textContent).toContain(
      "Couldn't load the agents: Something went wrong.",
    );
    expect(screen.queryByText("No agent has enrolled yet.")).toBeNull();
  });

  it("pages with Show more", async () => {
    const agents = vi.fn(async (before?: string) =>
      before === undefined ? { items: [agent(2)], next: agent(2).id } : { items: [agent(1)] },
    );
    const app = await atAgents([], { agents });

    await app.user.click(await screen.findByRole("button", { name: "Show more" }));

    expect(await row("agent-1")).toBeDefined();
    expect(agents).toHaveBeenLastCalledWith(agent(2).id, undefined);
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("opens the audit log narrowed to an agent from its row (U81)", async () => {
    const audit = vi.fn(async (): Promise<AuditPage> => ({ items: [], unreadable: [] }));
    const app = await atAgents([agent(1)], { audit, users: async () => ({ items: [] }) });

    await app.user.click(await screen.findByRole("link", { name: "Audit of agent-1" }));

    expect(await screen.findByText(`Showing only agent ${agent(1).id}.`)).toBeDefined();
    expect(audit).toHaveBeenCalledWith(undefined, undefined, "agent", agent(1).id);
  });

  describe("disabling", () => {
    /** A stateful fake: the write answers after a step-up when `stepUpFirst`. */
    async function disabling(stepUpFirst = false, bots = 0) {
      let current = agent(1, { bots });
      let stamped = !stepUpFirst;
      const disableAgent = vi.fn(async () => {
        if (!stamped) {
          throw refusal(403, "step_up_required");
        }
        current = { ...current, enabled: false, status: "disabled" };
        return current;
      });
      const app = await atAgents([], {
        agents: async () => ({ items: [current] }),
        disableAgent,
        stepUp: async () => {
          stamped = true;
        },
      });
      return { app, disableAgent };
    }

    it("asks first, saying it's final, and Cancel sends nothing", async () => {
      const { app, disableAgent } = await disabling();

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Disable" }));

      expect(
        screen.getByText(
          "Disabling agent-1 is final: its certificates are revoked, it disconnects, and it can't come back. To use that machine again, enroll a new agent.",
        ),
      ).toBeDefined();
      await app.user.click(screen.getByRole("button", { name: "Cancel" }));
      expect(disableAgent).not.toHaveBeenCalled();
      expect(screen.queryByText(/is final/)).toBeNull();
    });

    it("says its bots stay with it, when it holds any", async () => {
      const { app } = await disabling(false, 2);

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Disable" }));

      expect(
        screen.getByText(
          "Disabling agent-1 is final: its certificates are revoked, it disconnects, and it can't come back. To use that machine again, enroll a new agent. Its bots stay with it until you release them.",
        ),
      ).toBeDefined();
    });

    it("asks for the password when the server wants it, sends once more, and reads again", async () => {
      const { app, disableAgent } = await disabling(true);

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Disable" }));
      await app.user.click(screen.getByRole("button", { name: "Disable for good" }));
      await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
      await app.user.click(screen.getByRole("button", { name: "Continue" }));

      expect(await screen.findByText("agent-1 is disabled.")).toBeDefined();
      expect(disableAgent).toHaveBeenCalledTimes(2);
      await waitFor(async () => expect((await row("agent-1")).getByText("Disabled")).toBeDefined());
    });

    it("shows a refusal at the row and reads the list again", async () => {
      const agents = vi.fn(async () => ({ items: [agent(1)] }));
      const app = await atAgents([], {
        agents,
        disableAgent: async () => {
          throw refusal(404, "not_found", { message: "It's gone." });
        },
      });

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Disable" }));
      const reads = agents.mock.calls.length;
      await app.user.click(screen.getByRole("button", { name: "Disable for good" }));

      expect((await screen.findByRole("alert")).textContent).toContain("It's gone.");
      await waitFor(() => expect(agents.mock.calls.length).toBeGreaterThan(reads));
    });
  });

  describe("releasing", () => {
    const held = (bots: number) => agent(1, { status: "disabled", enabled: false, bots });

    it("asks first, with the operator's word, and Cancel sends nothing", async () => {
      const releaseAgent = vi.fn(async () => held(0));
      const app = await atAgents([held(2)], { releaseAgent });

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Release bots" }));

      expect(
        screen.getByText(
          "Release 2 bots held by agent-1? Do this only once its host and its bots are stopped: you are vouching for that. A bot still running there could put the same account online twice.",
        ),
      ).toBeDefined();
      await app.user.click(screen.getByRole("button", { name: "Cancel" }));
      expect(releaseAgent).not.toHaveBeenCalled();
    });

    it("says one bot, when it holds one", async () => {
      const app = await atAgents([held(1)]);

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Release bots" }));

      expect(screen.getByText(/^Release 1 bot held by agent-1\?/)).toBeDefined();
    });

    it("releases after the password, says so and reads again", async () => {
      let current = held(2);
      let stamped = false;
      const app = await atAgents([], {
        agents: async () => ({ items: [current] }),
        stepUp: async () => {
          stamped = true;
        },
        releaseAgent: async () => {
          if (!stamped) {
            throw refusal(403, "step_up_required");
          }
          current = held(0);
          return current;
        },
      });

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Release bots" }));
      const question = screen.getByText(/^Release 2 bots/).closest("div") as HTMLElement;
      await app.user.click(within(question).getByRole("button", { name: "Release bots" }));
      await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
      await app.user.click(screen.getByRole("button", { name: "Continue" }));

      expect(await screen.findByText("agent-1's bots are released.")).toBeDefined();
      await waitFor(async () =>
        expect((await row("agent-1")).queryByRole("button", { name: "Release bots" })).toBeNull(),
      );
    });

    it("shows agent_not_disabled's message and reads the agent again", async () => {
      const MESSAGE = "Only a disabled agent's bots can be released. Disable the agent first.";
      const agents = vi.fn(async () => ({ items: [held(1)] }));
      const app = await atAgents([], {
        agents,
        releaseAgent: async () => {
          throw refusal(409, "agent_not_disabled", { message: MESSAGE });
        },
      });

      await app.user.click((await row("agent-1")).getByRole("button", { name: "Release bots" }));
      const reads = agents.mock.calls.length;
      const question = screen.getByText(/^Release 1 bot/).closest("div") as HTMLElement;
      await app.user.click(within(question).getByRole("button", { name: "Release bots" }));

      expect((await screen.findByRole("alert")).textContent).toContain(MESSAGE);
      await waitFor(() => expect(agents.mock.calls.length).toBeGreaterThan(reads));
    });
  });

  it("builds nothing from a write that answers after the session's end (S87)", async () => {
    const late = deferred<AgentResponse>();
    const app = await atAgents([agent(1)], {
      disableAgent: () => late.promise,
      sessions: async () => {
        throw refusal(401, "unauthorized");
      },
    });
    await app.user.click((await row("agent-1")).getByRole("button", { name: "Disable" }));
    await app.user.click(screen.getByRole("button", { name: "Disable for good" }));

    await act(async () => {
      await app.runtime.router.navigate({ to: "/login" });
      app.runtime.queryClient.clear();
    });
    await act(async () => late.resolve(agent(1, { enabled: false })));

    expect(app.runtime.queryClient.getQueryCache().findAll({ queryKey: AGENTS_KEY })).toEqual([]);
  });
});
