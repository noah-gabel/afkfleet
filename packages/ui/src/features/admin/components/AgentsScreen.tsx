// The agents' screen (Plan.md P11.9; ADR-0019, ADR-0020, U76, U81, S49, R172, R177), for Admins
// and the Owner: every agent, newest first, a page at a time, with its status, the bots it holds
// and its certificate. Each row offers only what the server would allow (permissions.ts).
// - Disabling is final and asks first; releasing a disabled agent's bots asks first too, since
//   whoever releases vouches that its host and its bots are stopped (S49). Both need a step-up,
//   which the row asks for when the server wants it (useStepUpAction).
// - A token to add an agent comes only from the server's host (U76), so nothing here creates one.
// Nothing here carries a secret but the step-up's password, which StepUpForm holds: the list is a
// query and each change a mutation, and every answer only reads the list again (S87).
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Fragment, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { AGENTS_KEY, useAgents, useMe } from "../../../api/queries";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { AgentResponse } from "../../../generated/AgentResponse";
import { formatTime } from "../../../lib/time";
import {
  AGENT_COLUMNS,
  AGENTS_HEADING,
  AGENTS_INTRO,
  AUDIT_LINK,
  agentStatusLabel,
  agentsFailed,
  auditOf,
  botsWait,
  DISABLE,
  DISABLE_FOR_GOOD,
  disabled,
  disableQuestion,
  EXPIRED_HINT,
  NEVER_SEEN,
  NO_AGENTS,
  RELEASE_BOTS,
  released,
  releaseQuestion,
} from "../agentTexts";
import { useStepUpAction } from "../hooks/useStepUpAction";
import { agentActions } from "../permissions";
import { StepUpPrompt } from "./StepUpPrompt";

/** One change a row offers. */
type Change = "disable" | "release";

/** What asking before `change` says. */
function question(change: Change, agent: AgentResponse): string {
  return change === "disable"
    ? disableQuestion(agent.name, agent.bots)
    : releaseQuestion(agent.name, agent.bots);
}

/** `/admin/agents`. */
export function AgentsScreen() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const { data: actor } = useMe();
  const agents = useAgents();
  const refetch = () => void queryClient.invalidateQueries({ queryKey: AGENTS_KEY });
  const writes = useStepUpAction({ onRefused: refetch });
  const disable = useMutation({
    mutationFn: (id: string) => api.disableAgent(id),
    onSettled: refetch,
  });
  const release = useMutation({
    mutationFn: (id: string) => api.releaseAgent(id),
    onSettled: refetch,
  });
  const [asking, setAsking] = useState<{ id: string; change: Change } | null>(null);
  const rows = agents.data?.pages.flatMap((page) => page.items) ?? [];
  const busy = writes.running !== null || writes.prompt !== null;

  function run(agent: AgentResponse, change: Change): void {
    setAsking(null);
    void writes.attempt({
      key: agent.id,
      run: async () => {
        if (change === "disable") {
          await disable.mutateAsync(agent.id);
          toast.success(disabled(agent.name));
        } else {
          await release.mutateAsync(agent.id);
          toast.success(released(agent.name));
        }
      },
    });
  }

  return (
    <main className="flex flex-col gap-4 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">{AGENTS_HEADING}</h1>
      <p className="text-sm text-muted-foreground">{AGENTS_INTRO}</p>
      {agents.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {agentsFailed(describeError(toApiError(agents.error).error))}
        </p>
      ) : null}
      {agents.isSuccess && rows.length === 0 ? (
        <p>{NO_AGENTS}</p>
      ) : (
        <table aria-label={AGENTS_HEADING} className="w-full text-left text-sm">
          <thead className="text-muted-foreground">
            <tr>
              {AGENT_COLUMNS.map((column) => (
                <th key={column} className="py-1 pr-3 font-medium">
                  {column}
                </th>
              ))}
              <th className="py-1 font-medium">
                <span className="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((agent) => {
              const offered =
                actor === undefined
                  ? { disable: false, release: false }
                  : agentActions(agent, actor.role);
              const pending = asking?.id === agent.id ? asking.change : null;
              const failure = writes.failure?.key === agent.id ? writes.failure.message : undefined;
              return (
                <Fragment key={agent.id}>
                  <tr className="border-t align-top">
                    <td className="max-w-48 overflow-hidden py-2 pr-3 break-words">{agent.name}</td>
                    <td className="py-2 pr-3">
                      {agentStatusLabel(agent.status)}
                      {agent.status === "expired" ? (
                        <span className="block text-xs text-muted-foreground">{EXPIRED_HINT}</span>
                      ) : null}
                      {!agent.enabled && agent.bots > 0 ? (
                        <span className="block text-xs text-muted-foreground">
                          {botsWait(agent.bots)}
                        </span>
                      ) : null}
                    </td>
                    <td className="py-2 pr-3">{agent.bots}</td>
                    <td className="py-2 pr-3">{agent.capacity ?? ""}</td>
                    <td className="max-w-32 overflow-hidden py-2 pr-3 break-words">
                      {agent.version ?? ""}
                    </td>
                    <td className="py-2 pr-3">
                      {agent.last_seen_at === undefined
                        ? NEVER_SEEN
                        : formatTime(agent.last_seen_at)}
                    </td>
                    <td className="py-2 pr-3">
                      {agent.not_after === undefined ? "" : formatTime(agent.not_after)}
                    </td>
                    <td className="py-2 pr-3">{formatTime(agent.created_at)}</td>
                    <td className="py-2">
                      <div className="flex justify-end gap-2">
                        <Button asChild size="sm" variant="outline">
                          <Link
                            to="/admin/audit"
                            search={{ kind: "agent", id: agent.id }}
                            aria-label={auditOf(agent.name)}
                          >
                            {AUDIT_LINK}
                          </Link>
                        </Button>
                        {offered.disable ? (
                          <Button
                            type="button"
                            size="sm"
                            variant="outline"
                            disabled={busy}
                            onClick={() => setAsking({ id: agent.id, change: "disable" })}
                          >
                            {DISABLE}
                          </Button>
                        ) : null}
                        {offered.release ? (
                          <Button
                            type="button"
                            size="sm"
                            variant="outline"
                            disabled={busy}
                            onClick={() => setAsking({ id: agent.id, change: "release" })}
                          >
                            {RELEASE_BOTS}
                          </Button>
                        ) : null}
                      </div>
                    </td>
                  </tr>
                  {pending === null &&
                  writes.prompt !== agent.id &&
                  failure === undefined ? null : (
                    <tr>
                      <td colSpan={AGENT_COLUMNS.length + 1} className="pb-3">
                        {pending === null ? null : (
                          <Alert>
                            <AlertDescription className="flex flex-col gap-2 text-foreground">
                              <p>{question(pending, agent)}</p>
                              <div className="flex gap-2">
                                <Button
                                  type="button"
                                  variant="destructive"
                                  disabled={busy}
                                  onClick={() => run(agent, pending)}
                                >
                                  {pending === "disable" ? DISABLE_FOR_GOOD : RELEASE_BOTS}
                                </Button>
                                <Button
                                  type="button"
                                  variant="outline"
                                  onClick={() => setAsking(null)}
                                >
                                  Cancel
                                </Button>
                              </div>
                            </AlertDescription>
                          </Alert>
                        )}
                        {writes.prompt === agent.id ? (
                          <StepUpPrompt
                            id={`step-up-${agent.id}`}
                            pending={writes.running !== null}
                            onConfirm={writes.confirm}
                            onCancel={writes.cancel}
                          />
                        ) : null}
                        {failure === undefined ? null : (
                          <p role="alert" className="text-sm text-destructive">
                            {failure}
                          </p>
                        )}
                      </td>
                    </tr>
                  )}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      )}
      {agents.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={agents.isFetchingNextPage}
          onClick={() => void agents.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}
    </main>
  );
}
