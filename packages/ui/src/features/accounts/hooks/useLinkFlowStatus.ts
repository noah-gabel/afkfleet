// Following a link flow's status (Plan.md P9.8, S4 in ADR-0018's group F). The status never
// carries the code, so it's a query, keyed by the flow, outside the accounts' keys:
// - read every 2 s while the flow runs, also while the app is in the background, since the
//   user is in the browser, and a paused read could miss the end and its 15 minutes;
// - stopped for good at a known end state or a 404 (the flow is gone), so the banner's
//   recovery never reads a finished flow again; a state this version doesn't know keeps it
//   reading, which the server's 404 15 minutes after the end bounds;
// - paused by any other failed read, which the banner's recovery or "Try again" restarts;
//   a 429 or 503 waits what it asks, at least 2 s, then goes on;
// - `linked` or a 404 (it may have linked) refreshes every account query.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { ACCOUNTS_KEY } from "../../../api/queries";
import type { LinkFlowResponse } from "../../../generated/LinkFlowResponse";

/** How often a running flow's status is read. */
export const LINK_FLOW_POLL_MS = 2000;

/** How long a 429 or 503 without its own wait pauses the reads. */
const UNTOLD_WAIT_MS = 10_000;

/** The states a flow ends in. */
const END_STATES: ReadonlySet<string> = new Set([
  "declined",
  "expired",
  "taken",
  "over_quota",
  "refused",
  "failed",
  "linked",
]);

/** Whether `error` is the server's 404: the flow is gone for good. */
export function isGone(error: unknown): boolean {
  if (error === null || error === undefined) {
    return false;
  }
  const api = toApiError(error).error.api;
  return api?.status === 404;
}

/** Whether a flow whose last answer is `data`, or `error`, has ended. */
function settled(data: LinkFlowResponse | undefined, error: unknown): boolean {
  return (data !== undefined && END_STATES.has(data.status)) || isGone(error);
}

/** When to read again after the failure `error`, or never (`false`). */
function waitAfter(error: unknown): number | false {
  const api = toApiError(error).error.api;
  if (api === undefined || (api.status !== 429 && api.status !== 503)) {
    return false;
  }
  return api.retry_after_secs === undefined
    ? UNTOLD_WAIT_MS
    : Math.max(LINK_FLOW_POLL_MS, api.retry_after_secs * 1000);
}

/** The status of the flow `flowId`, or nothing without one; see the module comment. */
export function useLinkFlowStatus(flowId: string | null) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: ["linkFlow", flowId],
    queryFn: () => api.linkStatus(flowId ?? ""),
    enabled: (current) => flowId !== null && !settled(current.state.data, current.state.error),
    refetchInterval: (current) =>
      current.state.status === "error" ? waitAfter(current.state.error) : LINK_FLOW_POLL_MS,
    refetchIntervalInBackground: true,
  });
  const gone = isGone(query.error);
  const linked = query.data?.status === "linked";
  const told = useRef<string | null>(null);
  useEffect(() => {
    if (flowId !== null && (linked || gone) && told.current !== flowId) {
      told.current = flowId;
      void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
    }
  }, [flowId, linked, gone, queryClient]);
  return {
    data: query.data,
    error: query.error,
    gone,
    ended: flowId !== null && settled(query.data, query.error),
    refetch: () => void query.refetch(),
  };
}
