// The UI's API client in the desktop app (Plan.md P8.5, ADR-0017): each method invokes
// the Rust command of the same name with exactly the args `src-tauri/tests/ipc-contract.json`
// lists. Tauri's `invoke` and the factory of the live events' `Channel` are injected, so
// tests and the e2e build pass their own; this file imports nothing from Tauri but its
// types. A rejection that's a `CommandError` becomes an `ApiError` of it; anything else
// (Tauri's own refusals are plain strings) becomes `ipc`, its text dropped.
import type { ApiClient } from "@afkfleet/ui";
import { toApiError } from "@afkfleet/ui";
import type { AccountPage } from "@afkfleet/ui/generated/AccountPage";
import type { AccountResponse } from "@afkfleet/ui/generated/AccountResponse";
import type { ChangeGrantRequest } from "@afkfleet/ui/generated/ChangeGrantRequest";
import type { CreatedInviteResponse } from "@afkfleet/ui/generated/CreatedInviteResponse";
import type { CreatedOfflineAccountResponse } from "@afkfleet/ui/generated/CreatedOfflineAccountResponse";
import type { CreateGrantRequest } from "@afkfleet/ui/generated/CreateGrantRequest";
import type { CreateInviteRequest } from "@afkfleet/ui/generated/CreateInviteRequest";
import type { CreateOfflineAccountRequest } from "@afkfleet/ui/generated/CreateOfflineAccountRequest";
import type { GrantPage } from "@afkfleet/ui/generated/GrantPage";
import type { GrantResponse } from "@afkfleet/ui/generated/GrantResponse";
import type { InvitePage } from "@afkfleet/ui/generated/InvitePage";
import type { LinkFlowResponse } from "@afkfleet/ui/generated/LinkFlowResponse";
import type { LinkStartedResponse } from "@afkfleet/ui/generated/LinkStartedResponse";
import type { LiveUpdateMessage } from "@afkfleet/ui/generated/LiveUpdateMessage";
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

/**
 * Opens a channel whose messages go to `onMessage`, and answers the value `invoke` sends for
 * it: `@tauri-apps/api/core`'s `Channel` in the app, which needs Tauri's internals in the
 * page.
 */
export type OpenChannel = <T>(onMessage: (message: T) => void) => unknown;

/** The desktop app's API client; see the module comment. */
export class TauriApiClient implements ApiClient {
  readonly #invoke: Invoke;
  readonly #openChannel: OpenChannel;

  /**
   * A client over `invoke` and `openChannel`: `@tauri-apps/api/core`'s in the app, fakes in
   * tests.
   */
  constructor(invoke: Invoke, openChannel: OpenChannel) {
    this.#invoke = invoke;
    this.#openChannel = openChannel;
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

  startLink(): Promise<LinkStartedResponse> {
    return this.#call("start_link");
  }

  linkStatus(id: string): Promise<LinkFlowResponse> {
    return this.#call("link_status", { id });
  }

  createOfflineAccount(
    request: CreateOfflineAccountRequest,
  ): Promise<CreatedOfflineAccountResponse> {
    return this.#call("create_offline_account", { request });
  }

  accounts(before?: string, limit?: number): Promise<AccountPage> {
    return this.#call("accounts", { before, limit });
  }

  accountsAwaitingApproval(before?: string, limit?: number): Promise<AccountPage> {
    return this.#call("accounts_awaiting_approval", { before, limit });
  }

  account(id: string): Promise<AccountResponse> {
    return this.#call("account", { id });
  }

  deleteAccount(id: string): Promise<void> {
    return this.#call("delete_account", { id });
  }

  approveAccount(id: string): Promise<AccountResponse> {
    return this.#call("approve_account", { id });
  }

  refuseAccount(id: string): Promise<void> {
    return this.#call("refuse_account", { id });
  }

  relinkAccount(id: string): Promise<LinkStartedResponse> {
    return this.#call("relink_account", { id });
  }

  accountGrants(id: string, before?: string, limit?: number): Promise<GrantPage> {
    return this.#call("account_grants", { id, before, limit });
  }

  createGrant(id: string, request: CreateGrantRequest): Promise<GrantResponse> {
    return this.#call("create_grant", { id, request });
  }

  changeGrant(id: string, user: string, request: ChangeGrantRequest): Promise<GrantResponse> {
    return this.#call("change_grant", { id, user, request });
  }

  revokeGrant(id: string, user: string): Promise<void> {
    return this.#call("revoke_grant", { id, user });
  }

  openVerificationLink(): Promise<void> {
    return this.#call("open_verification_link");
  }

  async watchEvents(onUpdate: (update: LiveUpdateMessage) => void): Promise<void> {
    // Updates reach `onUpdate` only while the call runs.
    let open = true;
    const updates = this.#openChannel<LiveUpdateMessage>((update) => {
      if (open) {
        onUpdate(update);
      }
    });
    try {
      await this.#call("watch_events", { updates });
    } finally {
      open = false;
    }
  }
}
