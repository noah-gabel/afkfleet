// Linking a Microsoft account, or an account again (Plan.md P9.8, ADR-0018), on a route of
// its own (the user's U9 at group F's plan review), in two steps:
// 1. the warning, in the user's words, and "Get a code": nothing starts before that click;
// 2. the code with "Copy code", Microsoft's page, which opens by itself once (U10) and again
//    on "Open Microsoft's page", and the flow's live status (useLinkFlowStatus).
//
// The start answers the user code, a secret, so it's a direct call, never a query or a
// mutation; the code lives only in this screen's state, and goes when the flow ends or the
// user leaves (CLAUDE.md). The page opens through the Rust side, which opens only the link it
// checked when the answer arrived; the webview never names a URL. A start's 429 and 503 each
// get one text naming every cause, with the server's wait counting down (S7).
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { accountQuery } from "../../../api/queries";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { AccountResponse } from "../../../generated/AccountResponse";
import type { CommandError } from "../../../generated/CommandError";
import { retryRequest, useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import { copyWithToast } from "../../../lib/clipboard";
import { formatClock } from "../../../lib/time";
import { AuthCard } from "../../auth/components/AuthCard";
import { useLinkFlowStatus } from "../hooks/useLinkFlowStatus";
import {
  CODE_COPIED,
  COPY_FAILED,
  couldntOpen,
  flowText,
  LINK_GONE,
  LINK_INTRO,
  LINK_NOT_MICROSOFTS,
  LINK_WARNING,
  linkAgainIntro,
  NOTHING_TO_OPEN,
  START_BUSY,
  START_LIMITED,
  START_OFFLINE,
  START_OVER_QUOTA,
  START_PENDING,
  startFailed,
} from "../texts";

/** What a refused start says, and the wait it asks for, if any. */
export interface StartRefusal {
  readonly message: string;
  readonly seconds?: number;
}

/**
 * A refused start's text (S7 in ADR-0018's group F): one per status, whatever its cause;
 * `again` is the account being linked again, which tells its 409 apart.
 */
export function startRefusal(
  error: CommandError,
  again: Pick<AccountResponse, "kind" | "status"> | undefined,
): StartRefusal {
  const api = error.api;
  const seconds = api?.retry_after_secs;
  const wait = seconds !== undefined && seconds > 0 ? { seconds } : {};
  if (error.code === "invalid_verification_link") {
    return { message: LINK_NOT_MICROSOFTS };
  }
  switch (api?.status) {
    case 409:
      if (again === undefined) {
        return { message: START_OVER_QUOTA };
      }
      return { message: again.kind === "offline" ? START_OFFLINE : START_PENDING };
    case 429:
      return { message: START_LIMITED, ...wait };
    case 503:
      return { message: START_BUSY, ...wait };
    default: {
      const request = retryRequest(error);
      return request === undefined
        ? { message: startFailed(describeError(error)) }
        : { message: startFailed(request.message), seconds: request.seconds };
    }
  }
}

/** A running flow, as the screen holds it: the code only until the flow ends. */
interface Started {
  readonly flowId: string;
  readonly code: string | null;
  readonly link: string;
}

/** The states after which a new code may be asked for. */
const RETRYABLE: ReadonlySet<string> = new Set(["declined", "expired", "failed", "refused"]);

/** The link screen: a new link, or account `again` linked again. */
export function LinkScreen({ again }: { again?: string }) {
  const relinked = useAccountIfAgain(again);
  const api = useApiClient();
  const alive = useAlive();
  const wait = useRetryWait();
  const [started, setStarted] = useState<Started | null>(null);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string>();
  const [openError, setOpenError] = useState<string>();
  const status = useLinkFlowStatus(started?.flowId ?? null);
  const name = relinked?.name;

  // The code is a secret: it goes as soon as the flow ends.
  useEffect(() => {
    if (status.ended) {
      setStarted((current) =>
        current === null || current.code === null ? current : { ...current, code: null },
      );
    }
  }, [status.ended]);

  async function open(link: string): Promise<void> {
    setOpenError(undefined);
    try {
      await api.openVerificationLink();
    } catch (rejection) {
      if (!alive.current) {
        return;
      }
      const error = toApiError(rejection).error;
      setOpenError(
        error.code === "verification_link_open"
          ? couldntOpen(link)
          : error.code === "no_verification_link"
            ? NOTHING_TO_OPEN
            : describeError(error),
      );
    }
  }

  async function getCode(): Promise<void> {
    if (starting || wait.waiting !== null) {
      return;
    }
    setStartError(undefined);
    setOpenError(undefined);
    wait.reset();
    setStarting(true);
    try {
      const answer = again === undefined ? await api.startLink() : await api.relinkAccount(again);
      if (!alive.current) {
        return;
      }
      setStarted({ flowId: answer.flow_id, code: answer.user_code, link: answer.verification_uri });
      await open(answer.verification_uri);
    } catch (rejection) {
      if (!alive.current) {
        return;
      }
      const refusal = startRefusal(toApiError(rejection).error, relinked);
      if (refusal.seconds === undefined) {
        setStartError(refusal.message);
      } else {
        wait.startFor({ message: refusal.message, seconds: refusal.seconds });
      }
    } finally {
      if (alive.current) {
        setStarting(false);
      }
    }
  }

  const title =
    again === undefined ? "Link a Microsoft account" : `Link ${name ?? "the account"} again`;
  const intro = again === undefined ? LINK_INTRO : linkAgainIntro(name ?? "the account");
  const data = status.data;
  const ended = status.ended;
  const canRetry = status.gone || (data !== undefined && ended && RETRYABLE.has(data.status));
  const linkedId = data?.status === "linked" ? (data.account_id ?? again) : undefined;

  return (
    <AuthCard title={title} intro={intro}>
      <Alert>
        <AlertDescription className="text-foreground">{LINK_WARNING}</AlertDescription>
      </Alert>
      {started === null ? (
        <>
          {startError === undefined ? null : (
            <p role="alert" className="text-sm text-destructive">
              {startError}
            </p>
          )}
          <RetryWaitMessage wait={wait} />
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              disabled={starting || wait.waiting !== null}
              onClick={() => void getCode()}
            >
              Get a code
            </Button>
            <Button asChild variant="outline">
              <Link to="/accounts">Back to accounts</Link>
            </Button>
          </div>
        </>
      ) : (
        <>
          {started.code === null ? null : (
            <div className="flex flex-col gap-2">
              <p className="text-sm text-muted-foreground">Your code</p>
              <div className="flex flex-wrap items-center gap-3">
                <code className="rounded-md bg-muted px-3 py-2 font-mono text-2xl tracking-widest select-all">
                  {started.code}
                </code>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => {
                    if (started.code !== null) {
                      void copyWithToast(started.code, CODE_COPIED, COPY_FAILED);
                    }
                  }}
                >
                  Copy code
                </Button>
              </div>
              <Button type="button" className="self-start" onClick={() => void open(started.link)}>
                Open Microsoft's page
              </Button>
            </div>
          )}
          {openError === undefined || ended ? null : (
            <p role="alert" className="text-sm text-destructive select-text">
              {openError}
            </p>
          )}
          <FlowLine
            again={again !== undefined}
            name={name}
            status={status}
            onTryAgain={status.refetch}
          />
          <div className="flex flex-wrap gap-2">
            {canRetry ? (
              <Button
                type="button"
                disabled={starting}
                onClick={() => {
                  setStarted(null);
                  void getCode();
                }}
              >
                Get a new code
              </Button>
            ) : null}
            {linkedId === undefined ? null : (
              <Button asChild>
                <Link to="/accounts/$id" params={{ id: linkedId }}>
                  Open account
                </Link>
              </Button>
            )}
            <Button asChild variant="outline">
              <Link to="/accounts">Back to accounts</Link>
            </Button>
          </div>
        </>
      )}
    </AuthCard>
  );
}

/** The account being linked again, once read; nothing, and no read, for a new link. */
function useAccountIfAgain(id: string | undefined): AccountResponse | undefined {
  const api = useApiClient();
  const account = useQuery({ ...accountQuery(api, id ?? ""), enabled: id !== undefined });
  return id === undefined ? undefined : account.data;
}

/** The flow's line: its state, the 404, or a failed read with "Try again". */
function FlowLine({
  again,
  name,
  status,
  onTryAgain,
}: {
  again: boolean;
  name: string | undefined;
  status: ReturnType<typeof useLinkFlowStatus>;
  onTryAgain: () => void;
}) {
  if (status.gone) {
    return <p role="status">{LINK_GONE}</p>;
  }
  const data = status.data;
  if (status.error !== null && !status.ended) {
    return (
      <div className="flex flex-col gap-2">
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(status.error).error)}
        </p>
        <Button type="button" variant="outline" className="self-start" onClick={onTryAgain}>
          Try again
        </Button>
      </div>
    );
  }
  if (data === undefined) {
    return null;
  }
  return (
    <p role="status">
      {flowText(data.status, {
        again,
        name: data.name ?? name,
        accountStatus: data.account_status,
        reason: data.reason,
        until: data.expires_at === undefined ? undefined : formatClock(data.expires_at),
      })}
    </p>
  );
}
