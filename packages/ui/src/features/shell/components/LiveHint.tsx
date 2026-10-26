// The Header's line about the live events (Plan.md P11.8; ADR-0020, U30, R85): only while a
// reconnect waits, counting down, or once the watch stopped with an error that isn't the
// session's end; nothing while live, and nothing for the routine reconnect that doesn't
// wait. A screen reader hears each change once, never each second: the ticking line is
// hidden from it, and the status region holds the text the change started with.
import { useEffect, useState } from "react";

import { describeError } from "../../../api/describeError";
import { useLive } from "../../../app/context";
import type { LiveHint as Hint } from "../../../app/stores";
import type { CommandError } from "../../../generated/CommandError";

/** After `too_many_connections`: D's approved text, the server's own message. */
export const TOO_MANY =
  "Live updates are already open in too many places. Close the app somewhere else, or try again in a minute.";

/** What the line shows while a reconnect waits `seconds` more. */
export function pausedShown(seconds: number): string {
  return `Live updates paused. Reconnecting in ${seconds} s.`;
}

/** What a screen reader hears when a reconnect starts waiting `seconds`. */
export function pausedSpoken(seconds: number): string {
  return `Live updates paused. Reconnecting in ${seconds} seconds.`;
}

/** The line once the watch stopped with `error`; the period isn't doubled. */
export function stopped(error: CommandError): string {
  const why = describeError(error);
  const sentence = /[.!?]$/.test(why) ? why : `${why}.`;
  return `Live updates stopped: ${sentence} Screens update when you open them.`;
}

/** The line; see the module comment. */
export function LiveHint() {
  const hint = useLive((state) => state.hint);
  const left = useSecondsLeft(hint);
  const [shown, spoken] = texts(hint, left);
  return (
    <div className="text-sm text-muted-foreground">
      {shown === "" ? null : <span aria-hidden="true">{shown}</span>}
      <span role="status" className="sr-only">
        {spoken}
      </span>
    </div>
  );
}

/** What `hint` shows, with `left` seconds of its wait to go, and what it says. */
function texts(hint: Hint, left: number): [shown: string, spoken: string] {
  switch (hint.kind) {
    case "waiting":
      if (left <= 0) {
        return ["", ""];
      }
      return hint.tooMany ? [TOO_MANY, TOO_MANY] : [pausedShown(left), pausedSpoken(hint.secs)];
    case "stopped": {
      const text = stopped(hint.error);
      return [text, text];
    }
    case "none":
      return ["", ""];
    default:
      return ["", ""];
  }
}

/**
 * The whole seconds left of `hint`'s wait, rounded up, ticking each second; 0 without one.
 * Never more than the wait itself, so the first render after a new wait, whose clock reading
 * is older, shows the wait's own length.
 */
function useSecondsLeft(hint: Hint): number {
  const until = hint.kind === "waiting" ? hint.until : null;
  const secs = hint.kind === "waiting" ? hint.secs : 0;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    if (until === null) {
      return;
    }
    const tick = setInterval(() => {
      const at = Date.now();
      setNow(at);
      if (at >= until) {
        clearInterval(tick);
      }
    }, 1000);
    return () => clearInterval(tick);
  }, [until]);
  return until === null ? 0 : Math.max(0, Math.min(secs, Math.ceil((until - now) / 1000)));
}
