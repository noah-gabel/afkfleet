// A setup step's secret (Plan.md P8.7, ADR-0017), for the sign-in flow and the profile's
// authenticator replacement: `mfaEnroll()` once when the step starts (a ref keeps
// StrictMode's second effect run from asking again) and on "New QR code", which waits while
// one loads. Only the newest answer counts; a step the server no longer holds ends.
import { useCallback, useEffect, useRef } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import type { EnrollmentAction } from "../authFlowReducer";
import { isStepGone } from "../stepErrors";

/**
 * Requests the secret of the setup step `setupStepId` (null: no setup step now), dispatching
 * what happens to it. Answers the request for "New QR code".
 */
export function useEnrollment(
  setupStepId: number | null,
  dispatch: (action: EnrollmentAction) => void,
): (from: number) => Promise<void> {
  const api = useApiClient();
  const secretFor = useRef<number | null>(null);
  const enrolling = useRef<number | null>(null);
  const requestSecret = useCallback(
    async (from: number): Promise<void> => {
      if (enrolling.current === from) {
        return;
      }
      enrolling.current = from;
      dispatch({ type: "enrolling", from });
      try {
        const enrollment = await api.mfaEnroll();
        dispatch({ type: "enrolled", from, enrollment });
      } catch (rejection) {
        const error = toApiError(rejection).error;
        dispatch(
          isStepGone(error)
            ? { type: "stepEnded", from }
            : { type: "enrollFailed", from, message: describeError(error) },
        );
      } finally {
        if (enrolling.current === from) {
          enrolling.current = null;
        }
      }
    },
    [api, dispatch],
  );
  useEffect(() => {
    if (setupStepId !== null && secretFor.current !== setupStepId) {
      secretFor.current = setupStepId;
      void requestSecret(setupStepId);
    }
  }, [setupStepId, requestSecret]);
  return requestSecret;
}
