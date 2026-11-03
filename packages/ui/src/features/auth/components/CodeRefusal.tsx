// What the boxes left out, said where a screen reader hears it (Plan.md P11.11; ADR-0020, R205). A
// status region announces a change to what's already there, so it stays mounted, out of the flow while
// empty. The message's span is keyed by a count, so the same refusal at a second letter changes the
// DOM and is said again.
import { useCallback, useState } from "react";

/** A refusal to say, and how many have been said. */
export interface Refusal {
  readonly message: string | undefined;
  readonly said: number;
}

/** The refusal to show, and how to set it; `undefined` once there's nothing to say. */
export function useRefusal(): readonly [Refusal, (message: string | undefined) => void] {
  const [refusal, setRefusal] = useState<Refusal>({ message: undefined, said: 0 });
  const say = useCallback((message: string | undefined) => {
    setRefusal(({ said }) => ({ message, said: said + 1 }));
  }, []);
  return [refusal, say];
}

/** The status region. */
export function CodeRefusal({ refusal }: { refusal: Refusal }) {
  return (
    <p data-slot="code-refusal" role="status" aria-atomic="true" className="text-sm empty:absolute">
      {refusal.message === undefined ? null : <span key={refusal.said}>{refusal.message}</span>}
    </p>
  );
}
