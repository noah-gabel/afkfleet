// The signed-in screens' test helpers: the App signed in over a FakeApiClient, on Home or
// on another screen. Placeholders only (CLAUDE.md).
import { act, screen } from "@testing-library/react";

import type { ApiClient } from "../api/ApiClient";
import type { MeResponse } from "../generated/MeResponse";
import { me, PROFILE } from "./fixtures";
import { renderApp } from "./renderApp";

/** How a signed-in App starts. */
export interface SignedInOptions {
  /** `me`'s answer; a Member by default. */
  readonly user?: MeResponse;
  readonly advanceTimers?: (ms: number) => unknown;
  readonly strict?: boolean;
}

/** The App signed in as `options.user`, on Home. */
export async function signedIn(handlers: Partial<ApiClient> = {}, options: SignedInOptions = {}) {
  const user = options.user ?? me();
  const app = renderApp(
    { serverProfile: async () => PROFILE, me: async () => user, ...handlers },
    {
      ...(options.advanceTimers === undefined ? {} : { advanceTimers: options.advanceTimers }),
      ...(options.strict === undefined ? {} : { strict: options.strict }),
    },
  );
  await screen.findByText(new RegExp(`Signed in as ${user.username}`));
  return app;
}

/** The App signed in, then on `to`. */
export async function atScreen(
  to: "/profile" | "/profile/password" | "/profile/2fa" | "/admin/users" | "/admin/invites",
  handlers: Partial<ApiClient> = {},
  options: SignedInOptions = {},
) {
  const app = await signedIn(handlers, options);
  await act(() => app.runtime.router.navigate({ to }));
  return app;
}
