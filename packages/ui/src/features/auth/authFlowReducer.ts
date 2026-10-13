// The sign-in flow's state (Plan.md P8.7, ADR-0016, ADR-0017), a pure reducer that
// AuthFlow drives. One flow covers logging in, the 2FA code, the 2FA setup with its
// recovery codes, and registering, which logs in right after.
//
// The password is kept only while a later step needs it: from a `setup_required` login
// (or a registration) until the login that follows the recovery codes answers, since the
// code that confirmed the authenticator can't log in. Every other step holds none, and
// anything that ends or leaves a step (it expired, a third wrong code, a cancel) drops it.
// The step ends on its own deadline, from the server's `expires_in`, because an expired
// step answers like a wrong code.

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
  /** Whether to offer "Log out" (a session the flow can't use). */
  readonly logout?: boolean;
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
  | { readonly kind: "setup"; readonly deadline: number; readonly expiresIn: number }
  | { readonly kind: "recovery"; readonly codes: readonly string[] }
  | { readonly kind: "stillSignedIn"; readonly username: string };

/** The flow's state. */
export interface FlowState {
  readonly step: Step;
  readonly username: string;
  /** Only while a later step needs it; see the module comment. */
  readonly password: string | null;
  /** Bumped at every return to the password step, so its form starts empty. */
  readonly round: number;
}

/** What happens in the flow. */
export type FlowAction =
  | {
      readonly type: "codeRequired";
      readonly username: string;
      readonly expiresIn: number;
      readonly now: number;
      readonly next?: boolean;
    }
  | {
      readonly type: "setupRequired";
      readonly username: string;
      readonly password: string;
      readonly expiresIn: number;
      readonly now: number;
    }
  | { readonly type: "wrongCode" }
  | { readonly type: "confirmed"; readonly codes: readonly string[] }
  | { readonly type: "stepEnded" }
  | { readonly type: "backToPassword"; readonly notice?: Notice; readonly username?: string }
  | { readonly type: "stillSignedIn"; readonly username: string };

/** The flow's start: the login form, or the register form. */
export function initialFlow(mode: "login" | "register"): FlowState {
  return {
    step: mode === "login" ? { kind: "password" } : { kind: "register" },
    username: "",
    password: null,
    round: 0,
  };
}

/** The next state; see the module comment. */
export function authFlowReducer(state: FlowState, action: FlowAction): FlowState {
  switch (action.type) {
    case "codeRequired":
      return {
        ...state,
        username: action.username,
        password: null,
        step: {
          kind: "code",
          deadline: action.now + action.expiresIn * 1000,
          expiresIn: action.expiresIn,
          triesLeft: CODE_TRIES,
          next: action.next ?? false,
        },
      };
    case "setupRequired":
      return {
        ...state,
        username: action.username,
        password: action.password,
        step: {
          kind: "setup",
          deadline: action.now + action.expiresIn * 1000,
          expiresIn: action.expiresIn,
        },
      };
    case "wrongCode": {
      if (state.step.kind !== "code") {
        return state;
      }
      const triesLeft = state.step.triesLeft - 1;
      if (triesLeft <= 0) {
        return back(state, { text: STEP_ENDED });
      }
      return { ...state, step: { ...state.step, triesLeft, message: wrongCode(triesLeft) } };
    }
    case "confirmed":
      return { ...state, step: { kind: "recovery", codes: action.codes } };
    case "stepEnded":
      return back(state, { text: STEP_ENDED });
    case "backToPassword":
      return back(
        action.username === undefined ? state : { ...state, username: action.username },
        action.notice,
      );
    case "stillSignedIn":
      return {
        ...state,
        password: null,
        step: { kind: "stillSignedIn", username: action.username },
      };
  }
}

/** The password step, with `notice`, holding no password. */
function back(state: FlowState, notice: Notice | undefined): FlowState {
  return {
    ...state,
    password: null,
    round: state.round + 1,
    step: notice === undefined ? { kind: "password" } : { kind: "password", notice },
  };
}
