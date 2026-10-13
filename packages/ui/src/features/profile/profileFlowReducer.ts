// The profile's two flows (Plan.md P8.7, ADR-0016, ADR-0017), a pure reducer like the
// sign-in flow's: changing the password and replacing the authenticator. Each starts with
// the password again ("Confirm it's you", a step-up), which the server accepts for 5
// minutes; the password lives only in that form until the step-up answers.
// - Changing the password: then the new password, typed twice, before the step-up ends.
// - Replacing the authenticator: then straight into the 2FA setup with the setup token the
//   server keeps for this session (`mfaReset`), then the new recovery codes.
//
// Every step has an id, and every action names the step it was issued in (`from`): the
// reducer drops an action whose step is no longer current, so a late answer changes
// nothing. A step that ended (its deadline, or the server's `step_up_required`) goes back to
// the password.
import { STEP_UP_WINDOW_SECS } from "../../api/limits";
import {
  type EnrollmentAction,
  nextSetup,
  type SetupState,
  setupStep,
} from "../auth/authFlowReducer";

/** A step that ended: the step-up or the setup token expired. */
export const STEP_ENDED = "That step ended. Enter your password again.";

/** Where a profile flow is. */
export type ProfileStep =
  | { readonly kind: "confirm"; readonly notice?: string }
  | { readonly kind: "newPassword"; readonly deadline: number; readonly expiresIn: number }
  | SetupState
  | { readonly kind: "recovery"; readonly codes: readonly string[] };

/** A profile flow's state. */
export interface ProfileFlowState {
  readonly step: ProfileStep;
  /** Bumped at every change of step; see the module comment. */
  readonly stepId: number;
}

/** What happens in a profile flow, each for the step it was issued in (`from`). */
export type ProfileFlowAction =
  | EnrollmentAction
  | ({ readonly from: number } & (
      | {
          readonly type: "steppedUp";
          /** When the step-up was sent, in ms since the epoch. */
          readonly now: number;
        }
      | {
          readonly type: "resetStarted";
          readonly expiresIn: number;
          /** When the reset was sent, in ms since the epoch. */
          readonly now: number;
        }
      | { readonly type: "confirmed"; readonly codes: readonly string[] }
    ));

/** A flow's start: the password again. */
export const INITIAL_PROFILE_FLOW: ProfileFlowState = { step: { kind: "confirm" }, stepId: 0 };

/** The next state; see the module comment. */
export function profileFlowReducer(
  state: ProfileFlowState,
  action: ProfileFlowAction,
): ProfileFlowState {
  if (action.from !== state.stepId) {
    return state;
  }
  const step = state.step;
  switch (action.type) {
    case "steppedUp":
      return step.kind === "confirm"
        ? moved(state, {
            kind: "newPassword",
            deadline: action.now + STEP_UP_WINDOW_SECS * 1000,
            expiresIn: STEP_UP_WINDOW_SECS,
          })
        : state;
    case "resetStarted":
      return step.kind === "confirm"
        ? moved(state, setupStep(action.expiresIn, action.now))
        : state;
    case "enrolling":
    case "enrolled":
    case "enrollFailed":
      return step.kind === "setup" ? { ...state, step: nextSetup(step, action) } : state;
    case "confirmed":
      return step.kind === "setup"
        ? moved(state, { kind: "recovery", codes: action.codes })
        : state;
    case "stepEnded":
      return moved(state, { kind: "confirm", notice: STEP_ENDED });
  }
}

/** `state` in the new step `step`, its id bumped. */
function moved(state: ProfileFlowState, step: ProfileStep): ProfileFlowState {
  return { step, stepId: state.stepId + 1 };
}
