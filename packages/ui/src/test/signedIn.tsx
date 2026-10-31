// The signed-in screens' test helpers: the App signed in over a FakeApiClient, on Home or
// on another screen. Placeholders only (CLAUDE.md).
import { act, screen, within } from "@testing-library/react";

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

/** Waits until Home shows for `username`: its bots' heading, and the Header's user. */
export async function onHome(username = me().username): Promise<void> {
  await screen.findByRole("heading", { name: "Bots" });
  await within(screen.getByRole("banner")).findByText(username, { exact: true });
}

/** The App signed in as `options.user`, on Home, without bots unless `handlers` give some. */
export async function signedIn(handlers: Partial<ApiClient> = {}, options: SignedInOptions = {}) {
  const user = options.user ?? me();
  const app = renderApp(
    {
      serverProfile: async () => PROFILE,
      me: async () => user,
      bots: async () => ({ items: [] }),
      modes: async () => ({ items: [] }),
      botChat: async () => ({ items: [] }),
      ...handlers,
    },
    {
      ...(options.advanceTimers === undefined ? {} : { advanceTimers: options.advanceTimers }),
      ...(options.strict === undefined ? {} : { strict: options.strict }),
    },
  );
  await onHome(user.username);
  return app;
}

/** The App signed in, then on `to`. */
export async function atScreen(
  to:
    | "/modes"
    | "/modes/new"
    | "/accounts"
    | "/accounts/link"
    | "/profile"
    | "/profile/password"
    | "/profile/2fa"
    | "/admin/users"
    | "/admin/invites"
    | "/admin/servers"
    | "/admin/servers/new"
    | "/admin/agents"
    | "/admin/audit",
  handlers: Partial<ApiClient> = {},
  options: SignedInOptions = {},
) {
  const app = await signedIn(handlers, options);
  await act(() => app.runtime.router.navigate({ to }));
  return app;
}

/** The App signed in, then on bot `id`'s screen. */
export async function atBot(
  id: string,
  handlers: Partial<ApiClient> = {},
  options: SignedInOptions = {},
) {
  const app = await signedIn(handlers, options);
  await act(() => app.runtime.router.navigate({ to: "/bots/$id", params: { id } }));
  return app;
}

/** The App signed in, then on account `id`'s screen, or on its link-again screen. */
export async function atAccount(
  id: string,
  handlers: Partial<ApiClient> = {},
  options: SignedInOptions & { readonly again?: boolean } = {},
) {
  const app = await signedIn(handlers, options);
  await act(() =>
    app.runtime.router.navigate(
      options.again === true
        ? { to: "/accounts/$id/link-again", params: { id } }
        : { to: "/accounts/$id", params: { id } },
    ),
  );
  return app;
}
