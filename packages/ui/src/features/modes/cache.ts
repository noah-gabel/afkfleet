// A mode's answer in the every-page list (Plan.md P11.9; ADR-0020, R163): put in where it belongs
// by the live patches' own functions, then read again. Nothing while the list is unread.
import type { QueryClient } from "@tanstack/react-query";

import { placed, without } from "../../api/livePatches";
import { MODES_LIST_KEY } from "../../api/queries";
import type { ModeResponse } from "../../generated/ModeResponse";

/** Reads the list again. */
export function rereadModes(queryClient: QueryClient): void {
  void queryClient.invalidateQueries({ queryKey: MODES_LIST_KEY, exact: true });
}

/** `mode` in the list, in its place, and the list read again. */
export function rememberMode(queryClient: QueryClient, mode: ModeResponse): void {
  queryClient.setQueryData(MODES_LIST_KEY, (data) =>
    data === undefined ? undefined : placed(data, mode),
  );
  rereadModes(queryClient);
}

/** Mode `id` out of the list, and the list read again. */
export function forgetMode(queryClient: QueryClient, id: string): void {
  queryClient.setQueryData(MODES_LIST_KEY, (data) =>
    data === undefined ? undefined : without(data, id),
  );
  rereadModes(queryClient);
}
