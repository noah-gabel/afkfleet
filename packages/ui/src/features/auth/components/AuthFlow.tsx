// The sign-in flow (Plan.md P8.7, ADR-0016, ADR-0017): logging in with the 2FA code, the
// first login's 2FA setup with its recovery codes, and registering, which logs in right
// after. Its state is a reducer here (authFlowReducer.ts), so the password, the TOTP secret
// and the recovery codes live only in this component's state and go when it does.
//
// Every call here carries a secret in its request or its answer, so each is a direct call,
// never a TanStack query or mutation, whose caches would keep it (CLAUDE.md). The calls go
// through the watched client like every other: a `no_session` here ends nothing, since no
// session is signed in yet, and an unreachable server raises the banner.
//
// Late answers: each call runs for the step it started in (`guarded`). While it runs, that
// step's controls are disabled and a second submit does nothing; its answer is dropped if
// the step changed (the reducer checks `from`) or the flow left its route (`alive`), and it
// never signs in or navigates then. A step's deadline counts from when its request was
// sent, so the client never waits longer than the server.
//
// Every way in sets the session signed in, with a new generation, and invalidates `me`
// before opening Home: the code step's success, a login that finds the same user's session
// still held, and "Continue as" another user's.
import { useQueryClient } from "@tanstack/react-query";
import { useLocation, useNavigate } from "@tanstack/react-router";
import { useReducer } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { meQuery, useServerProfile } from "../../../api/queries";
import { useAppStores } from "../../../app/context";
import type { CommandError } from "../../../generated/CommandError";
import type { LoginStepResponse } from "../../../generated/LoginStepResponse";
import { retryRequest } from "../../../hooks/useRetryWait";
import { useStepGuard } from "../../../hooks/useStepGuard";
import { displayOrigin } from "../../../lib/origin";
import { useLogout } from "../../shell/hooks/useLogout";
import {
  authFlowReducer,
  initialFlow,
  type Notice,
  REGISTERED,
  SETUP_DONE,
} from "../authFlowReducer";
import { handoffOf, withHandoff } from "../handoff";
import { useEnrollment } from "../hooks/useEnrollment";
import type { LoginValues, RegisterValues } from "../schemas";
import { isStepGone } from "../stepErrors";
import { CodeStep } from "./CodeStep";
import { LoginForm, StillSignedIn } from "./LoginForm";
import { RecoveryCodesStep } from "./RecoveryCodesStep";
import { RegisterForm } from "./RegisterForm";
import { SetupStep } from "./SetupStep";

/**
 * A notice of `text` and the failure behind it: its wait, which the login form counts down,
 * or describeError's text.
 */
function noticeOf(text: string, error: CommandError): Notice {
  const wait = retryRequest(error);
  return wait === undefined ? { text, reason: describeError(error) } : { text, wait };
}

/** A wrong code at the code step: the server's 401 `invalid_credentials`. */
function isWrongCode(error: CommandError): boolean {
  return error.api?.status === 401 && error.api.code === "invalid_credentials";
}

/** The sign-in flow, starting at the login form or the register form. */
export function AuthFlow({ mode }: { mode: "login" | "register" }) {
  const location = useLocation();
  const [state, dispatch] = useReducer(authFlowReducer, undefined, () =>
    initialFlow(mode, mode === "login" ? handoffOf(location.state) : undefined),
  );
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { session } = useAppStores();
  const { logOut } = useLogout();
  const { data: profile } = useServerProfile();
  const origin = profile?.state === "set" ? displayOrigin(profile.profile.url) : undefined;

  // Whether this flow is still on screen, which step is current, and the step whose call
  // is running: its controls wait, a second call is ignored (hooks/useStepGuard).
  const { alive, isCurrent, guarded, pending } = useStepGuard(state.stepId);

  /** Into the app, if the step is still current: signed in, a fresh `me`, Home. */
  async function enter(from: number): Promise<void> {
    if (!isCurrent(from)) {
      return;
    }
    session.getState().signIn();
    await queryClient.invalidateQueries({ queryKey: meQuery(api).queryKey });
    if (alive.current) {
      await navigate({ to: "/home" });
    }
  }

  /** The step a password step's answer leads to; its deadline counts from `sentAt`. */
  function proceed(from: number, step: LoginStepResponse, values: LoginValues, sentAt: number) {
    if (step.step === "mfa_required") {
      dispatch({
        type: "codeRequired",
        from,
        username: values.username,
        expiresIn: step.expires_in,
        now: sentAt,
      });
    } else {
      dispatch({
        type: "setupRequired",
        from,
        username: values.username,
        password: values.password,
        expiresIn: step.expires_in,
        now: sentAt,
      });
    }
  }

  const logIn = (values: LoginValues) =>
    guarded(async (from) => {
      const sentAt = Date.now();
      let step: LoginStepResponse;
      try {
        step = await api.login(values);
      } catch (rejection) {
        const error = toApiError(rejection);
        if (error.error.code !== "already_logged_in") {
          throw error;
        }
        await stillHeld(from, values.username);
        return;
      }
      proceed(from, step, values, sentAt);
    });

  /** A session is still held: whose? The typed password is dropped either way. */
  async function stillHeld(from: number, typed: string): Promise<void> {
    try {
      const user = await queryClient.fetchQuery({ ...meQuery(api), staleTime: 0 });
      if (user.username.toLowerCase() === typed.toLowerCase()) {
        await enter(from);
        return;
      }
      dispatch({ type: "stillSignedIn", from, username: user.username });
    } catch (rejection) {
      dispatch({
        type: "backToPassword",
        from,
        username: typed,
        notice: { text: describeError(toApiError(rejection).error), logout: true },
      });
    }
  }

  const register = (values: RegisterValues) =>
    guarded(async (from) => {
      const credentials = { username: values.username, password: values.password };
      await api.register({ invite: values.invite, ...credentials });
      const sentAt = Date.now();
      let step: LoginStepResponse;
      try {
        step = await api.login(credentials);
      } catch (rejection) {
        if (isCurrent(from)) {
          const notice = noticeOf(REGISTERED, toApiError(rejection).error);
          await navigate({
            to: "/login",
            state: (previous) => withHandoff(previous, { username: values.username, notice }),
          });
        }
        return;
      }
      proceed(from, step, credentials, sentAt);
    });

  const verify = (code: string) =>
    guarded(async (from) => {
      try {
        await api.loginMfa(code);
      } catch (rejection) {
        const error = toApiError(rejection);
        if (isWrongCode(error.error)) {
          dispatch({ type: "wrongCode", from });
          return;
        }
        if (error.error.code === "no_pending_step") {
          dispatch({ type: "stepEnded", from });
          return;
        }
        throw error;
      }
      await enter(from);
    });

  // One secret per setup step: requested when the step starts (once, also under
  // StrictMode, which runs effects twice) and on "New QR code".
  const requestSecret = useEnrollment(state.step.kind === "setup" ? state.stepId : null, dispatch);

  const confirm = (code: string) =>
    guarded(async (from) => {
      let answer: Awaited<ReturnType<typeof api.mfaConfirm>>;
      try {
        answer = await api.mfaConfirm(code);
      } catch (rejection) {
        const error = toApiError(rejection);
        if (isStepGone(error.error)) {
          dispatch({ type: "stepEnded", from });
          return;
        }
        throw error;
      }
      dispatch({ type: "confirmed", from, codes: answer.recovery_codes });
    });

  /** After the recovery codes: log in again for the next code; the password goes now. */
  const afterCodes = () =>
    guarded(async (from) => {
      const { username, password } = state;
      if (password === null) {
        dispatch({ type: "backToPassword", from, notice: { text: SETUP_DONE } });
        return;
      }
      const sentAt = Date.now();
      try {
        const step = await api.login({ username, password });
        if (step.step === "mfa_required") {
          dispatch({
            type: "codeRequired",
            from,
            username,
            expiresIn: step.expires_in,
            now: sentAt,
            next: true,
          });
          return;
        }
        dispatch({ type: "backToPassword", from, notice: { text: SETUP_DONE } });
      } catch (rejection) {
        dispatch({
          type: "backToPassword",
          from,
          notice: noticeOf(SETUP_DONE, toApiError(rejection).error),
        });
      }
    });

  const logOutHere = () =>
    void guarded(async (from) => {
      if (await logOut()) {
        dispatch({ type: "backToPassword", from });
      }
    });
  const continueAs = () => void guarded((from) => enter(from));
  const ended = () => dispatch({ type: "stepEnded", from: state.stepId });
  const cancel = () => dispatch({ type: "backToPassword", from: state.stepId });

  const step = state.step;
  switch (step.kind) {
    case "password":
      return (
        <LoginForm
          key={state.stepId}
          origin={origin}
          username={state.username}
          notice={step.notice}
          pending={pending}
          onSubmit={logIn}
          onLogOut={logOutHere}
        />
      );
    case "register":
      return <RegisterForm origin={origin} pending={pending} onSubmit={register} />;
    case "code":
      return (
        <CodeStep
          key={state.stepId}
          step={step}
          pending={pending}
          onSubmit={verify}
          onExpire={ended}
          onCancel={cancel}
        />
      );
    case "setup":
      return (
        <SetupStep
          key={state.stepId}
          step={step}
          pending={pending}
          onNewSecret={() => void requestSecret(state.stepId)}
          onConfirm={confirm}
          onExpire={ended}
          onCancel={cancel}
        />
      );
    case "recovery":
      return <RecoveryCodesStep codes={step.codes} pending={pending} onContinue={afterCodes} />;
    case "stillSignedIn":
      return (
        <StillSignedIn
          username={step.username}
          pending={pending}
          onContinue={continueAs}
          onLogOut={logOutHere}
        />
      );
  }
}
