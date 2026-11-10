// The agents' screen (Plan.md P11.9, P11.11; ADR-0019, ADR-0020, U76, U81, U122, S49, R172, R177,
// R229), for Admins and the Owner: every agent, newest first, a page at a time, with its status, the
// bots it holds and its certificate. Each row offers only what the server would allow
// (permissions.ts).
// - Disabling is final and asks first; releasing a disabled agent's bots asks first too, since
//   whoever releases vouches that its host and its bots are stopped (S49). Both need a step-up,
//   which the row asks for when the server wants it (useStepUpAction).
// - A token to add an agent comes only from the server's host (U76), so nothing here creates one.
// - An enabled agent whose certificate ends within 14 days says so under its status, which takes the
//   warning tone, as "Certificate until" drops in a small window (the user's choice, PR #92's
//   review); the time is read again each minute. A certificate renews by itself long before that
//   (P13.6, ADR-0027), so the line means the agent couldn't: it says what lets it.
// - In a small window the table drops Enrolled, then Certificate until, then Version, then Can run
//   (U91, U122), at widths that never show a column, hide it and show it again (R228). A name
//   keeps 12 characters of width, or the expired hint's column takes it and a name as short as
//   agent-main breaks.
// Nothing here carries a secret but the step-up's password, which StepUpForm holds: the list is a
// query and each change a mutation, and a success or a refusal only reads the list again, once
// (S87, R177). A step-up isn't a refusal, so it reads nothing before the prompt.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Fragment, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { AGENTS_KEY, useAgents, useMe } from "../../../api/queries";
import { ScreenState } from "../../../components/ScreenState";
import { StatusLine } from "../../../components/StatusLine";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableSpanRow,
} from "../../../components/ui/table";
import type { AgentResponse } from "../../../generated/AgentResponse";
import { useNow } from "../../../hooks/useNow";
import { formatTime } from "../../../lib/time";
import {
  AGENTS_HEADING,
  AUDIT_LINK,
  agentStatusLabel,
  agentStatusTone,
  agentsFailed,
  auditOf,
  botsWait,
  certificateWarning,
  DISABLE,
  DISABLE_FOR_GOOD,
  disabled,
  disableQuestion,
  ENROLL_COMMANDS,
  EXPIRED_HINT,
  LOADING_AGENTS,
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

/** The table's columns, which a row's question spans. */
const COLUMNS = 9;

/** What asking before `change` says. */
function question(change: Change, agent: AgentResponse): string {
  return change === "disable"
    ? disableQuestion(agent.name, agent.bots)
    : releaseQuestion(agent.name, agent.bots);
}

/** How an agent is added, its two commands in mono (U76). */
function Intro() {
  return (
    <p className="text-sm text-muted-foreground">
      To add one, run <code className="font-mono text-foreground">{ENROLL_COMMANDS.token}</code> on
      the server's host and pipe the bundle into{" "}
      <code className="font-mono text-foreground">{ENROLL_COMMANDS.enroll}</code> on the agent's
      host.
    </p>
  );
}

/** `/admin/agents`. */
export function AgentsScreen() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const { data: actor } = useMe();
  const agents = useAgents();
  const now = useNow(() => 60_000);
  const refetch = () => void queryClient.invalidateQueries({ queryKey: AGENTS_KEY });
  const writes = useStepUpAction({ onRefused: refetch });
  const disable = useMutation({
    mutationFn: (id: string) => api.disableAgent(id),
    onSuccess: refetch,
  });
  const release = useMutation({
    mutationFn: (id: string) => api.releaseAgent(id),
    onSuccess: refetch,
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
    <main className="flex flex-1 flex-col gap-4 p-4">
      <h1 className="text-lg font-semibold tracking-tight">{AGENTS_HEADING}</h1>
      <Intro />
      {agents.isError ? (
        <ScreenState kind="error">
          {agentsFailed(describeError(toApiError(agents.error).error))}
        </ScreenState>
      ) : null}
      {agents.isPending ? <ScreenState kind="loading">{LOADING_AGENTS}</ScreenState> : null}
      {agents.isSuccess && rows.length === 0 ? (
        <ScreenState kind="empty">{NO_AGENTS}</ScreenState>
      ) : null}
      {rows.length === 0 ? null : (
        <Table aria-label={AGENTS_HEADING}>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Bots</TableHead>
              <TableHead drop="under-790">Can run</TableHead>
              <TableHead drop="under-960">Version</TableHead>
              <TableHead>Last seen</TableHead>
              <TableHead drop="under-1100">Certificate until</TableHead>
              <TableHead drop="under-1180">Enrolled</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((agent) => {
              const offered =
                actor === undefined
                  ? { disable: false, release: false }
                  : agentActions(agent, actor.role);
              const pending = asking?.id === agent.id ? asking.change : null;
              const failure = writes.failure?.key === agent.id ? writes.failure.message : undefined;
              const ending =
                agent.enabled && agent.status !== "expired" && agent.not_after !== undefined
                  ? certificateWarning(agent.not_after, now)
                  : undefined;
              return (
                <Fragment key={agent.id}>
                  <TableRow>
                    <TableCell className="font-mono break-all">
                      <span className="inline-block min-w-[12ch]">{agent.name}</span>
                    </TableCell>
                    <TableCell>
                      <StatusLine
                        tone={ending === undefined ? agentStatusTone(agent.status) : "warning"}
                      >
                        {agentStatusLabel(agent.status)}
                      </StatusLine>
                      {agent.status === "expired" ? (
                        <span className="block text-xs text-muted-foreground">{EXPIRED_HINT}</span>
                      ) : null}
                      {ending === undefined ? null : (
                        <span className="block text-xs text-muted-foreground">{ending}</span>
                      )}
                      {!agent.enabled && agent.bots > 0 ? (
                        <span className="block text-xs text-muted-foreground">
                          {botsWait(agent.bots)}
                        </span>
                      ) : null}
                    </TableCell>
                    <TableCell className="font-mono">{agent.bots}</TableCell>
                    <TableCell drop="under-790" className="font-mono">
                      {agent.capacity ?? ""}
                    </TableCell>
                    <TableCell drop="under-960" className="font-mono break-all">
                      {agent.version ?? ""}
                    </TableCell>
                    <TableCell>
                      {agent.last_seen_at === undefined
                        ? NEVER_SEEN
                        : formatTime(agent.last_seen_at)}
                    </TableCell>
                    <TableCell drop="under-1100">
                      {agent.not_after === undefined ? "" : formatTime(agent.not_after)}
                    </TableCell>
                    <TableCell drop="under-1180">{formatTime(agent.created_at)}</TableCell>
                    <TableCell>
                      <div className="flex flex-wrap justify-end gap-1">
                        <Button asChild size="sm" variant="ghost">
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
                            variant="ghost"
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
                            variant="ghost"
                            disabled={busy}
                            onClick={() => setAsking({ id: agent.id, change: "release" })}
                          >
                            {RELEASE_BOTS}
                          </Button>
                        ) : null}
                      </div>
                    </TableCell>
                  </TableRow>
                  {pending === null &&
                  writes.prompt !== agent.id &&
                  failure === undefined ? null : (
                    <TableSpanRow span={COLUMNS} className="pb-3">
                      <div className="flex flex-col gap-2">
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
                      </div>
                    </TableSpanRow>
                  )}
                </Fragment>
              );
            })}
          </TableBody>
        </Table>
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
