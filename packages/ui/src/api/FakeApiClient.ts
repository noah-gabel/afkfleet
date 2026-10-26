// The API client for tests (Plan.md P8.5, ADR-0017): each test scripts only the methods
// it needs; any other method rejects with an `ApiError` that names it, so a test can't
// pass by accident. Every call is recorded with its arguments, for checking interactions.

import type { AccountPage } from "../generated/AccountPage";
import type { AccountResponse } from "../generated/AccountResponse";
import type { ApiErrorDetails } from "../generated/ApiErrorDetails";
import type { ChangeGrantRequest } from "../generated/ChangeGrantRequest";
import type { CommandErrorCode } from "../generated/CommandErrorCode";
import type { CreatedInviteResponse } from "../generated/CreatedInviteResponse";
import type { CreatedOfflineAccountResponse } from "../generated/CreatedOfflineAccountResponse";
import type { CreateGrantRequest } from "../generated/CreateGrantRequest";
import type { CreateInviteRequest } from "../generated/CreateInviteRequest";
import type { CreateOfflineAccountRequest } from "../generated/CreateOfflineAccountRequest";
import type { FieldError } from "../generated/FieldError";
import type { GrantPage } from "../generated/GrantPage";
import type { GrantResponse } from "../generated/GrantResponse";
import type { InvitePage } from "../generated/InvitePage";
import type { LinkFlowResponse } from "../generated/LinkFlowResponse";
import type { LinkStartedResponse } from "../generated/LinkStartedResponse";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
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
import type { ApiClient, ApiMethod } from "./ApiClient";
import { ApiError } from "./ApiError";

/** One recorded call. */
export interface FakeCall {
  readonly method: ApiMethod;
  readonly args: readonly unknown[];
}

/** A scripted API client; see the module comment. */
export class FakeApiClient implements ApiClient {
  /** Every call so far, in order. */
  readonly calls: FakeCall[] = [];
  readonly #handlers: Partial<ApiClient>;

  /** A client whose methods answer through `handlers`. */
  constructor(handlers: Partial<ApiClient> = {}) {
    this.#handlers = handlers;
  }

  /** Records the call and returns the method's handler, or throws when there's none. */
  #handler<M extends ApiMethod>(method: M, args: readonly unknown[]): ApiClient[M] {
    this.calls.push({ method, args });
    const handler = this.#handlers[method];
    if (handler === undefined) {
      throw new ApiError({ code: "ipc", message: `FakeApiClient has no handler for ${method}` });
    }
    return handler as ApiClient[M];
  }

  async serverProfile(): Promise<ServerProfileState> {
    return this.#handler("serverProfile", [])();
  }

  async setServerProfile(request: ServerProfileRequest): Promise<ServerProfileResponse> {
    return this.#handler("setServerProfile", [request])(request);
  }

  async checkServer(): Promise<void> {
    return this.#handler("checkServer", [])();
  }

  async login(request: LoginRequest): Promise<LoginStepResponse> {
    return this.#handler("login", [request])(request);
  }

  async loginMfa(code: string): Promise<void> {
    return this.#handler("loginMfa", [code])(code);
  }

  async logout(): Promise<LogoutResponse> {
    return this.#handler("logout", [])();
  }

  async register(request: RegisterRequest): Promise<RegisterResponse> {
    return this.#handler("register", [request])(request);
  }

  async stepUp(request: StepUpRequest): Promise<void> {
    return this.#handler("stepUp", [request])(request);
  }

  async mfaEnroll(): Promise<MfaEnrollResponse> {
    return this.#handler("mfaEnroll", [])();
  }

  async mfaConfirm(code: string): Promise<MfaConfirmResponse> {
    return this.#handler("mfaConfirm", [code])(code);
  }

  async mfaReset(): Promise<SetupStepResponse> {
    return this.#handler("mfaReset", [])();
  }

  async me(): Promise<MeResponse> {
    return this.#handler("me", [])();
  }

  async changePassword(request: PasswordChangeRequest): Promise<void> {
    return this.#handler("changePassword", [request])(request);
  }

  async sessions(before?: string, limit?: number): Promise<SessionPage> {
    return this.#handler("sessions", [before, limit])(before, limit);
  }

  async revokeSession(id: string): Promise<void> {
    return this.#handler("revokeSession", [id])(id);
  }

  async users(before?: string, limit?: number): Promise<UserPage> {
    return this.#handler("users", [before, limit])(before, limit);
  }

  async patchUser(id: string, request: UserPatchRequest): Promise<UserResponse> {
    return this.#handler("patchUser", [id, request])(id, request);
  }

  async invites(before?: string, limit?: number): Promise<InvitePage> {
    return this.#handler("invites", [before, limit])(before, limit);
  }

  async createInvite(request: CreateInviteRequest): Promise<CreatedInviteResponse> {
    return this.#handler("createInvite", [request])(request);
  }

  async revokeInvite(id: string): Promise<void> {
    return this.#handler("revokeInvite", [id])(id);
  }

  async startLink(): Promise<LinkStartedResponse> {
    return this.#handler("startLink", [])();
  }

  async linkStatus(id: string): Promise<LinkFlowResponse> {
    return this.#handler("linkStatus", [id])(id);
  }

  async createOfflineAccount(
    request: CreateOfflineAccountRequest,
  ): Promise<CreatedOfflineAccountResponse> {
    return this.#handler("createOfflineAccount", [request])(request);
  }

  async accounts(before?: string, limit?: number): Promise<AccountPage> {
    return this.#handler("accounts", [before, limit])(before, limit);
  }

  async accountsAwaitingApproval(before?: string, limit?: number): Promise<AccountPage> {
    return this.#handler("accountsAwaitingApproval", [before, limit])(before, limit);
  }

  async account(id: string): Promise<AccountResponse> {
    return this.#handler("account", [id])(id);
  }

  async deleteAccount(id: string): Promise<void> {
    return this.#handler("deleteAccount", [id])(id);
  }

  async approveAccount(id: string): Promise<AccountResponse> {
    return this.#handler("approveAccount", [id])(id);
  }

  async refuseAccount(id: string): Promise<void> {
    return this.#handler("refuseAccount", [id])(id);
  }

  async relinkAccount(id: string): Promise<LinkStartedResponse> {
    return this.#handler("relinkAccount", [id])(id);
  }

  async accountGrants(id: string, before?: string, limit?: number): Promise<GrantPage> {
    return this.#handler("accountGrants", [id, before, limit])(id, before, limit);
  }

  async createGrant(id: string, request: CreateGrantRequest): Promise<GrantResponse> {
    return this.#handler("createGrant", [id, request])(id, request);
  }

  async changeGrant(id: string, user: string, request: ChangeGrantRequest): Promise<GrantResponse> {
    return this.#handler("changeGrant", [id, user, request])(id, user, request);
  }

  async revokeGrant(id: string, user: string): Promise<void> {
    return this.#handler("revokeGrant", [id, user])(id, user);
  }

  async openVerificationLink(): Promise<void> {
    return this.#handler("openVerificationLink", [])();
  }

  async watchEvents(onUpdate: (update: LiveUpdateMessage) => void): Promise<void> {
    return this.#handler("watchEvents", [onUpdate])(onUpdate);
  }
}

/** One watch a `fakeWatch` handler took: the test sends its updates and ends it. */
export interface FakeWatchCall {
  /** Sends `update` to the watch, as the Rust side would; nothing once it ended. */
  push(update: LiveUpdateMessage): void;
  /** Ends the watch as a stopped one: its call resolves. */
  end(): void;
  /** Ends the watch with `error`, as a session's end or a refused certificate does. */
  fail(error: ApiError): void;
  /** Whether it ended. */
  readonly ended: boolean;
}

/** The live events' fake: a `watchEvents` handler and every watch it took. */
export interface FakeWatch {
  /** The handler for `FakeApiClient`'s `watchEvents`. */
  readonly handler: ApiClient["watchEvents"];
  /** Every watch so far, in order. */
  readonly calls: readonly FakeWatchCall[];
  /** The newest watch; it throws when none started. */
  latest(): FakeWatchCall;
}

/**
 * A fake of the live events (R78): each `watchEvents` call is held until the test ends it,
 * and, as in the app, a newer watch stops the older one.
 */
export function fakeWatch(): FakeWatch {
  const calls: FakeWatchCall[] = [];
  return {
    handler: (onUpdate) =>
      new Promise<void>((resolve, reject) => {
        calls.at(-1)?.end();
        let ended = false;
        calls.push({
          push(update) {
            if (!ended) {
              onUpdate(update);
            }
          },
          end() {
            if (!ended) {
              ended = true;
              resolve();
            }
          },
          fail(error) {
            if (!ended) {
              ended = true;
              reject(error);
            }
          },
          get ended() {
            return ended;
          },
        });
      }),
    calls,
    latest() {
      const call = calls.at(-1);
      if (call === undefined) {
        throw new Error("no watch started");
      }
      return call;
    },
  };
}

/** What a test's error may carry besides its code. */
export interface FakeErrorDetails {
  readonly api?: ApiErrorDetails;
  readonly fields?: FieldError[];
}

/** An `ApiError` with `code`, for a handler to throw. */
export function fakeApiError(code: CommandErrorCode, details: FakeErrorDetails = {}): ApiError {
  return new ApiError({
    code,
    message: `fake ${code}`,
    ...(details.api === undefined ? {} : { api: details.api }),
    ...(details.fields === undefined ? {} : { fields: details.fields }),
  });
}
