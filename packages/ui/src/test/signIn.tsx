// The sign-in flow's test helpers: the App on the login form over a FakeApiClient, and
// the answers the flow's tests script. Placeholders only (CLAUDE.md).
import { screen } from "@testing-library/react";

import type { ApiClient } from "../api/ApiClient";
import { fakeApiError } from "../api/FakeApiClient";
import type { ApiErrorDetails } from "../generated/ApiErrorDetails";
import type { LoginStepResponse } from "../generated/LoginStepResponse";
import type { MfaEnrollResponse } from "../generated/MfaEnrollResponse";
import { me, PROFILE } from "./fixtures";
import { type RenderedApp, renderApp } from "./renderApp";

export const PASSWORD = "placeholder password";
export const SECRET = "ABCDEFGHIJKLMNOPQRST";
export const ENROLLMENT: MfaEnrollResponse = {
  otpauth_uri: `otpauth://totp/afkfleet:afkbot1?secret=${SECRET}&issuer=afkfleet`,
  qr_svg: "data:image/svg+xml;base64,PHN2Zy8+",
  secret: SECRET,
};
export const CODES = Array.from({ length: 10 }, (_, i) => `aaaa-bbbb-cccc-dddd-eee${i}`);
export const MFA: LoginStepResponse = { step: "mfa_required", expires_in: 300 };
export const SETUP: LoginStepResponse = { step: "setup_required", expires_in: 600 };

export function refusal(status: number, code: string, extra: Partial<ApiErrorDetails> = {}) {
  return fakeApiError("api", {
    api: {
      status,
      code: code as ApiErrorDetails["code"],
      message: `refused: ${code}`,
      request_id: "01900000-0000-7000-8000-000000000009",
      ...extra,
    },
  });
}

export const WRONG = () => refusal(401, "invalid_credentials");

/** The App on the login form: no session at launch; `loginMfa` (or `signIn()`) starts one. */
export async function atLogin(
  handlers: Partial<ApiClient> = {},
  advanceTimers?: (ms: number) => unknown,
  strict = false,
) {
  const session = { live: false };
  const app = renderApp(
    {
      serverProfile: async () => PROFILE,
      me: async () => {
        if (!session.live) {
          throw fakeApiError("no_session");
        }
        return me();
      },
      loginMfa: async () => {
        session.live = true;
      },
      ...handlers,
    },
    { ...(advanceTimers === undefined ? {} : { advanceTimers }), strict },
  );
  await screen.findByRole("heading", { name: "Log in" });
  return { ...app, session };
}

export async function logIn(app: Awaited<ReturnType<typeof atLogin>>, username = "afkbot1") {
  await app.user.type(screen.getByLabelText("Username"), username);
  await app.user.type(screen.getByLabelText("Password"), PASSWORD);
  await app.user.click(screen.getByRole("button", { name: "Log in" }));
}

export async function enterCode(app: Pick<RenderedApp, "user">, code: string) {
  const field = await screen.findByLabelText("Code");
  await app.user.clear(field);
  await app.user.type(field, code);
  await app.user.click(screen.getByRole("button", { name: /^(Verify|Confirm)$/ }));
}

/** Everything the QueryClient holds, as text. */
export function cacheText(app: Pick<RenderedApp, "runtime">): string {
  const queries = app.runtime.queryClient
    .getQueryCache()
    .getAll()
    .map((query) => ({ key: query.queryKey, state: query.state }));
  const mutations = app.runtime.queryClient
    .getMutationCache()
    .getAll()
    .map((mutation) => mutation.state);
  return JSON.stringify({ queries, mutations });
}
