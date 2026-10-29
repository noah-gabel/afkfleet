// The app's API as the UI sees it (Plan.md P8.5, ADR-0017): one method per command of
// the desktop app's Rust side. The UI never knows the platform: `apps/desktop` injects a
// `TauriApiClient`, tests a `FakeApiClient`. No method takes or returns a token. Every
// method rejects with an `ApiError` (ApiError.ts).
import type { AccountPage } from "../generated/AccountPage";
import type { AccountResponse } from "../generated/AccountResponse";
import type { BotCommandResponse } from "../generated/BotCommandResponse";
import type { BotModeRequest } from "../generated/BotModeRequest";
import type { BotPage } from "../generated/BotPage";
import type { BotPatchRequest } from "../generated/BotPatchRequest";
import type { BotResponse } from "../generated/BotResponse";
import type { ChangeGrantRequest } from "../generated/ChangeGrantRequest";
import type { ChatCommandRequest } from "../generated/ChatCommandRequest";
import type { ChatPage } from "../generated/ChatPage";
import type { ChatSendRequest } from "../generated/ChatSendRequest";
import type { ChatSendResponse } from "../generated/ChatSendResponse";
import type { CreatedInviteResponse } from "../generated/CreatedInviteResponse";
import type { CreatedOfflineAccountResponse } from "../generated/CreatedOfflineAccountResponse";
import type { CreateGrantRequest } from "../generated/CreateGrantRequest";
import type { CreateInviteRequest } from "../generated/CreateInviteRequest";
import type { CreateOfflineAccountRequest } from "../generated/CreateOfflineAccountRequest";
import type { CreateServerSettingsRequest } from "../generated/CreateServerSettingsRequest";
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
import type { ModePage } from "../generated/ModePage";
import type { PasswordChangeRequest } from "../generated/PasswordChangeRequest";
import type { PreviewChatFormatsRequest } from "../generated/PreviewChatFormatsRequest";
import type { PreviewChatFormatsResponse } from "../generated/PreviewChatFormatsResponse";
import type { RegisterRequest } from "../generated/RegisterRequest";
import type { RegisterResponse } from "../generated/RegisterResponse";
import type { ServerProfileRequest } from "../generated/ServerProfileRequest";
import type { ServerProfileResponse } from "../generated/ServerProfileResponse";
import type { ServerProfileState } from "../generated/ServerProfileState";
import type { ServerSettingsPage } from "../generated/ServerSettingsPage";
import type { ServerSettingsResponse } from "../generated/ServerSettingsResponse";
import type { SessionPage } from "../generated/SessionPage";
import type { SetupStepResponse } from "../generated/SetupStepResponse";
import type { StepUpRequest } from "../generated/StepUpRequest";
import type { UpdateServerSettingsRequest } from "../generated/UpdateServerSettingsRequest";
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
  /** Starts linking a Microsoft account; its answer carries the user code, shown this once. */
  startLink(): Promise<LinkStartedResponse>;
  /** The state of the caller's link flow; it never carries the code. */
  linkStatus(id: string): Promise<LinkFlowResponse>;
  /** A new offline account, while the server allows them (test servers). */
  createOfflineAccount(
    request: CreateOfflineAccountRequest,
  ): Promise<CreatedOfflineAccountResponse>;
  /** A page of the accounts the caller may see. */
  accounts(before?: string, limit?: number): Promise<AccountPage>;
  /** A page of the pending links the caller may approve or refuse. */
  accountsAwaitingApproval(before?: string, limit?: number): Promise<AccountPage>;
  /** One account, as the caller sees it. */
  account(id: string): Promise<AccountResponse>;
  /** Deletes an account with its grants, after a step-up. */
  deleteAccount(id: string): Promise<void>;
  /** Approves a pending link, after a step-up. */
  approveAccount(id: string): Promise<AccountResponse>;
  /** Refuses a pending link, which deletes it, after a step-up. */
  refuseAccount(id: string): Promise<void>;
  /** Starts linking an account again; its answer carries the user code, shown this once. */
  relinkAccount(id: string): Promise<LinkStartedResponse>;
  /** A page of who else has access to an account (Manage). */
  accountGrants(id: string, before?: string, limit?: number): Promise<GrantPage>;
  /** Gives a user access by their exact username, after a step-up. */
  createGrant(id: string, request: CreateGrantRequest): Promise<GrantResponse>;
  /** Changes a grant's level, after a step-up. */
  changeGrant(id: string, user: string, request: ChangeGrantRequest): Promise<GrantResponse>;
  /** Revokes a grant, after a step-up. */
  revokeGrant(id: string, user: string): Promise<void>;
  /** Opens Microsoft's sign-in page: the link the newest start answered with, checked in Rust. */
  openVerificationLink(): Promise<void>;
  /**
   * Watches the session's live events (P11.8): every update goes to `onUpdate`, in order,
   * until the watch ends. It resolves once the watch was stopped: by a newer watch, a
   * server switch, or the app's channel refusing an update (S64). It rejects with
   * `no_server` without a server, `no_session` when the session ends or there's none, and
   * `certificate` when the server's certificate is refused. No update holds a token or a
   * ticket.
   */
  watchEvents(onUpdate: (update: LiveUpdateMessage) => void): Promise<void>;
  /** A page of the bots the caller may see, newest first (P11.9). */
  bots(before?: string, limit?: number): Promise<BotPage>;
  /** One bot, as the caller sees it. */
  bot(id: string): Promise<BotResponse>;
  /** A page of the modes the caller may see, newest first. */
  modes(before?: string, limit?: number): Promise<ModePage>;
  /**
   * Sets the bot's server (Manage, after a step-up, ADR-0019, S47): the bot as it reads
   * after. A 403 `step_up_required` asks for the password first.
   */
  patchBot(id: string, request: BotPatchRequest): Promise<BotResponse>;
  /** Sets the bot's mode (Control; Manage for a mode with `unlisted_command`, S68). */
  setBotMode(id: string, request: BotModeRequest): Promise<BotResponse>;
  /** The bot should run (Control): the bot, unplaced with why where it can't run yet. */
  startBot(id: string): Promise<BotResponse>;
  /** The bot should be stopped (Control); a paused or failed bot keeps its state (S51). */
  stopBot(id: string): Promise<BotResponse>;
  /** Restarts a running bot's run through its agent (Control): always `sent` (S73). */
  restartBot(id: string): Promise<BotCommandResponse>;
  /** Lets a failed bot try again: `sent` to its agent, or its stored state `cleared`. */
  resetBot(id: string): Promise<BotCommandResponse>;
  /** Lets a paused bot join again: `sent` to its agent, or its stored state `cleared`. */
  resumeBot(id: string): Promise<BotCommandResponse>;
  /** A page of the bot's chat, newest first, older than the line `before` (P11.3). */
  botChat(id: string, before?: number, limit?: number): Promise<ChatPage>;
  /**
   * Has the bot send a line (Control): plain chat or a command on the server's allowlist.
   * A manager's command off it is 403 `command_needs_step_up`, for `sendCommand` (S4). A
   * typed command may hold a password, so this is always a direct call (U34).
   */
  sendChat(id: string, request: ChatSendRequest): Promise<ChatSendResponse>;
  /** Has the bot send any command (Manage for one off the allowlist), after a step-up (S8). */
  sendCommand(id: string, request: ChatCommandRequest): Promise<ChatSendResponse>;
  /** A page of the servers' settings, newest first, at most 10 (Admins and the Owner). */
  serverSettings(before?: string, limit?: number): Promise<ServerSettingsPage>;
  /** A Minecraft server's new settings, after a step-up (Admins and the Owner, U54). */
  createServerSettings(request: CreateServerSettingsRequest): Promise<ServerSettingsResponse>;
  /**
   * The settings as they should be from now on, at the `version` read, after a step-up: 409
   * `server_settings_changed` when they changed since (S77).
   */
  updateServerSettings(
    id: string,
    request: UpdateServerSettingsRequest,
  ): Promise<ServerSettingsResponse>;
  /** Deletes a server's settings, after a step-up. */
  deleteServerSettings(id: string): Promise<void>;
  /**
   * Which sender unsaved chat formats read from a line, and which format fitted first, counted
   * from 0; nothing is stored, and no step-up is needed (Admins and the Owner, U58, R134).
   */
  previewChatFormats(request: PreviewChatFormatsRequest): Promise<PreviewChatFormatsResponse>;
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
  "startLink",
  "linkStatus",
  "createOfflineAccount",
  "accounts",
  "accountsAwaitingApproval",
  "account",
  "deleteAccount",
  "approveAccount",
  "refuseAccount",
  "relinkAccount",
  "accountGrants",
  "createGrant",
  "changeGrant",
  "revokeGrant",
  "openVerificationLink",
  "watchEvents",
  "bots",
  "bot",
  "modes",
  "patchBot",
  "setBotMode",
  "startBot",
  "stopBot",
  "restartBot",
  "resetBot",
  "resumeBot",
  "botChat",
  "sendChat",
  "sendCommand",
  "serverSettings",
  "createServerSettings",
  "updateServerSettings",
  "deleteServerSettings",
  "previewChatFormats",
] as const satisfies readonly ApiMethod[];
