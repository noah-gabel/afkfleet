// The profile's two flows (Plan.md P8.7, ADR-0016, ADR-0017), each on a route of its own:
// changing the password (`/profile/password`) and replacing the authenticator
// (`/profile/2fa`). Both start with the password again (a step-up); its state is a reducer
// here (profileFlowReducer.ts), so the TOTP secret and the recovery codes live only in this
// component and go when the user leaves the route.
//
// Every call that carries a secret in its request or its answer (`stepUp`, `changePassword`,
// `mfaReset` with its `mfaEnroll` and `mfaConfirm`) is a direct call, never a TanStack query
// or mutation, whose caches would keep it (CLAUDE.md). Each runs for the step it started in
// (hooks/useStepGuard): while it runs, its step's controls wait; a late answer changes
// nothing, and never toasts or navigates after the user left. A step's deadline counts from
// when its request was sent: the step-up's 5 minutes, or the setup token's `expires_in`.
//
// When it's done, the server has ended every other session of the user: the toast says so,
// and `me` and the sessions are fetched again.
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useReducer } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { SESSIONS_KEY } from "../../../api/queries";
import type { MfaConfirmResponse } from "../../../generated/MfaConfirmResponse";
import { useStepGuard } from "../../../hooks/useStepGuard";
import { isStepUpRequired } from "../../../lib/stepUp";
import { RecoveryCodesStep } from "../../auth/components/RecoveryCodesStep";
import { SetupStep } from "../../auth/components/SetupStep";
import { useEnrollment } from "../../auth/hooks/useEnrollment";
import { isStepGone } from "../../auth/stepErrors";
import { INITIAL_PROFILE_FLOW, profileFlowReducer } from "../profileFlowReducer";
import { ConfirmStep } from "./ConfirmStep";
import { NewPasswordStep } from "./NewPasswordStep";

/** The toast after a password change. */
export const PASSWORD_CHANGED = "Your password was changed. Every other session was logged out.";
/** The toast after the authenticator was replaced. */
export const MFA_REPLACED =
  "Your authenticator was replaced. Every other session was logged out; delete the old entry from your authenticator app.";

/** Each flow's first step. */
const CONFIRM = {
  password: { title: "Change password", intro: "First, enter your current password." },
  mfa: {
    title: "Replace your authenticator",
    intro:
      "First, enter your password. Your current authenticator keeps working until the new one is confirmed.",
  },
} as const;

/** The setup step's texts here. */
const REPLACE_SETUP = {
  title: "Set up your new authenticator",
  intro:
    "Scan the QR code with your authenticator app, or type the key into it, then enter the code it shows. Your current authenticator keeps working until you confirm.",
  hint: "Only the newest QR code works. Once you've confirmed, delete the old entry for this account from your authenticator app.",
};

/** The recovery codes step's texts here. */
const NEW_CODES = {
  title: "New recovery codes",
  intro:
    "Your old recovery codes no longer work. Each of these works once instead of a code from your app. They won't be shown again.",
  continueLabel: "Finish",
};

/** One of the profile's flows; see the module comment. */
export function ProfileFlow({ mode }: { mode: "password" | "mfa" }) {
  const [state, dispatch] = useReducer(profileFlowReducer, INITIAL_PROFILE_FLOW);
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { alive, isCurrent, guarded, pending } = useStepGuard(state.stepId);
  const requestSecret = useEnrollment(state.step.kind === "setup" ? state.stepId : null, dispatch);

  /** Done, if the step is still current: the toast, fresh data, the profile. */
  async function done(from: number, message: string): Promise<void> {
    if (!isCurrent(from)) {
      return;
    }
    toast.success(message);
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["me"] }),
      queryClient.invalidateQueries({ queryKey: SESSIONS_KEY }),
    ]);
    if (alive.current) {
      await navigate({ to: "/profile" });
    }
  }

  const confirm = (password: string) =>
    guarded(async (from) => {
      const sentAt = Date.now();
      await api.stepUp({ password });
      if (mode === "password") {
        dispatch({ type: "steppedUp", from, now: sentAt });
        return;
      }
      const resetAt = Date.now();
      let reset: Awaited<ReturnType<typeof api.mfaReset>>;
      try {
        reset = await api.mfaReset();
      } catch (rejection) {
        const error = toApiError(rejection);
        if (isStepUpRequired(error.error)) {
          dispatch({ type: "stepEnded", from });
          return;
        }
        throw error;
      }
      dispatch({ type: "resetStarted", from, expiresIn: reset.expires_in, now: resetAt });
    });

  const change = (password: string) =>
    guarded(async (from) => {
      try {
        await api.changePassword({ new_password: password });
      } catch (rejection) {
        const error = toApiError(rejection);
        if (isStepUpRequired(error.error)) {
          dispatch({ type: "stepEnded", from });
          return;
        }
        throw error;
      }
      await done(from, PASSWORD_CHANGED);
    });

  const confirmCode = (code: string) =>
    guarded(async (from) => {
      let answer: MfaConfirmResponse;
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

  const finish = () => guarded((from) => done(from, MFA_REPLACED));
  const ended = () => dispatch({ type: "stepEnded", from: state.stepId });
  const cancel = () => void navigate({ to: "/profile" });

  const step = state.step;
  switch (step.kind) {
    case "confirm":
      return (
        <ConfirmStep
          key={state.stepId}
          title={CONFIRM[mode].title}
          intro={CONFIRM[mode].intro}
          notice={step.notice}
          pending={pending}
          onConfirm={confirm}
          onCancel={cancel}
        />
      );
    case "newPassword":
      return (
        <NewPasswordStep
          key={state.stepId}
          step={step}
          pending={pending}
          onSubmit={change}
          onExpire={ended}
          onCancel={cancel}
        />
      );
    case "setup":
      return (
        <SetupStep
          key={state.stepId}
          step={step}
          texts={REPLACE_SETUP}
          pending={pending}
          onNewSecret={() => void requestSecret(state.stepId)}
          onConfirm={confirmCode}
          onExpire={ended}
          onCancel={cancel}
        />
      );
    case "recovery":
      return (
        <RecoveryCodesStep
          codes={step.codes}
          texts={NEW_CODES}
          pending={pending}
          onContinue={finish}
        />
      );
  }
}
