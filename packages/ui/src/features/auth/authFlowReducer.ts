// The sign-in flow's state (Plan.md P8.7, ADR-0016, ADR-0017), a pure reducer that
// AuthFlow drives. One flow covers logging in, the 2FA code, the 2FA setup with its
// recovery codes, and registering, which logs in right after.
//
// The password is kept only while a later step needs it: from a `setup_required` login
// (or a registration) until the login that follows the recovery codes answers, since the
// code that confirmed the authenticator can't log in. Every other step holds none, and
// anything that ends or leaves a step (it expired, a third wrong code, a cancel) drops it.
// The TOTP secret lives in its setup step and goes with it.
//
// Every step has an id, and every action names the step it was issued in (`from`): the
// reducer drops an action whose step is no longer current, so a late answer (a code
// checked after its step expired, a confirm after the user went back) changes nothing.
// A step ends on its own deadline, from the server's `expires_in` counted from when its
// request was sent, because an expired step answers like a wrong code.
import type { MfaEnrollResponse } from "../../generated/MfaEnrollResponse";
import type { WaitRequest } from "../../hooks/useRetryWait";

/** The code step's tries: the server allows 3 per step. */
export const CODE_TRIES = 3;

/** A step that ended: expired, used up or gone. */
export const STEP_ENDED = "That sign-in step ended. Enter your password again.";
/** Registered, but the login right after failed. */
export const REGISTERED = "Your account was created. Log in to set up 2FA.";
/** Set up, but the login after the recovery codes failed. */
export const SETUP_DONE = "2FA is set up. Log in with your password and a code from your app.";
/** The code step after the 2FA setup. */
export const NEXT_CODE =
  "Wait until your authenticator app shows a new code, then enter it. The code you just used won't work again.";

/** A wrong code's text, with the tries left. */
export function wrongCode(triesLeft: number): string {
  return triesLeft === 1 ? "Wrong code. 1 try left." : `Wrong code. ${triesLeft} tries left.`;
}

/** What the password step says above its form. */
export interface Notice {
  readonly text: string;
  /** describeError's text of what went wrong, when something did. */
  readonly reason?: string;
  /** Instead of `reason`: a refusal's wait, which the login form counts down. */
  readonly wait?: WaitRequest;
  /** Whether to offer "Log out" (a session the flow can't use). */
  readonly logout?: boolean;
}

/** What a failed login after registering hands to the login form at /login. */
export interface Handoff {
  readonly username: string;
  readonly notice: Notice;
}

/**
 * The 2FA setup step, shared with the profile's authenticator replacement: its deadline
 * (from the server's `expires_in`, counted from when the request was sent), and the
 * secret, requested once per step and on "New QR code".
 */
export interface SetupState {
  readonly kind: "setup";
  readonly deadline: number;
  readonly expiresIn: number;
  /** A secret is being requested; the old one is hidden meanwhile. */
  readonly loading: boolean;
  readonly enrollment?: MfaEnrollResponse;
  readonly enrollError?: string;
}

/** What happens to a setup step's secret, for the step it was issued in (`from`). */
export type EnrollmentAction = { readonly from: number } & (
  | { readonly type: "enrolling" }
  | { readonly type: "enrolled"; readonly enrollment: MfaEnrollResponse }
  | { readonly type: "enrollFailed"; readonly message: string }
  | { readonly type: "stepEnded" }
);

/** A setup step that starts loading its secret, ending `expiresIn` s after `now`. */
export function setupStep(expiresIn: number, now: number): SetupState {
  return { kind: "setup", deadline: now + expiresIn * 1000, expiresIn, loading: true };
}

/** The setup step after a change of its secret. */
export function nextSetup(
  step: SetupState,
  action: Extract<EnrollmentAction, { type: "enrolling" | "enrolled" | "enrollFailed" }>,
): SetupState {
  const { kind, deadline, expiresIn } = step;
  const base = { kind, deadline, expiresIn };
  switch (action.type) {
    case "enrolling":
      return { ...base, loading: true };
    case "enrolled":
      return { ...base, loading: false, enrollment: action.enrollment };
    case "enrollFailed":
      return { ...base, loading: false, enrollError: action.message };
  }
}

/** Where the flow is. */
export type Step =
  | { readonly kind: "password"; readonly notice?: Notice }
  | { readonly kind: "register" }
  | {
      readonly kind: "code";
      readonly deadline: number;
      readonly expiresIn: number;
      readonly triesLeft: number;
      /** After the 2FA setup: wait for the next code. */
      readonly next: boolean;
      readonly message?: string;
    }
  | SetupState
  | { readonly kind: "recovery"; readonly codes: readonly string[] }
  | { readonly kind: "stillSignedIn"; readonly username: string };

/** The flow's state. */
export interface FlowState {
  readonly step: Step;
  /** Bumped at every change of step; see the module comment. */
  readonly stepId: number;
  readonly username: string;
  /** Only while a later step needs it; see the module comment. */
  readonly password: string | null;
}

/** What happens in the flow, each for the step it was issued in (`from`). */
export type FlowAction = { readonly from: number } & (
  | {
      readonly type: "codeRequired";
      readonly username: string;
      readonly expiresIn: number;
      /** When the request was sent, in ms since the epoch. */
      readonly now: number;
      readonly next?: boolean;
    }
  | {
      readonly type: "setupRequired";
      readonly username: string;
      readonly password: string;
      readonly expiresIn: number;
      /** When the request was sent, in ms since the epoch. */
      readonly now: number;
    }
  | { readonly type: "wrongCode" }
  | { readonly type: "enrolling" }
  | { readonly type: "enrolled"; readonly enrollment: MfaEnrollResponse }
  | { readonly type: "enrollFailed"; readonly message: string }
  | { readonly type: "confirmed"; readonly codes: readonly string[] }
  | { readonly type: "stepEnded" }
  | { readonly type: "backToPassword"; readonly notice?: Notice; readonly username?: string }
  | { readonly type: "stillSignedIn"; readonly username: string }
);

/** The flow's start: the login form (with what a registration handed over), or the register form. */
export function initialFlow(mode: "login" | "register", handoff?: Handoff): FlowState {
  if (mode === "register") {
    return { step: { kind: "register" }, stepId: 0, username: "", password: null };
  }
  return {
    step:
      handoff === undefined ? { kind: "password" } : { kind: "password", notice: handoff.notice },
    stepId: 0,
    username: handoff?.username ?? "",
    password: null,
  };
}

/** The next state; see the module comment. */
export function authFlowReducer(state: FlowState, action: FlowAction): FlowState {
  if (action.from !== state.stepId) {
    return state;
  }
  const step = state.step;
  switch (action.type) {
    case "codeRequired":
      return moved(state, {
        username: action.username,
        password: null,
        step: {
          kind: "code",
          deadline: action.now + action.expiresIn * 1000,
          expiresIn: action.expiresIn,
          triesLeft: CODE_TRIES,
          next: action.next ?? false,
        },
      });
    case "setupRequired":
      return moved(state, {
        username: action.username,
        password: action.password,
        step: setupStep(action.expiresIn, action.now),
      });
    case "wrongCode": {
      if (step.kind !== "code") {
        return state;
      }
      const triesLeft = step.triesLeft - 1;
      if (triesLeft <= 0) {
        return back(state, { text: STEP_ENDED });
      }
      return { ...state, step: { ...step, triesLeft, message: wrongCode(triesLeft) } };
    }
    case "enrolling":
    case "enrolled":
    case "enrollFailed":
      return step.kind === "setup" ? { ...state, step: nextSetup(step, action) } : state;
    case "confirmed":
      return moved(state, { step: { kind: "recovery", codes: action.codes } });
    case "stepEnded":
      return back(state, { text: STEP_ENDED });
    case "backToPassword":
      return back(
        action.username === undefined ? state : { ...state, username: action.username },
        action.notice,
      );
    case "stillSignedIn":
      return moved(state, {
        password: null,
        step: { kind: "stillSignedIn", username: action.username },
      });
  }
}

/** `state` in a new step: `changes` applied, the step id bumped. */
function moved(state: FlowState, changes: Partial<FlowState> & { step: Step }): FlowState {
  return { ...state, ...changes, stepId: state.stepId + 1 };
}

/** The password step, with `notice`, holding no password. */
function back(state: FlowState, notice: Notice | undefined): FlowState {
  return moved(state, {
    password: null,
    step: notice === undefined ? { kind: "password" } : { kind: "password", notice },
  });
}
