// The UI's API client in the desktop app (Plan.md P8.5, ADR-0017): each method invokes
// the Rust command of the same name with exactly the args `src-tauri/tests/ipc-contract.json`
// lists. Tauri's `invoke` is injected, so tests pass a fake; this file imports nothing from
// Tauri but its types. A rejection that's a `CommandError` becomes an `ApiError` of it;
// anything else (Tauri's own refusals are plain strings) becomes `ipc`, its text dropped.
import type { ApiClient } from "@afkfleet/ui";
import { toApiError } from "@afkfleet/ui";
import type { CreatedInviteResponse } from "@afkfleet/ui/generated/CreatedInviteResponse";
import type { CreateInviteRequest } from "@afkfleet/ui/generated/CreateInviteRequest";
import type { InvitePage } from "@afkfleet/ui/generated/InvitePage";
import type { LoginRequest } from "@afkfleet/ui/generated/LoginRequest";
import type { LoginStepResponse } from "@afkfleet/ui/generated/LoginStepResponse";
import type { LogoutResponse } from "@afkfleet/ui/generated/LogoutResponse";
import type { MeResponse } from "@afkfleet/ui/generated/MeResponse";
import type { MfaConfirmResponse } from "@afkfleet/ui/generated/MfaConfirmResponse";
import type { MfaEnrollResponse } from "@afkfleet/ui/generated/MfaEnrollResponse";
import type { PasswordChangeRequest } from "@afkfleet/ui/generated/PasswordChangeRequest";
import type { RegisterRequest } from "@afkfleet/ui/generated/RegisterRequest";
import type { RegisterResponse } from "@afkfleet/ui/generated/RegisterResponse";
import type { ServerProfileRequest } from "@afkfleet/ui/generated/ServerProfileRequest";
import type { ServerProfileResponse } from "@afkfleet/ui/generated/ServerProfileResponse";
import type { ServerProfileState } from "@afkfleet/ui/generated/ServerProfileState";
import type { SessionPage } from "@afkfleet/ui/generated/SessionPage";
import type { SetupStepResponse } from "@afkfleet/ui/generated/SetupStepResponse";
import type { StepUpRequest } from "@afkfleet/ui/generated/StepUpRequest";
import type { UserPage } from "@afkfleet/ui/generated/UserPage";
import type { UserPatchRequest } from "@afkfleet/ui/generated/UserPatchRequest";
import type { UserResponse } from "@afkfleet/ui/generated/UserResponse";
import type { InvokeArgs } from "@tauri-apps/api/core";

/** Tauri's `invoke`, as this client uses it. */
export type Invoke = <T>(command: string, args?: InvokeArgs) => Promise<T>;

/** The desktop app's API client; see the module comment. */
export class TauriApiClient implements ApiClient {
  readonly #invoke: Invoke;

  /** A client over `invoke`: `@tauri-apps/api/core`'s in the app, a fake in tests. */
  constructor(invoke: Invoke) {
    this.#invoke = invoke;
  }

  /** Invokes `command` with `args`, every rejection as an `ApiError`. */
  async #call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
    try {
      return await this.#invoke<T>(command, args);
    } catch (rejection) {
      throw toApiError(rejection);
    }
  }

  serverProfile(): Promise<ServerProfileState> {
    return this.#call("server_profile");
  }

  setServerProfile(request: ServerProfileRequest): Promise<ServerProfileResponse> {
    return this.#call("set_server_profile", { request });
  }

  checkServer(): Promise<void> {
    return this.#call("check_server");
  }

  login(request: LoginRequest): Promise<LoginStepResponse> {
    return this.#call("login", { request });
  }

  loginMfa(code: string): Promise<void> {
    return this.#call("login_mfa", { code });
  }

  logout(): Promise<LogoutResponse> {
    return this.#call("logout");
  }

  register(request: RegisterRequest): Promise<RegisterResponse> {
    return this.#call("register", { request });
  }

  stepUp(request: StepUpRequest): Promise<void> {
    return this.#call("step_up", { request });
  }

  mfaEnroll(): Promise<MfaEnrollResponse> {
    return this.#call("mfa_enroll");
  }

  mfaConfirm(code: string): Promise<MfaConfirmResponse> {
    return this.#call("mfa_confirm", { code });
  }

  mfaReset(): Promise<SetupStepResponse> {
    return this.#call("mfa_reset");
  }

  me(): Promise<MeResponse> {
    return this.#call("me");
  }

  changePassword(request: PasswordChangeRequest): Promise<void> {
    return this.#call("change_password", { request });
  }

  sessions(before?: string, limit?: number): Promise<SessionPage> {
    return this.#call("sessions", { before, limit });
  }

  revokeSession(id: string): Promise<void> {
    return this.#call("revoke_session", { id });
  }

  users(before?: string, limit?: number): Promise<UserPage> {
    return this.#call("users", { before, limit });
  }

  patchUser(id: string, request: UserPatchRequest): Promise<UserResponse> {
    return this.#call("patch_user", { id, request });
  }

  invites(before?: string, limit?: number): Promise<InvitePage> {
    return this.#call("invites", { before, limit });
  }

  createInvite(request: CreateInviteRequest): Promise<CreatedInviteResponse> {
    return this.#call("create_invite", { request });
  }

  revokeInvite(id: string): Promise<void> {
    return this.#call("revoke_invite", { id });
  }
}
