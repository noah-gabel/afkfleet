// The guards of a flow of steps (Plan.md P8.7, ADR-0017), shared by the sign-in flow and
// the profile's flows. Every step has an id, and each call runs for the step it started in:
// - `alive`: whether the flow is still on screen. A late answer never signs in, toasts or
//   navigates after the user left.
// - `isCurrent(from)`: whether the flow is on screen and `from` still its step.
// - `guarded(work)`: runs `work` for the current step, once at a time. While it runs, that
//   step's controls wait (`pending`) and a second call does nothing; a new step isn't
//   blocked by an older step's call still in flight. It rethrows for the form to show.
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

/** Whether the component is still mounted, as a ref. */
export function useAlive() {
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  return alive;
}

/** The guards for the flow whose current step is `stepId`; see the module comment. */
export function useStepGuard(stepId: number) {
  const alive = useAlive();
  const current = useRef(stepId);
  useLayoutEffect(() => {
    current.current = stepId;
  }, [stepId]);
  const isCurrent = useCallback(
    (from: number) => alive.current && current.current === from,
    [alive],
  );

  const busyFor = useRef<number | null>(null);
  const [busyStep, setBusyStep] = useState<number | null>(null);
  const pending = busyStep === stepId;

  async function guarded(work: (from: number) => Promise<void>): Promise<void> {
    const from = stepId;
    if (busyFor.current === from) {
      return;
    }
    busyFor.current = from;
    setBusyStep(from);
    try {
      await work(from);
    } finally {
      if (busyFor.current === from) {
        busyFor.current = null;
        if (alive.current) {
          setBusyStep(null);
        }
      }
    }
  }

  return { alive, isCurrent, guarded, pending };
}
