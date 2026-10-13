// Answers the UI's tests script their FakeApiClient with. Placeholders only (CLAUDE.md).
import type { MeResponse } from "../generated/MeResponse";
import type { ServerProfileState } from "../generated/ServerProfileState";
import type { UserRole } from "../generated/UserRole";

/** The test server's origin, as the Rust side writes it. */
export const ORIGIN = "https://example.com/";

/** A profile for `ORIGIN` without a custom CA. */
export const PROFILE: ServerProfileState = { state: "set", profile: { url: ORIGIN } };

/** The signed-in user. */
export function me(role: UserRole = "member", username = "afkbot1"): MeResponse {
  return {
    id: "01900000-0000-7000-8000-000000000001",
    username,
    role,
    created_at: "2026-10-13T08:00:00Z",
    password_changed_at: "2026-10-13T08:00:00Z",
    mfa_confirmed_at: "2026-10-13T08:05:00Z",
    recovery_codes_left: 10,
    failed_code_attempts: { count: 0 },
  };
}

/** A promise and the functions that settle it, for holding an answer back. */
export function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
