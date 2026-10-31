// The API client of the UI's own web entry (src/main.tsx): it has no server connection, so
// every call rejects with `ipc`. No test code ships in that bundle; the e2e build lives in
// apps/desktop/e2e (P8.9).
import type { ApiClient } from "./ApiClient";
import { ApiError } from "./ApiError";

/** Why every call fails in the web build. */
export const WEB_BUILD_MESSAGE = "The web build has no server connection.";

/** Every call's answer. */
function unavailable(): Promise<never> {
  return Promise.reject(new ApiError({ code: "ipc", message: WEB_BUILD_MESSAGE }));
}

/** An API client without a server; see the module comment. */
export class UnavailableApiClient implements ApiClient {
  serverProfile = unavailable;
  setServerProfile = unavailable;
  checkServer = unavailable;
  login = unavailable;
  loginMfa = unavailable;
  logout = unavailable;
  register = unavailable;
  stepUp = unavailable;
  mfaEnroll = unavailable;
  mfaConfirm = unavailable;
  mfaReset = unavailable;
  me = unavailable;
  changePassword = unavailable;
  sessions = unavailable;
  revokeSession = unavailable;
  users = unavailable;
  patchUser = unavailable;
  invites = unavailable;
  createInvite = unavailable;
  revokeInvite = unavailable;
  startLink = unavailable;
  linkStatus = unavailable;
  createOfflineAccount = unavailable;
  accounts = unavailable;
  bots = unavailable;
  bot = unavailable;
  modes = unavailable;
  createMode = unavailable;
  createModeWithCommands = unavailable;
  updateMode = unavailable;
  updateModeWithCommands = unavailable;
  deleteMode = unavailable;
  patchBot = unavailable;
  setBotMode = unavailable;
  startBot = unavailable;
  stopBot = unavailable;
  restartBot = unavailable;
  resetBot = unavailable;
  resumeBot = unavailable;
  botChat = unavailable;
  sendChat = unavailable;
  sendCommand = unavailable;
  serverSettings = unavailable;
  createServerSettings = unavailable;
  updateServerSettings = unavailable;
  deleteServerSettings = unavailable;
  previewChatFormats = unavailable;
  audit = unavailable;
  agents = unavailable;
  disableAgent = unavailable;
  releaseAgent = unavailable;
  userGrants = unavailable;
  sharedAccounts = unavailable;
  leaveAccount = unavailable;
  accountsAwaitingApproval = unavailable;
  account = unavailable;
  deleteAccount = unavailable;
  approveAccount = unavailable;
  refuseAccount = unavailable;
  relinkAccount = unavailable;
  accountGrants = unavailable;
  createGrant = unavailable;
  changeGrant = unavailable;
  revokeGrant = unavailable;
  openVerificationLink = unavailable;
  watchEvents = unavailable;
}
