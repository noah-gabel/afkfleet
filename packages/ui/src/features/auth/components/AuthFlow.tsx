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
// Every way in sets the session signed in, with a new generation, before opening Home:
// the code step's success, a login that finds the same user's session still held, and
// "Continue as" another user's.
import type { QueryClient } from "@tanstack/react-query";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useReducer } from "react";

import type { ApiClient } from "../../../api/ApiClient";
import { type ApiError, toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { meQuery, useServerProfile } from "../../../api/queries";
import { useAppStores } from "../../../app/context";
import type { CommandError } from "../../../generated/CommandError";
import type { LoginStepResponse } from "../../../generated/LoginStepResponse";
import { displayOrigin } from "../../../lib/origin";
import { useLogout } from "../../shell/hooks/useLogout";
import { authFlowReducer, initialFlow, REGISTERED, SETUP_DONE } from "../authFlowReducer";
import type { LoginValues, RegisterValues } from "../schemas";
import { CodeStep } from "./CodeStep";
import { LoginForm, StillSignedIn } from "./LoginForm";
import { RecoveryCodesStep } from "./RecoveryCodesStep";
import { RegisterForm } from "./RegisterForm";
import { SetupStep } from "./SetupStep";

/** A wrong code at the code step: the server's 401 `invalid_credentials`. */
function isWrongCode(error: CommandError): boolean {
  return error.api?.status === 401 && error.api.code === "invalid_credentials";
}

/** A setup or code step the server or the client no longer holds. */
function isStepGone(error: CommandError): boolean {
  return error.code === "no_pending_step" || error.api?.status === 401;
}

/** The sign-in flow, starting at the login form or the register form. */
export function AuthFlow({ mode }: { mode: "login" | "register" }) {
  const [state, dispatch] = useReducer(authFlowReducer, mode, initialFlow);
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { session } = useAppStores();
  const { logOut } = useLogout();
  const { data: profile } = useServerProfile();
  const origin = profile?.state === "set" ? displayOrigin(profile.profile.url) : undefined;

  /** Into the app: signed in, a new generation, a fresh `me`, Home. */
  async function enter(): Promise<void> {
    session.getState().signIn();
    await queryClient.invalidateQueries({ queryKey: meQuery(api).queryKey });
    await navigate({ to: "/home" });
  }

  /** The step a password step's answer leads to. */
  function proceed(step: LoginStepResponse, values: LoginValues): void {
    if (step.step === "mfa_required") {
      dispatch({
        type: "codeRequired",
        username: values.username,
        expiresIn: step.expires_in,
        now: Date.now(),
      });
    } else {
      dispatch({
        type: "setupRequired",
        username: values.username,
        password: values.password,
        expiresIn: step.expires_in,
        now: Date.now(),
      });
    }
  }

  async function logIn(values: LoginValues): Promise<void> {
    let step: LoginStepResponse;
    try {
      step = await api.login(values);
    } catch (rejection) {
      const error = toApiError(rejection);
      if (error.error.code !== "already_logged_in") {
        throw error;
      }
      await stillHeld(values.username);
      return;
    }
    proceed(step, values);
  }

  /** A session is still held: whose? The typed password is dropped either way. */
  async function stillHeld(typed: string): Promise<void> {
    try {
      const user = await fetchMe(queryClient, api);
      if (user.username.toLowerCase() === typed.toLowerCase()) {
        await enter();
        return;
      }
      dispatch({ type: "stillSignedIn", username: user.username });
    } catch (rejection) {
      dispatch({
        type: "backToPassword",
        username: typed,
        notice: { text: describeError(toApiError(rejection).error), logout: true },
      });
    }
  }

  async function register(values: RegisterValues): Promise<void> {
    const credentials = { username: values.username, password: values.password };
    await api.register({ invite: values.invite, ...credentials });
    let step: LoginStepResponse;
    try {
      step = await api.login(credentials);
    } catch (rejection) {
      dispatch({
        type: "backToPassword",
        username: values.username,
        notice: { text: REGISTERED, reason: describeError(toApiError(rejection).error) },
      });
      return;
    }
    proceed(step, credentials);
  }

  async function verify(code: string): Promise<void> {
    try {
      await api.loginMfa(code);
    } catch (rejection) {
      const error = toApiError(rejection);
      if (isWrongCode(error.error)) {
        dispatch({ type: "wrongCode" });
        return;
      }
      if (error.error.code === "no_pending_step") {
        dispatch({ type: "stepEnded" });
        return;
      }
      throw error;
    }
    await enter();
  }

  async function enroll() {
    try {
      return await api.mfaEnroll();
    } catch (rejection) {
      throw stepGoneOr(toApiError(rejection));
    }
  }

  async function confirm(code: string): Promise<void> {
    let answer: Awaited<ReturnType<ApiClient["mfaConfirm"]>>;
    try {
      answer = await api.mfaConfirm(code);
    } catch (rejection) {
      throw stepGoneOr(toApiError(rejection));
    }
    dispatch({ type: "confirmed", codes: answer.recovery_codes });
  }

  /** Ends the step if `error` says it's gone; returns `error` to rethrow. */
  function stepGoneOr(error: ApiError): ApiError {
    if (isStepGone(error.error)) {
      dispatch({ type: "stepEnded" });
    }
    return error;
  }

  /** After the recovery codes: log in again for the next code; the password goes now. */
  async function afterCodes(): Promise<void> {
    const { username, password } = state;
    if (password === null) {
      dispatch({ type: "backToPassword", notice: { text: SETUP_DONE } });
      return;
    }
    try {
      const step = await api.login({ username, password });
      if (step.step === "mfa_required") {
        dispatch({
          type: "codeRequired",
          username,
          expiresIn: step.expires_in,
          now: Date.now(),
          next: true,
        });
        return;
      }
      dispatch({ type: "backToPassword", notice: { text: SETUP_DONE } });
    } catch (rejection) {
      dispatch({
        type: "backToPassword",
        notice: { text: SETUP_DONE, reason: describeError(toApiError(rejection).error) },
      });
    }
  }

  async function logOutHere(): Promise<void> {
    if (await logOut()) {
      dispatch({ type: "backToPassword" });
    }
  }

  const ended = () => dispatch({ type: "stepEnded" });
  const cancel = () => dispatch({ type: "backToPassword" });
  const step = state.step;
  switch (step.kind) {
    case "password":
      return (
        <LoginForm
          key={state.round}
          origin={origin}
          username={state.username}
          notice={step.notice}
          onSubmit={logIn}
          onLogOut={() => void logOutHere()}
        />
      );
    case "register":
      return <RegisterForm origin={origin} onSubmit={register} />;
    case "code":
      return <CodeStep step={step} onSubmit={verify} onExpire={ended} onCancel={cancel} />;
    case "setup":
      return (
        <SetupStep
          step={step}
          onEnroll={enroll}
          onConfirm={confirm}
          onExpire={ended}
          onCancel={cancel}
        />
      );
    case "recovery":
      return <RecoveryCodesStep codes={step.codes} onContinue={afterCodes} />;
    case "stillSignedIn":
      return (
        <StillSignedIn
          username={step.username}
          onContinue={() => void enter()}
          onLogOut={() => void logOutHere()}
        />
      );
  }
}

/** The user of the session still held, fetched afresh (it's no secret). */
function fetchMe(queryClient: QueryClient, api: ApiClient) {
  return queryClient.fetchQuery({ ...meQuery(api), staleTime: 0 });
}
