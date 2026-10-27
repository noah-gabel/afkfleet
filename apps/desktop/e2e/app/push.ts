// The e2e build's live events hook (Plan.md P11.9; ADR-0020's note for H): a spec pushes an
// update into the page's current watch, as the app's Rust side sends one over its Channel.
// The channel factory keeps the newest watch's handler, and `window.__afkfleetE2ePush` sends
// to it; TauriApiClient still passes on only what arrives before its watch settles. Only the
// e2e build has it: scripts/e2e-bundle-guard.mjs refuses its name in every shipped bundle.
import type { OpenChannel } from "../../src/TauriApiClient";

/** The hook's name on `window`, which the bundle guard looks for. */
export const PUSH_HOOK = "__afkfleetE2ePush";

/** A channel factory whose newest handler the hook sends every pushed update to. */
export function pushableChannel(): OpenChannel {
  let current: ((message: unknown) => void) | null = null;
  Object.defineProperty(window, PUSH_HOOK, {
    value: (update: unknown) => {
      current?.(update);
    },
  });
  return <T>(onMessage: (message: T) => void) => {
    current = onMessage as (message: unknown) => void;
    // The value a real channel serializes to.
    return "__CHANNEL__:0";
  };
}
