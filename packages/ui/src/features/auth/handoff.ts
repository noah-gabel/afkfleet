// A failed login after registering hands its username and notice to the login form at
// /login (Plan.md P8.7, ADR-0017), so that form is a real /login whose "Register with an
// invite" link works. The handoff rides in the history entry's state (memory history: it
// never leaves the app) and is read back through a zod check, like any other input.
import { z } from "../../api/zod";
import type { Handoff } from "./authFlowReducer";

/** The history state's key for the handoff. */
const KEY = "afkfleetSignIn";

const handoffSchema = z.object({
  username: z.string(),
  notice: z.object({
    text: z.string(),
    reason: z.string().optional(),
    wait: z.object({ message: z.string(), seconds: z.number().int().positive() }).optional(),
    logout: z.boolean().optional(),
  }),
});

/** `state` with `handoff` added, for `navigate({ state })`. */
export function withHandoff<S extends object>(state: S, handoff: Handoff): S {
  const next = { ...state, [KEY]: handoff };
  return next;
}

/** The handoff in a history entry's `state`, if it holds a valid one. */
export function handoffOf(state: unknown): Handoff | undefined {
  if (typeof state !== "object" || state === null) {
    return undefined;
  }
  const parsed = handoffSchema.safeParse(Reflect.get(state, KEY));
  if (!parsed.success) {
    return undefined;
  }
  const { username, notice } = parsed.data;
  return {
    username,
    notice: {
      text: notice.text,
      ...(notice.reason === undefined ? {} : { reason: notice.reason }),
      ...(notice.wait === undefined ? {} : { wait: notice.wait }),
      ...(notice.logout === undefined ? {} : { logout: notice.logout }),
    },
  };
}
