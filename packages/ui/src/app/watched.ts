// The one place every API call's outcome is seen (Plan.md P8.6, ADR-0017): queries,
// mutations and AuthFlow's direct calls all go through `watched(client)`, which App puts
// behind the input checks. Each call records, when it starts, the session's generation and
// the banner's epoch (stores.ts), then:
// - `unreachable` and `timed_out` put the banner up, `certificate` its certificate state;
// - `no_session` ends the session, but only through `endIfSignedIn`, so once per end, never
//   at launch, inside AuthFlow or during a logout, and never for a call from before a
//   re-login;
// - a success takes the banner down if the call started after it went up, which then does
//   what a successful check does (`recovered`);
// - the live events' watch: only its `no_session` counts here, as any call's; whether its
//   updates and its other ends raise the banner is the live layer's to say, since only it
//   knows which watch is current (S62, R88), and its end takes nothing down (S59, R84).
// The error is rethrown as it came, as an `ApiError`.
import type { ApiClient } from "../api/ApiClient";
import { type ApiError, toApiError } from "../api/ApiError";
import type { CommandErrorCode } from "../generated/CommandErrorCode";
import type { AppStores } from "./stores";

/** What the runtime does when the watched client sees these. */
export interface WatchEvents {
  /** The session ended during use (once per end). */
  sessionEnded(): void;
  /** The banner went down: refetch what failed. */
  recovered(): void;
}

/** `client`, with every call's outcome handled; see the module comment. */
export function watched(client: ApiClient, stores: AppStores, events: WatchEvents): ApiClient {
  /** `call`, watched. */
  function watch<A extends unknown[], R>(call: (...args: A) => Promise<R>) {
    return async (...args: A): Promise<R> => {
      const generation = stores.session.getState().generation;
      const epoch = stores.connection.getState().epoch;
      let value: R;
      try {
        value = await call(...args);
      } catch (rejection) {
        const error = toApiError(rejection);
        failed(error, generation);
        throw error;
      }
      if (stores.connection.getState().recover(epoch)) {
        events.recovered();
      }
      return value;
    };
  }

  /**
   * The live events' watch (S59, R84, R88): its updates go on as they came, and of its ends
   * only `no_session` counts here; the live layer raises the banner for the current watch's.
   * Its end never takes the banner down, since a stopped watch says nothing about the server.
   */
  async function watchLive(onUpdate: Parameters<ApiClient["watchEvents"]>[0]): Promise<void> {
    const generation = stores.session.getState().generation;
    try {
      await client.watchEvents(onUpdate);
    } catch (rejection) {
      const error = toApiError(rejection);
      if (error.error.code === "no_session") {
        failed(error, generation);
      }
      throw error;
    }
  }

  /** What a failure of a call that started in `generation` changes. */
  function failed(error: ApiError, generation: number): void {
    switch (error.error.code) {
      case "unreachable":
      case "timed_out":
      case "certificate":
        connectionFailed(stores, error.error.code);
        return;
      case "no_session":
        if (stores.session.getState().endIfSignedIn(generation)) {
          events.sessionEnded();
        }
        return;
      default:
        // Every other failure is the caller's to show.
        return;
    }
  }

  return {
    serverProfile: watch(() => client.serverProfile()),
    setServerProfile: watch((request) => client.setServerProfile(request)),
    checkServer: watch(() => client.checkServer()),
    login: watch((request) => client.login(request)),
    loginMfa: watch((code) => client.loginMfa(code)),
    logout: watch(() => client.logout()),
    register: watch((request) => client.register(request)),
    stepUp: watch((request) => client.stepUp(request)),
    mfaEnroll: watch(() => client.mfaEnroll()),
    mfaConfirm: watch((code) => client.mfaConfirm(code)),
    mfaReset: watch(() => client.mfaReset()),
    me: watch(() => client.me()),
    changePassword: watch((request) => client.changePassword(request)),
    sessions: watch((before, limit) => client.sessions(before, limit)),
    revokeSession: watch((id) => client.revokeSession(id)),
    users: watch((before, limit) => client.users(before, limit)),
    patchUser: watch((id, request) => client.patchUser(id, request)),
    invites: watch((before, limit) => client.invites(before, limit)),
    createInvite: watch((request) => client.createInvite(request)),
    revokeInvite: watch((id) => client.revokeInvite(id)),
    startLink: watch(() => client.startLink()),
    linkStatus: watch((id) => client.linkStatus(id)),
    createOfflineAccount: watch((request) => client.createOfflineAccount(request)),
    accounts: watch((before, limit) => client.accounts(before, limit)),
    accountsAwaitingApproval: watch((before, limit) =>
      client.accountsAwaitingApproval(before, limit),
    ),
    account: watch((id) => client.account(id)),
    deleteAccount: watch((id) => client.deleteAccount(id)),
    approveAccount: watch((id) => client.approveAccount(id)),
    refuseAccount: watch((id) => client.refuseAccount(id)),
    relinkAccount: watch((id) => client.relinkAccount(id)),
    accountGrants: watch((id, before, limit) => client.accountGrants(id, before, limit)),
    createGrant: watch((id, request) => client.createGrant(id, request)),
    changeGrant: watch((id, user, request) => client.changeGrant(id, user, request)),
    revokeGrant: watch((id, user) => client.revokeGrant(id, user)),
    openVerificationLink: watch(() => client.openVerificationLink()),
    watchEvents: watchLive,
    bots: watch((before, limit) => client.bots(before, limit)),
    bot: watch((id) => client.bot(id)),
    modes: watch((before, limit) => client.modes(before, limit)),
    createMode: watch((request) => client.createMode(request)),
    createModeWithCommands: watch((request) => client.createModeWithCommands(request)),
    updateMode: watch((id, request) => client.updateMode(id, request)),
    updateModeWithCommands: watch((id, request) => client.updateModeWithCommands(id, request)),
    deleteMode: watch((id) => client.deleteMode(id)),
    patchBot: watch((id, request) => client.patchBot(id, request)),
    setBotMode: watch((id, request) => client.setBotMode(id, request)),
    startBot: watch((id) => client.startBot(id)),
    stopBot: watch((id) => client.stopBot(id)),
    restartBot: watch((id) => client.restartBot(id)),
    resetBot: watch((id) => client.resetBot(id)),
    resumeBot: watch((id) => client.resumeBot(id)),
    botChat: watch((id, before, limit) => client.botChat(id, before, limit)),
    sendChat: watch((id, request) => client.sendChat(id, request)),
    sendCommand: watch((id, request) => client.sendCommand(id, request)),
    serverSettings: watch((before, limit) => client.serverSettings(before, limit)),
    createServerSettings: watch((request) => client.createServerSettings(request)),
    updateServerSettings: watch((id, request) => client.updateServerSettings(id, request)),
    deleteServerSettings: watch((id) => client.deleteServerSettings(id)),
    previewChatFormats: watch((request) => client.previewChatFormats(request)),
  };
}

/**
 * What a failure with `code` changes for the banner: `unreachable` and `timed_out` put it up,
 * `certificate` its certificate state; nothing for any other code.
 */
export function connectionFailed(stores: AppStores, code: CommandErrorCode): void {
  switch (code) {
    case "unreachable":
    case "timed_out":
      stores.connection.getState().fail("down");
      return;
    case "certificate":
      stores.connection.getState().fail("certificate");
      return;
    default:
      return;
  }
}
