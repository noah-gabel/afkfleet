// The app's API as the UI sees it (Plan.md P8.5, ADR-0017): one method per command of
// the desktop app's Rust side. The UI never knows the platform: `apps/desktop` injects a
// `TauriApiClient`, tests a `FakeApiClient`. No method takes or returns a token. Every
// method rejects with an `ApiError` (ApiError.ts).
import type { CreatedInviteResponse } from "../generated/CreatedInviteResponse";
import type { CreateInviteRequest } from "../generated/CreateInviteRequest";
import type { InvitePage } from "../generated/InvitePage";
import type { LoginRequest } from "../generated/LoginRequest";
import type { LoginStepResponse } from "../generated/LoginStepResponse";
import type { LogoutResponse } from "../generated/LogoutResponse";
import type { MeResponse } from "../generated/MeResponse";
import type { MfaConfirmResponse } from "../generated/MfaConfirmResponse";
import type { MfaEnrollResponse } from "../generated/MfaEnrollResponse";
import type { PasswordChangeRequest } from "../generated/PasswordChangeRequest";
import type { RegisterRequest } from "../generated/RegisterRequest";
import type { RegisterResponse } from "../generated/RegisterResponse";
import type { ServerProfileRequest } from "../generated/ServerProfileRequest";
import type { ServerProfileResponse } from "../generated/ServerProfileResponse";
import type { ServerProfileState } from "../generated/ServerProfileState";
import type { SessionPage } from "../generated/SessionPage";
import type { SetupStepResponse } from "../generated/SetupStepResponse";
import type { StepUpRequest } from "../generated/StepUpRequest";
import type { UserPage } from "../generated/UserPage";
import type { UserPatchRequest } from "../generated/UserPatchRequest";
import type { UserResponse } from "../generated/UserResponse";

/** The app's API; see the module comment. IDs are the server's UUID strings. */
export interface ApiClient {
  /** The current server profile, or why there's none. */
  serverProfile(): Promise<ServerProfileState>;
  /** Sets the server; any change but an identical profile logs out of the old one first. */
  setServerProfile(request: ServerProfileRequest): Promise<ServerProfileResponse>;
  /** Whether the current server can serve requests. */
  checkServer(): Promise<void>;
  /** The password step; its token stays in the app's Rust side. */
  login(request: LoginRequest): Promise<LoginStepResponse>;
  /** The code step, which starts the session. */
  loginMfa(code: string): Promise<void>;
  /** Logs out: always ends the session here; says whether the server confirmed it. */
  logout(): Promise<LogoutResponse>;
  /** An account from an invite; log in with it next. */
  register(request: RegisterRequest): Promise<RegisterResponse>;
  /** The password again, for step-up actions. */
  stepUp(request: StepUpRequest): Promise<void>;
  /** A new TOTP secret for the pending setup step. */
  mfaEnroll(): Promise<MfaEnrollResponse>;
  /** The authenticator's first code; answers the recovery codes, shown once. */
  mfaConfirm(code: string): Promise<MfaConfirmResponse>;
  /** Starts replacing the authenticator, after a step-up. */
  mfaReset(): Promise<SetupStepResponse>;
  /** The user's profile. */
  me(): Promise<MeResponse>;
  /** A new password, after a step-up. */
  changePassword(request: PasswordChangeRequest): Promise<void>;
  /** A page of the user's live sessions. */
  sessions(before?: string, limit?: number): Promise<SessionPage>;
  /** Ends another session of the user. */
  revokeSession(id: string): Promise<void>;
  /** A page of the users (Admins and the Owner). */
  users(before?: string, limit?: number): Promise<UserPage>;
  /** One change of a user: a role or `disabled`, after a step-up. */
  patchUser(id: string, request: UserPatchRequest): Promise<UserResponse>;
  /** A page of the invites. */
  invites(before?: string, limit?: number): Promise<InvitePage>;
  /** A new invite, whose code is answered this once, after a step-up. */
  createInvite(request: CreateInviteRequest): Promise<CreatedInviteResponse>;
  /** Revokes a pending invite, after a step-up. */
  revokeInvite(id: string): Promise<void>;
}

/** A method of the API. */
export type ApiMethod = keyof ApiClient;

/** Every method, in the order of build.rs's commands. */
export const API_METHODS = [
  "serverProfile",
  "setServerProfile",
  "checkServer",
  "login",
  "loginMfa",
  "logout",
  "register",
  "stepUp",
  "mfaEnroll",
  "mfaConfirm",
  "mfaReset",
  "me",
  "changePassword",
  "sessions",
  "revokeSession",
  "users",
  "patchUser",
  "invites",
  "createInvite",
  "revokeInvite",
] as const satisfies readonly ApiMethod[];
