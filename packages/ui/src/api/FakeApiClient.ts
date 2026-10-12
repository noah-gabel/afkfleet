// The API client for tests (Plan.md P8.5, ADR-0017): each test scripts only the methods
// it needs; any other method rejects with an `ApiError` that names it, so a test can't
// pass by accident. Every call is recorded with its arguments, for checking interactions.
import type { ApiErrorDetails } from "../generated/ApiErrorDetails";
import type { CommandErrorCode } from "../generated/CommandErrorCode";
import type { CreatedInviteResponse } from "../generated/CreatedInviteResponse";
import type { CreateInviteRequest } from "../generated/CreateInviteRequest";
import type { FieldError } from "../generated/FieldError";
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
