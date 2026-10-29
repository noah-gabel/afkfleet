// The e2e suite's fake backend (Plan.md P8.9, ADR-0017): the desktop app's Rust side and the
// server in one, answering the 53 commands of src-tauri/tests/ipc-contract.json with the
// generated types, the Rust side's fixed `CommandError` messages and the server's error
// codes. It keeps the state one test needs, fresh per test: users with their passwords and
// authenticators, invites, the one session the app holds, the pending login or setup step,
// the session's step-up, the Microsoft accounts with their grants and the one link flow, and
// each account's bot, as the server makes one with every account, and the modes: the two
// built-in ones and another user's shared `warp`, which sends a command off the allowlist.
// The bot writes follow the server's simplest answers (ADR-0019, S50; ADR-0020, S73), and so
// does the chat: each bot's lines, a line sent only to an online bot, a command off the
// allowlist only through `/command` after a step-up (S4, S8), answered `sent` or, by
// `chatOutcome`, `pending` (S5); `addChat` keeps a spec's pushed line in REST too.
// The servers' settings are an Admin's or the Owner's, each write after the step-up, as the
// server keeps them (ADR-0020, S74, S77, S78): a server keyed lowercase with its port always
// written (25565 when none is given), its texts sorted and counted once, answered as their
// lines; a stale version is 409 `server_settings_changed`, a server that has settings 422
// at `server`, the hundred and first 409 `conflict`, and an ID it doesn't hold 404.
// A flow reads `waiting` once, then `linked`, its account pending unless the Owner linked
// it.
//
// A live events watch (`watch_events`) is held open until a newer one starts (`Ok`) or the
// session ends (`no_session`), as the Rust side holds it; a spec pushes updates into the page
// through the e2e build's hook, after changing the fake (`setBot`) so REST answers the same.
//
// An authenticator here has one valid code at a time (`code(username)`), which a successful
// use replaces, like a TOTP code that can't be used twice. A pending setup expects
// `setupCode()`. Everything is a placeholder (CLAUDE.md).
import type { AccountKind } from "@afkfleet/ui/generated/AccountKind";
import type { AccountPage } from "@afkfleet/ui/generated/AccountPage";
import type { AccountResponse } from "@afkfleet/ui/generated/AccountResponse";
import type { AccountStatus } from "@afkfleet/ui/generated/AccountStatus";
import type { ApiErrorDetails } from "@afkfleet/ui/generated/ApiErrorDetails";
import type { BotCommandResponse } from "@afkfleet/ui/generated/BotCommandResponse";
import type { BotPage } from "@afkfleet/ui/generated/BotPage";
import type { BotResponse } from "@afkfleet/ui/generated/BotResponse";
import type { BotStatus } from "@afkfleet/ui/generated/BotStatus";
import type { ChatLine } from "@afkfleet/ui/generated/ChatLine";
import type { ChatPage } from "@afkfleet/ui/generated/ChatPage";
import type { ChatSendOutcome } from "@afkfleet/ui/generated/ChatSendOutcome";
import type { ChatSendResponse } from "@afkfleet/ui/generated/ChatSendResponse";
import type { CommandError } from "@afkfleet/ui/generated/CommandError";
import type { CommandErrorCode } from "@afkfleet/ui/generated/CommandErrorCode";
import type { CreatedInviteResponse } from "@afkfleet/ui/generated/CreatedInviteResponse";
import type { CreatedOfflineAccountResponse } from "@afkfleet/ui/generated/CreatedOfflineAccountResponse";
import type { ErrorCode } from "@afkfleet/ui/generated/ErrorCode";
import type { GrantLevel } from "@afkfleet/ui/generated/GrantLevel";
import type { GrantPage } from "@afkfleet/ui/generated/GrantPage";
import type { GrantResponse } from "@afkfleet/ui/generated/GrantResponse";
import type { InvitePage } from "@afkfleet/ui/generated/InvitePage";
import type { InviteResponse } from "@afkfleet/ui/generated/InviteResponse";
import type { LinkFlowResponse } from "@afkfleet/ui/generated/LinkFlowResponse";
import type { LinkStartedResponse } from "@afkfleet/ui/generated/LinkStartedResponse";
import type { LoginStepResponse } from "@afkfleet/ui/generated/LoginStepResponse";
import type { LogoutResponse } from "@afkfleet/ui/generated/LogoutResponse";
import type { MeResponse } from "@afkfleet/ui/generated/MeResponse";
import type { MfaConfirmResponse } from "@afkfleet/ui/generated/MfaConfirmResponse";
import type { MfaEnrollResponse } from "@afkfleet/ui/generated/MfaEnrollResponse";
import type { ModePage } from "@afkfleet/ui/generated/ModePage";
import type { ModeResponse } from "@afkfleet/ui/generated/ModeResponse";
import type { RegisterResponse } from "@afkfleet/ui/generated/RegisterResponse";
import type { ServerProfileState } from "@afkfleet/ui/generated/ServerProfileState";
import type { ServerSettingsPage } from "@afkfleet/ui/generated/ServerSettingsPage";
import type { ServerSettingsResponse } from "@afkfleet/ui/generated/ServerSettingsResponse";
import type { SessionPage } from "@afkfleet/ui/generated/SessionPage";
import type { SetupStepResponse } from "@afkfleet/ui/generated/SetupStepResponse";
import type { UserPage } from "@afkfleet/ui/generated/UserPage";
import type { UserResponse } from "@afkfleet/ui/generated/UserResponse";
import type { UserRole } from "@afkfleet/ui/generated/UserRole";

/** The Owner's username and every user's password. */
export const OWNER = "afkowner";
export const PASSWORD = "placeholder password";
/** A user who registered and never set up 2FA. */
export const NEW_USER = "afkbot1";
/** A Member with 2FA. */
export const MEMBER = "afkmember";
/** Every link flow's user code. */
export const LINK_CODE = "B7X4-KDLQ";
/** The server's 422 for a name that can't be given a grant (U4 in ADR-0018). */
const UNGRANTABLE =
  "can't be given a grant: unknown, disabled, or already has access; check the name";
/** A Member invite's code that registration accepts. */
export const INVITE_CODE = "afk_iv_placeholderplaceholderplaceholder01";
/** The server the app is set up for. */
export const SERVER = "https://example.com/";

/** Every command the fake answers: ipc-contract.json's, checked by unit/fake.spec.ts. */
export const COMMANDS = [
  "server_profile",
  "set_server_profile",
  "check_server",
  "login",
  "login_mfa",
  "logout",
  "register",
  "step_up",
  "mfa_enroll",
  "mfa_confirm",
  "mfa_reset",
  "me",
  "change_password",
  "sessions",
  "revoke_session",
  "users",
  "patch_user",
  "invites",
  "create_invite",
  "revoke_invite",
  "start_link",
  "link_status",
  "create_offline_account",
  "accounts",
  "accounts_awaiting_approval",
  "account",
  "delete_account",
  "approve_account",
  "refuse_account",
  "relink_account",
  "account_grants",
  "create_grant",
  "change_grant",
  "revoke_grant",
  "open_verification_link",
  "watch_events",
  "bots",
  "bot",
  "modes",
  "patch_bot",
  "set_bot_mode",
  "start_bot",
  "stop_bot",
  "restart_bot",
  "reset_bot",
  "resume_bot",
  "bot_chat",
  "send_chat",
  "send_command",
  "server_settings",
  "create_server_settings",
  "update_server_settings",
  "delete_server_settings",
] as const;

/** One command. */
export type Command = (typeof COMMANDS)[number];

/** What a command does: its value, or a refusal. */
export type Outcome =
  | { readonly ok: true; readonly value?: unknown }
  | { readonly ok: false; readonly error: CommandError };

/** The Rust side's fixed messages (fleet-api-types' `app`). */
const MESSAGES: Partial<Record<CommandErrorCode, string>> = {
  api: "The server refused the request.",
  no_session: "You're not logged in.",
  no_pending_step: "No login or 2FA step is pending. Start again.",
  already_logged_in: "You're already logged in. Log out first.",
  no_verification_link: "There's no Microsoft page to open.",
};

/** The server's messages (fleet-api-types' `ErrorCode::message`). */
const API_MESSAGES: Partial<Record<ErrorCode, string>> = {
  invalid_credentials: "The username, password or code is wrong.",
  forbidden: "You don't have permission to do this.",
  step_up_required: "Enter your password again to do this.",
  not_found: "The resource doesn't exist.",
  conflict: "The request conflicts with the current state.",
  validation_failed: "Some fields are invalid.",
  bot_not_online: "Only an online bot can send chat.",
  command_needs_step_up: "This command needs your password: confirm it's you to send it.",
  server_settings_changed:
    "These settings changed since you opened them. Load them again to see the change.",
};

/** A refusal of the Rust side. */
function refused(code: CommandErrorCode): Outcome {
  return { ok: false, error: { code, message: MESSAGES[code] ?? code } };
}

/** A refusal of the server. */
function api(status: number, code: ErrorCode, fields?: ApiErrorDetails["fields"]): Outcome {
  const details: ApiErrorDetails = {
    status,
    code,
    message: API_MESSAGES[code] ?? code,
    request_id: "01900000-0000-7000-8000-0000000000ee",
    ...(fields === undefined ? {} : { fields }),
  };
  return { ok: false, error: { code: "api", message: MESSAGES.api ?? "", api: details } };
}

/** An answer. */
function value(answer?: unknown): Outcome {
  return { ok: true, value: answer };
}

/** A version 7 ID's text, numbered `n`. */
function id(n: number): string {
  return `01900000-0000-7000-8000-${String(n).padStart(12, "0")}`;
}

/** A user the fake keeps. */
interface User {
  readonly id: string;
  readonly username: string;
  role: UserRole;
  password: string;
  disabled: boolean;
  readonly created_at: string;
  /** The authenticator's one valid code, or null before 2FA is set up. */
  code: string | null;
  recovery: string[];
  mfaConfirmedAt?: string;
}

/** An invite the fake keeps, with its code. */
interface Invite {
  readonly response: InviteResponse;
  readonly code: string;
}

/** The one session the app holds. */
interface Session {
  readonly id: string;
  readonly user: User;
  steppedUp: boolean;
}

/** A Microsoft or offline account the fake keeps, with its grants and its bot. */
interface Account {
  readonly id: string;
  name: string;
  readonly kind: AccountKind;
  status: AccountStatus;
  readonly ownerId: string;
  readonly linkedAt: string;
  readonly grants: Map<string, { level: GrantLevel; readonly by: string }>;
  readonly bot: Bot;
}

/** An account's bot: what the server keeps and reports of it. */
interface Bot {
  readonly id: string;
  modeId: string;
  serverAddress?: string;
  desiredState: BotResponse["desired_state"];
  status: BotStatus;
  /** Its chat, oldest first, as the server keeps it: the whole line, every field. */
  readonly chat: ChatLine[];
}

/** The modes, newest first: another user's shared `warp`, then the built-in ones with the IDs
 * the server's seeds give them. */
const MODES: readonly ModeResponse[] = [
  {
    ...builtIn("01a14202-2800-7000-8000-000000000003", "warp"),
    ownership: "someone_else",
    unlisted_command: true,
  },
  builtIn("01a14202-2800-7000-8000-000000000002", "farm"),
  builtIn("01a14202-2800-7000-8000-000000000001", "afk"),
];

/** The server's `[chat] command_allowlist`: commands a Control holder may send. */
const ALLOWLIST: readonly string[] = ["/spawn"];

/** The mode a new bot runs: `afk`. */
const AFK = "01a14202-2800-7000-8000-000000000001";

/** A built-in mode: no owner, read-only for everyone. */
function builtIn(id: string, name: string): ModeResponse {
  return {
    id,
    name,
    visibility: "shared",
    ownership: "built_in",
    editable: false,
    deletable: false,
    visibility_editable: false,
    unlisted_command: false,
    version: 1,
    created_at: "2026-10-13T08:00:00.000Z",
    updated_at: "2026-10-13T08:00:00.000Z",
    steps: [],
  };
}

/** The one link flow: its user, the account it links again, and its reads. */
interface Flow {
  readonly id: string;
  readonly userId: string;
  readonly again: string | null;
  reads: number;
  linked?: Account;
}

/** A server's settings the fake keeps. */
interface Settings {
  readonly id: string;
  server: string;
  texts: string[];
  /** The chat formats as written, in order, duplicates kept (ADR-0020, R139). */
  formats: string[];
  version: number;
}

/** The key the server answers for `server`: lowercase, the port always written. */
function keyOf(server: string): string {
  const lower = server.toLowerCase();
  return lower.includes(":") ? lower : `${lower}:25565`;
}

/** A request's texts as the server keeps them: sorted, each once. */
function textsOf(texts: unknown): string[] {
  return [...new Set(Array.isArray(texts) ? texts.map(String) : [])].sort();
}

/** A request's chat formats as the server keeps them: as written, in order. */
function formatsOf(formats: unknown): string[] {
  return Array.isArray(formats) ? formats.map(String) : [];
}

/** The cross-field rule's refusal: a row holds a text unless it holds a format (R140). */
function holdsNothing(): Outcome {
  return api(422, "validation_failed", [
    {
      path: "conflict_texts",
      message: "must hold at least one text, unless a chat format is set",
    },
  ]);
}

/** `settings` as the server answers them: each text as its lines. */
function settingsResponse(settings: Settings): ServerSettingsResponse {
  return {
    id: settings.id,
    server: settings.server,
    conflict_texts: settings.texts.map((text) => ({ lines: text.split("\n") })),
    chat_formats: [...settings.formats],
    version: settings.version,
    created_at: "2026-10-28T08:00:00.000Z",
    updated_at: "2026-10-28T08:00:00.000Z",
  };
}

/** A pending login or setup step. */
type Pending =
  | { readonly kind: "mfa"; readonly user: User; tries: number }
  | { readonly kind: "setup"; readonly user: User; readonly replace: boolean; enrolled: boolean };

/** The fake backend of one test; see the module comment. */
export class FakeBackend {
  readonly #users: User[] = [];
  readonly #invites: Invite[] = [];
  #session: Session | null = null;
  /** How the live events watch the app holds open ends: settled by a newer one or a logout. */
  #watch: ((outcome: Outcome) => void) | null = null;
  #pending: Pending | null = null;
  readonly #accounts: Account[] = [];
  /** The servers' settings, newest first. */
  #settings: Settings[] = [];
  #flow: Flow | null = null;
  #opened = 0;
  #accountNames = 0;
  #ids = 100;
  #codes = 0;
  #lines = 0;
  /** What a line sent to an online bot answers: `sent` (200), or `pending` (202). */
  chatOutcome: ChatSendOutcome = "sent";
  /** The last line sent, with its request, for a spec to push its `chat` and `chat_sent`. */
  lastSend: { readonly request: string; readonly line: ChatLine } | null = null;

  constructor() {
    this.#addUser(OWNER, "owner", true);
    this.#addUser(NEW_USER, "member", false);
    this.#addUser(MEMBER, "member", true);
    this.#invites.push({
      code: INVITE_CODE,
      response: {
        id: id(this.#next()),
        role: "member",
        state: "pending",
        created_by: { id: this.#user(OWNER).id, username: OWNER },
        created_at: "2026-10-13T08:00:00.000Z",
        expires_at: "2026-10-16T08:00:00.000Z",
      },
    });
  }

  #next(): number {
    this.#ids += 1;
    return this.#ids;
  }

  /** A new 6-digit code, never the same twice. */
  #newCode(): string {
    this.#codes += 1;
    return String(100_000 + this.#codes);
  }

  #addUser(username: string, role: UserRole, withMfa: boolean): User {
    const user: User = {
      id: id(this.#next()),
      username,
      role,
      password: PASSWORD,
      disabled: false,
      created_at: "2026-10-13T08:00:00.000Z",
      code: withMfa ? this.#newCode() : null,
      recovery: [],
      ...(withMfa ? { mfaConfirmedAt: "2026-10-13T08:05:00.000Z" } : {}),
    };
    this.#users.push(user);
    return user;
  }

  #user(username: string): User {
    const user = this.#users.find((one) => one.username.toLowerCase() === username.toLowerCase());
    if (user === undefined) {
      throw new Error(`the fake has no user ${username}`);
    }
    return user;
  }

  /** `username`'s authenticator's current code. */
  code(username: string): string {
    const code = this.#user(username).code;
    if (code === null) {
      throw new Error(`${username} has no authenticator`);
    }
    return code;
  }

  /** The code that confirms a pending setup. */
  setupCode(): string {
    return "424242";
  }

  /** Whether `username` exists with 2FA set up. */
  hasMfa(username: string): boolean {
    return this.#users.some((user) => user.username === username && user.code !== null);
  }

  /** The newest invite's code. */
  newestInviteCode(): string | undefined {
    return this.#invites.at(-1)?.code;
  }

  /**
   * Runs `command` with `args`, as the app's Rust side and the server would: at once, but a
   * live events watch, which answers when it ends.
   */
  handle(command: string, args: Record<string, unknown>): Outcome | Promise<Outcome> {
    const request = (args.request ?? {}) as Record<string, unknown>;
    switch (command as Command) {
      case "server_profile":
        return value({ state: "set", profile: { url: SERVER } } satisfies ServerProfileState);
      case "set_server_profile":
        return refused("invalid_server_url");
      case "check_server":
        return value();
      case "login":
        return this.#login(String(request.username), String(request.password));
      case "login_mfa":
        return this.#loginMfa(String(args.code));
      case "logout":
        this.#endWatch(refused("no_session"));
        this.#session = null;
        return value({ server: "ended" } satisfies LogoutResponse);
      case "register":
        return this.#register(
          String(request.invite),
          String(request.username),
          String(request.password),
        );
      case "step_up":
        return this.#stepUp(String(request.password));
      case "mfa_enroll":
        return this.#enroll();
      case "mfa_confirm":
        return this.#confirm(String(args.code));
      case "mfa_reset":
        return this.#reset();
      case "me":
        return this.#signedIn((session) => value(this.#me(session.user)));
      case "change_password":
        return this.#steppedUp((session) => {
          session.user.password = String(request.new_password);
          session.steppedUp = false;
          return value();
        });
      case "sessions":
        return this.#signedIn((session) =>
          value({
            items: [
              {
                id: session.id,
                created_at: "2026-10-13T09:00:00.000Z",
                last_used_at: "2026-10-13T09:00:00.000Z",
                expires_at: "2026-10-20T09:00:00.000Z",
                current: true,
              },
            ],
          } satisfies SessionPage),
        );
      case "revoke_session":
        return api(409, "conflict");
      case "users":
        return this.#admin(() =>
          value({
            items: [...this.#users].reverse().map((user) => this.#userResponse(user)),
          } satisfies UserPage),
        );
      case "patch_user":
        return this.#patch(String(args.id), request);
      case "invites":
        return this.#admin(() =>
          value({
            items: [...this.#invites].reverse().map((one) => one.response),
          } satisfies InvitePage),
        );
      case "create_invite":
        return this.#createInvite(String(request.role));
      case "revoke_invite":
        return this.#revokeInvite(String(args.id));
      case "watch_events":
        return this.#startWatch();
      case "server_settings":
        return this.#admin(() =>
          value({ items: this.#settings.map(settingsResponse) } satisfies ServerSettingsPage),
        );
      case "create_server_settings":
        return this.#steppedUp(() => this.#admin(() => this.#createSettings(request)));
      case "update_server_settings":
        return this.#steppedUp(() =>
          this.#admin(() => this.#updateSettings(String(args.id), request)),
        );
      case "delete_server_settings":
        return this.#steppedUp(() =>
          this.#admin(() => {
            const kept = this.#settings.filter((one) => one.id !== String(args.id));
            if (kept.length === this.#settings.length) {
              return api(404, "not_found");
            }
            this.#settings = kept;
            return value();
          }),
        );
      default:
        return this.#accountCommand(command as Command, args);
    }
  }

  /** A new watch: held until a newer one starts or the session ends, which stops the older. */
  #startWatch(): Outcome | Promise<Outcome> {
    if (this.#session === null) {
      return refused("no_session");
    }
    this.#endWatch(value());
    return new Promise((resolve) => {
      this.#watch = resolve;
    });
  }

  /** Ends the held watch, if there is one, with `outcome`. */
  #endWatch(outcome: Outcome): void {
    this.#watch?.(outcome);
    this.#watch = null;
  }

  #signedIn(then: (session: Session) => Outcome): Outcome {
    return this.#session === null ? refused("no_session") : then(this.#session);
  }

  #steppedUp(then: (session: Session) => Outcome): Outcome {
    return this.#signedIn((session) =>
      session.steppedUp ? then(session) : api(403, "step_up_required"),
    );
  }

  #admin(then: (session: Session) => Outcome): Outcome {
    return this.#signedIn((session) =>
      session.user.role === "member" ? api(403, "forbidden") : then(session),
    );
  }

  #login(username: string, password: string): Outcome {
    if (this.#session !== null) {
      return refused("already_logged_in");
    }
    const user = this.#users.find((one) => one.username.toLowerCase() === username.toLowerCase());
    if (user === undefined || user.password !== password || user.disabled) {
      return api(401, "invalid_credentials");
    }
    if (user.code === null) {
      this.#pending = { kind: "setup", user, replace: false, enrolled: false };
      return value({ step: "setup_required", expires_in: 600 } satisfies LoginStepResponse);
    }
    this.#pending = { kind: "mfa", user, tries: 3 };
    return value({ step: "mfa_required", expires_in: 300 } satisfies LoginStepResponse);
  }

  #loginMfa(code: string): Outcome {
    const pending = this.#pending;
    if (pending?.kind !== "mfa") {
      return refused("no_pending_step");
    }
    const user = pending.user;
    const recovery = user.recovery.indexOf(code);
    if (code === user.code || recovery >= 0) {
      if (recovery >= 0) {
        user.recovery.splice(recovery, 1);
      } else {
        user.code = this.#newCode();
      }
      this.#pending = null;
      this.#session = { id: id(this.#next()), user, steppedUp: false };
      return value();
    }
    pending.tries -= 1;
    if (pending.tries === 0) {
      this.#pending = null;
    }
    return api(401, "invalid_credentials");
  }

  #register(invite: string, username: string, password: string): Outcome {
    const found = this.#invites.find(
      (one) => one.code === invite && one.response.state === "pending",
    );
    if (found === undefined) {
      return api(422, "validation_failed", [{ path: "invite", message: "can't be used" }]);
    }
    if (this.#users.some((user) => user.username.toLowerCase() === username.toLowerCase())) {
      return api(422, "validation_failed", [{ path: "username", message: "is taken" }]);
    }
    const user = this.#addUser(username, found.response.role, false);
    user.password = password;
    const index = this.#invites.indexOf(found);
    this.#invites[index] = {
      code: found.code,
      response: {
        ...found.response,
        state: "used",
        used_by: { id: user.id, username },
        used_at: "2026-10-13T10:00:00.000Z",
      },
    };
    return value({ id: user.id, username, role: user.role } satisfies RegisterResponse);
  }

  #stepUp(password: string): Outcome {
    return this.#signedIn((session) => {
      if (password !== session.user.password) {
        return api(403, "forbidden");
      }
      session.steppedUp = true;
      return value();
    });
  }

  #enroll(): Outcome {
    const pending = this.#pending;
    if (pending?.kind !== "setup") {
      return refused("no_pending_step");
    }
    pending.enrolled = true;
    const secret = "JBSWY3DPEHPK3PXP";
    return value({
      otpauth_uri: `otpauth://totp/afkfleet:${pending.user.username}?secret=${secret}&issuer=afkfleet`,
      qr_svg: `data:image/svg+xml;base64,${btoa('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8"/></svg>')}`,
      secret,
    } satisfies MfaEnrollResponse);
  }

  #confirm(code: string): Outcome {
    const pending = this.#pending;
    if (pending?.kind !== "setup" || !pending.enrolled) {
      return refused("no_pending_step");
    }
    if (code !== this.setupCode()) {
      return api(422, "validation_failed", [{ path: "code", message: "doesn't match" }]);
    }
    const user = pending.user;
    user.code = this.#newCode();
    user.mfaConfirmedAt = "2026-10-13T10:05:00.000Z";
    user.recovery = Array.from({ length: 10 }, (_, n) => `rcvr-${user.username}-${n}`);
    this.#pending = null;
    return value({ recovery_codes: [...user.recovery] } satisfies MfaConfirmResponse);
  }

  #reset(): Outcome {
    return this.#steppedUp((session) => {
      this.#pending = { kind: "setup", user: session.user, replace: true, enrolled: false };
      return value({ expires_in: 600 } satisfies SetupStepResponse);
    });
  }

  #me(user: User): MeResponse {
    return {
      id: user.id,
      username: user.username,
      role: user.role,
      created_at: user.created_at,
      password_changed_at: user.created_at,
      ...(user.mfaConfirmedAt === undefined ? {} : { mfa_confirmed_at: user.mfaConfirmedAt }),
      recovery_codes_left: user.recovery.length,
      failed_code_attempts: { count: 0 },
    };
  }

  #userResponse(user: User): UserResponse {
    return {
      id: user.id,
      username: user.username,
      role: user.role,
      disabled: user.disabled,
      created_at: user.created_at,
      mfa_enrolled: user.code !== null,
    };
  }

  #patch(target: string, request: Record<string, unknown>): Outcome {
    return this.#steppedUp((session) => {
      const user = this.#users.find((one) => one.id === target);
      if (user === undefined) {
        return api(404, "not_found");
      }
      if (session.user.role !== "owner" && user.role !== "member") {
        return api(403, "forbidden");
      }
      if (request.role === "member" || request.role === "admin") {
        user.role = request.role;
      }
      if (typeof request.disabled === "boolean") {
        user.disabled = request.disabled;
      }
      return value(this.#userResponse(user));
    });
  }

  #createInvite(role: string): Outcome {
    return this.#steppedUp((session) => {
      if (role !== "member" && role !== "admin") {
        return api(403, "forbidden");
      }
      const n = this.#next();
      const invite: Invite = {
        code: `afk_iv_placeholderplaceholderplaceholder${String(n).padStart(3, "0")}`,
        response: {
          id: id(n),
          role,
          state: "pending",
          created_by: { id: session.user.id, username: session.user.username },
          created_at: "2026-10-13T11:00:00.000Z",
          expires_at: "2026-10-16T11:00:00.000Z",
        },
      };
      this.#invites.push(invite);
      return value({ code: invite.code, invite: invite.response } satisfies CreatedInviteResponse);
    });
  }

  /** A server's new settings, as the server's create answers them. */
  #createSettings(request: Record<string, unknown>): Outcome {
    const server = keyOf(String(request.server));
    const texts = textsOf(request.conflict_texts);
    const formats = formatsOf(request.chat_formats);
    if (texts.length === 0 && formats.length === 0) {
      return holdsNothing();
    }
    // The server's order: the cap, then a taken server.
    if (this.#settings.length >= 100) {
      return api(409, "conflict");
    }
    if (this.#settings.some((one) => one.server === server)) {
      return api(422, "validation_failed", [{ path: "server", message: "already has settings" }]);
    }
    const settings: Settings = { id: id(this.#next()), server, texts, formats, version: 1 };
    this.#settings = [settings, ...this.#settings];
    return value(settingsResponse(settings));
  }

  /** Settings `target` as an edit leaves them, at the version it read. */
  #updateSettings(target: string, request: Record<string, unknown>): Outcome {
    const settings = this.#settings.find((one) => one.id === target);
    if (settings === undefined) {
      return api(404, "not_found");
    }
    const server = keyOf(String(request.server));
    const texts = textsOf(request.conflict_texts);
    const formats = formatsOf(request.chat_formats);
    if (
      server === settings.server &&
      texts.join("\0") === settings.texts.join("\0") &&
      formats.join("\0") === settings.formats.join("\0") &&
      formats.length === settings.formats.length
    ) {
      return value(settingsResponse(settings));
    }
    if (Number(request.version) !== settings.version) {
      return api(409, "server_settings_changed");
    }
    if (texts.length === 0 && formats.length === 0) {
      return holdsNothing();
    }
    if (this.#settings.some((one) => one.id !== target && one.server === server)) {
      return api(422, "validation_failed", [{ path: "server", message: "already has settings" }]);
    }
    settings.server = server;
    settings.texts = texts;
    settings.formats = formats;
    settings.version += 1;
    return value(settingsResponse(settings));
  }

  #revokeInvite(target: string): Outcome {
    return this.#steppedUp(() => {
      const index = this.#invites.findIndex((one) => one.response.id === target);
      const found = this.#invites[index];
      if (found === undefined) {
        return api(404, "not_found");
      }
      if (found.response.state === "used") {
        return api(409, "conflict");
      }
      this.#invites[index] = {
        code: found.code,
        response: { ...found.response, state: "revoked", revoked_at: "2026-10-13T12:00:00.000Z" },
      };
      return value();
    });
  }

  // ---- Microsoft accounts (P9.8) ----

  /** How many times the app asked to open Microsoft's page. */
  opened(): number {
    return this.#opened;
  }

  /** An account of `username`'s, named `AfkBot<n>`, for a test to start from. */
  addAccount(username: string, status: AccountStatus = "active"): string {
    return this.#addAccount(this.#user(username), "microsoft", status).id;
  }

  #addAccount(owner: User, kind: AccountKind, status: AccountStatus, name?: string): Account {
    this.#accountNames += 1;
    const accountId = id(this.#next());
    const account: Account = {
      id: accountId,
      name: name ?? `AfkBot${this.#accountNames}`,
      kind,
      status,
      ownerId: owner.id,
      linkedAt: "2026-10-13T12:00:00.000Z",
      grants: new Map(),
      bot: {
        id: id(this.#next()),
        modeId: AFK,
        desiredState: "stopped",
        status: { kind: "unplaced", why: "stopped" },
        chat: [],
      },
    };
    this.#accounts.push(account);
    return account;
  }

  /** What `user` holds on `account`: their grant or their implicit Manage, or nothing. */
  #level(user: User, account: Account): GrantLevel | null {
    const owner = this.#users.find((one) => one.id === account.ownerId);
    if (
      account.ownerId === user.id ||
      user.role === "owner" ||
      (user.role === "admin" && owner?.role === "member")
    ) {
      return "manage";
    }
    return account.grants.get(user.id)?.level ?? null;
  }

  /** Whether `user` may review `account`: someone above its owner, never its owner. */
  #reviews(user: User, account: Account): boolean {
    const owner = this.#users.find((one) => one.id === account.ownerId);
    if (account.status !== "pending" || owner === undefined || owner.id === user.id) {
      return false;
    }
    return user.role === "owner" || (user.role === "admin" && owner.role === "member");
  }

  #accountResponse(user: User, account: Account): AccountResponse {
    const level = this.#level(user, account) ?? "view";
    const owner = this.#users.find((one) => one.id === account.ownerId);
    return {
      id: account.id,
      name: account.name,
      kind: account.kind,
      status: account.status,
      level,
      ...(level === "manage" && owner !== undefined
        ? { owner: { id: owner.id, username: owner.username }, linked_at: account.linkedAt }
        : {}),
    };
  }

  /** `account`'s bot as `user` sees it: the address for Control and above. */
  #botResponse(user: User, account: Account): BotResponse {
    const level = this.#level(user, account) ?? "view";
    const bot = account.bot;
    return {
      id: bot.id,
      account: this.#accountResponse(user, account),
      ...(level !== "view" && bot.serverAddress !== undefined
        ? { server_address: bot.serverAddress }
        : {}),
      mode_id: bot.modeId,
      desired_state: bot.desiredState,
      status: bot.status,
    };
  }

  /**
   * The bot `id` for a write that needs `least`: 404 when `session`'s user can't see it,
   * 403 below `least`; otherwise `then` with its account and the user's level.
   */
  #botWrite(
    session: Session,
    id: unknown,
    least: "control" | "manage",
    then: (account: Account, level: GrantLevel) => Outcome,
  ): Outcome {
    const account = this.#accounts.find((one) => one.bot.id === String(id));
    const level = account === undefined ? null : this.#level(session.user, account);
    if (account === undefined || level === null) {
      return api(404, "not_found");
    }
    const enough = level === "manage" || (least === "control" && level === "control");
    return enough ? then(account, level) : api(403, "forbidden");
  }

  /** `bot` wanted running or stopped: a paused or failed one keeps its state (S51). */
  #want(bot: Bot, running: boolean): void {
    bot.desiredState = running ? "running" : "stopped";
    const { state, reason } = bot.status;
    const resting =
      (state === "paused" || state === "failed") && reason !== undefined ? { state, reason } : {};
    if (!running) {
      bot.status = { kind: "unplaced", why: "stopped", ...resting };
    } else if (bot.serverAddress === undefined) {
      bot.status = { kind: "unplaced", why: "no_server", ...resting };
    } else {
      bot.status = { kind: "awaiting_report", since: new Date().toISOString() };
    }
  }

  /**
   * A restart, a reset or a resume (S50, S73): sent to a reported bot in the state it needs,
   * cleared for a resting bot that runs nowhere, otherwise the server's 409.
   */
  #command(user: User, account: Account, command: string): Outcome {
    const { status } = account.bot;
    const needs = command === "reset_bot" ? "failed" : "paused";
    const answer = (effect: BotCommandResponse["effect"]) =>
      value({ bot: this.#botResponse(user, account), effect } satisfies BotCommandResponse);
    if (command === "restart_bot") {
      const running = ["online", "connecting", "awaiting_session", "backoff"];
      return status.kind === "reported" && running.includes(String(status.state))
        ? answer("sent")
        : api(409, "bot_not_running");
    }
    if (status.state !== needs) {
      return api(409, needs === "failed" ? "bot_not_failed" : "bot_not_paused");
    }
    if (status.kind === "unplaced") {
      account.bot.status = { kind: "unplaced", why: status.why ?? "stopped" };
      return answer("cleared");
    }
    return answer("sent");
  }

  /**
   * A line `user` at `level` has the bot send through `route` (ADR-0020, S4, S5, S8): a
   * command off the allowlist only through `/command` and for Manage (`/chat` points a
   * manager there), an online bot only; then the line is kept and answered with
   * `chatOutcome`.
   */
  #send(
    user: User,
    account: Account,
    level: GrantLevel,
    message: string,
    route: "chat" | "command",
  ): Outcome {
    const line = message.trim();
    if (route === "command" && !line.startsWith("/")) {
      return api(422, "validation_failed", [
        { path: "message", message: "must be a command, starting with /" },
      ]);
    }
    const name = line.split(/\s/u)[0] ?? "";
    if (line.startsWith("/") && !ALLOWLIST.includes(name)) {
      if (level !== "manage") {
        return api(403, "forbidden");
      }
      if (route === "chat") {
        return api(403, "command_needs_step_up");
      }
    }
    const { status } = account.bot;
    if (status.kind !== "reported" || status.state !== "online") {
      return api(409, "bot_not_online");
    }
    const kept: ChatLine = {
      id: this.#nextLine(),
      at: new Date().toISOString(),
      direction: "out",
      kind: "user",
      text: line,
      truncated: false,
      arguments_hidden: false,
      sent_by: { id: user.id, username: user.username },
      mine: true,
    };
    account.bot.chat.push(kept);
    const answer: ChatSendResponse = {
      request_id: id(this.#next()),
      message_id: kept.id,
      outcome: this.chatOutcome,
    };
    this.lastSend = { request: answer.request_id, line: kept };
    return value(answer);
  }

  /**
   * A page of `account`'s chat as `user` at `level` reads it, newest first, older than
   * `before`: a command a user or a mode sent shows only its name below Manage (S9), and
   * who sent a user's line only to Manage (U3).
   */
  #chatPage(
    user: User,
    account: Account,
    level: GrantLevel,
    before: unknown,
    limit: unknown,
  ): ChatPage {
    const size = typeof limit === "number" ? limit : 50;
    const older = [...account.bot.chat]
      .reverse()
      .filter((line) => typeof before !== "number" || line.id < before);
    const items = older.slice(0, size).map((line): ChatLine => {
      const mine = line.sent_by?.id === user.id;
      const { sent_by: sentBy, ...rest } = line;
      const sent = level === "manage" && sentBy !== undefined ? { sent_by: sentBy } : {};
      const cut =
        level !== "manage" && (line.kind === "user" || line.kind === "mode")
          ? line.text.match(/^(\/\S*)\s+\S/u)?.[1]
          : undefined;
      return cut === undefined
        ? { ...rest, ...sent, mine }
        : { ...rest, ...sent, text: cut, arguments_hidden: true, mine };
    });
    const last = items.at(-1);
    return older.length > size && last !== undefined ? { items, next: last.id } : { items };
  }

  /** The next chat line's ID: a newer line has a greater one. */
  #nextLine(): number {
    this.#lines += 1;
    return this.#lines;
  }

  /**
   * Adds `line` to the chat of the bot of the account named `name`, as its agent would
   * report it; returns the line with its ID, for a spec to push as the `chat` frame.
   */
  addChat(name: string, line: Omit<ChatLine, "id">): ChatLine {
    const account = this.#accounts.find((one) => one.name === name);
    if (account === undefined) {
      throw new Error(`the fake has no account ${name}`);
    }
    const kept: ChatLine = { ...line, id: this.#nextLine() };
    account.bot.chat.push(kept);
    return kept;
  }

  /** Changes the bot of the account named `name`, as its agent's reports or a write would. */
  setBot(name: string, changes: Partial<Omit<Bot, "id">>): void {
    const account = this.#accounts.find((one) => one.name === name);
    if (account === undefined) {
      throw new Error(`the fake has no account ${name}`);
    }
    Object.assign(account.bot, changes);
  }

  /** The bot of the account named `name`, as the Owner sees it. */
  bot(name: string): BotResponse {
    const account = this.#accounts.find((one) => one.name === name);
    if (account === undefined) {
      throw new Error(`the fake has no account ${name}`);
    }
    return this.#botResponse(this.#user(OWNER), account);
  }

  /** Removes the account named `name`, and its bot with it. */
  removeAccount(name: string): void {
    const at = this.#accounts.findIndex((one) => one.name === name);
    if (at < 0) {
      throw new Error(`the fake has no account ${name}`);
    }
    this.#accounts.splice(at, 1);
  }

  /** `account`, if `user` may see it, with what they hold on it. */
  #seen(user: User, target: unknown): { account: Account; level: GrantLevel } | null {
    const account = this.#accounts.find((one) => one.id === String(target));
    if (account === undefined) {
      return null;
    }
    const level = this.#level(user, account);
    return level === null ? null : { account, level };
  }

  /** `then` on an account the session's user manages; 404 or 403 otherwise. */
  #managed(session: Session, target: unknown, then: (account: Account) => Outcome): Outcome {
    const seen = this.#seen(session.user, target);
    if (seen === null) {
      return api(404, "not_found");
    }
    return seen.level === "manage" ? then(seen.account) : api(403, "forbidden");
  }

  #startLink(again: unknown): Outcome {
    return this.#signedIn((session) => {
      let relinked: string | null = null;
      if (again !== undefined) {
        const seen = this.#seen(session.user, again);
        if (seen === null) {
          return api(404, "not_found");
        }
        if (seen.level !== "manage") {
          return api(403, "forbidden");
        }
        if (seen.account.status === "pending" || seen.account.kind === "offline") {
          return api(409, "conflict");
        }
        relinked = seen.account.id;
      }
      this.#flow = { id: id(this.#next()), userId: session.user.id, again: relinked, reads: 0 };
      return value({
        flow_id: this.#flow.id,
        user_code: LINK_CODE,
        verification_uri: "https://www.microsoft.com/link",
        expires_at: "2026-10-13T12:15:00.000Z",
      } satisfies LinkStartedResponse);
    });
  }

  /** The flow's state: waiting at its first read, linked from its second on. */
  #linkStatus(target: unknown): Outcome {
    return this.#signedIn((session) => {
      const flow = this.#flow;
      if (flow === null || flow.id !== String(target) || flow.userId !== session.user.id) {
        return api(404, "not_found");
      }
      flow.reads += 1;
      if (flow.reads === 1) {
        return value({
          status: "waiting",
          expires_at: "2026-10-13T12:15:00.000Z",
        } satisfies LinkFlowResponse);
      }
      flow.linked ??= this.#commit(session.user, flow.again);
      return value({
        status: "linked",
        account_id: flow.linked.id,
        account_status: flow.linked.status,
        name: flow.linked.name,
      } satisfies LinkFlowResponse);
    });
  }

  /** A flow's commit: a new account, pending unless the Owner linked it, or one linked again. */
  #commit(user: User, again: string | null): Account {
    const relinked = this.#accounts.find((one) => one.id === again);
    if (relinked !== undefined) {
      relinked.status = "active";
      return relinked;
    }
    return this.#addAccount(user, "microsoft", user.role === "owner" ? "active" : "pending");
  }

  #grantsOf(account: Account): GrantResponse[] {
    return [...account.grants.entries()].map(([userId, grant]) => {
      const user = this.#users.find((one) => one.id === userId);
      const by = this.#users.find((one) => one.id === grant.by);
      return {
        user: { id: userId, username: user?.username ?? "" },
        level: grant.level,
        granted_by: { id: grant.by, username: by?.username ?? "" },
        granted_at: "2026-10-13T12:30:00.000Z",
      };
    });
  }

  #createGrant(target: unknown, request: Record<string, unknown>): Outcome {
    return this.#steppedUp((session) =>
      this.#managed(session, target, (account) => {
        const grantee = this.#users.find(
          (one) => one.username === String(request.username) && !one.disabled,
        );
        if (grantee === undefined || this.#level(grantee, account) === "manage") {
          return api(422, "validation_failed", [{ path: "username", message: UNGRANTABLE }]);
        }
        if (account.grants.has(grantee.id) || account.status === "pending") {
          return api(409, "conflict");
        }
        const level = String(request.level) as GrantLevel;
        account.grants.set(grantee.id, { level, by: session.user.id });
        return value(this.#grantsOf(account).find((grant) => grant.user.id === grantee.id));
      }),
    );
  }

  #changeGrant(target: unknown, user: unknown, request: Record<string, unknown>): Outcome {
    return this.#steppedUp((session) =>
      this.#managed(session, target, (account) => {
        const grant = account.grants.get(String(user));
        if (grant === undefined) {
          return api(404, "not_found");
        }
        grant.level = String(request.level) as GrantLevel;
        return value(this.#grantsOf(account).find((one) => one.user.id === String(user)));
      }),
    );
  }

  #review(target: unknown, approve: boolean): Outcome {
    return this.#steppedUp((session) => {
      const seen = this.#seen(session.user, target);
      if (seen === null) {
        return api(404, "not_found");
      }
      if (seen.account.status !== "pending") {
        return api(409, "conflict");
      }
      if (!this.#reviews(session.user, seen.account)) {
        return api(403, "forbidden");
      }
      if (!approve) {
        this.#accounts.splice(this.#accounts.indexOf(seen.account), 1);
        return value();
      }
      seen.account.status = "active";
      return value(this.#accountResponse(session.user, seen.account));
    });
  }

  #accountCommand(command: Command, args: Record<string, unknown>): Outcome {
    const request = (args.request ?? {}) as Record<string, unknown>;
    switch (command) {
      case "start_link":
        return this.#startLink(undefined);
      case "relink_account":
        return this.#startLink(args.id);
      case "link_status":
        return this.#linkStatus(args.id);
      case "open_verification_link":
        if (this.#flow === null) {
          return refused("no_verification_link");
        }
        this.#opened += 1;
        return value();
      case "create_offline_account":
        return this.#signedIn((session) => {
          const account = this.#addAccount(session.user, "offline", "active", String(request.name));
          return value({
            id: account.id,
            name: account.name,
            kind: "offline",
            status: "active",
          } satisfies CreatedOfflineAccountResponse);
        });
      case "accounts":
        return this.#signedIn((session) =>
          value({
            items: [...this.#accounts]
              .reverse()
              .filter((account) => this.#level(session.user, account) !== null)
              .map((account) => this.#accountResponse(session.user, account)),
          } satisfies AccountPage),
        );
      case "accounts_awaiting_approval":
        return this.#signedIn((session) =>
          value({
            items: this.#accounts
              .filter((account) => this.#reviews(session.user, account))
              .map((account) => this.#accountResponse(session.user, account)),
          } satisfies AccountPage),
        );
      case "account":
        return this.#signedIn((session) => {
          const seen = this.#seen(session.user, args.id);
          return seen === null
            ? api(404, "not_found")
            : value(this.#accountResponse(session.user, seen.account));
        });
      case "delete_account":
        return this.#steppedUp((session) =>
          this.#managed(session, args.id, (account) => {
            this.#accounts.splice(this.#accounts.indexOf(account), 1);
            return value();
          }),
        );
      case "bots":
        return this.#signedIn((session) =>
          value({
            items: [...this.#accounts]
              .reverse()
              .filter((account) => this.#level(session.user, account) !== null)
              .map((account) => this.#botResponse(session.user, account)),
          } satisfies BotPage),
        );
      case "bot":
        return this.#signedIn((session) => {
          const account = this.#accounts.find((one) => one.bot.id === String(args.id));
          return account === undefined || this.#level(session.user, account) === null
            ? api(404, "not_found")
            : value(this.#botResponse(session.user, account));
        });
      case "modes":
        return this.#signedIn(() => value({ items: [...MODES] } satisfies ModePage));
      case "patch_bot":
        return this.#steppedUp((session) =>
          this.#botWrite(session, args.id, "manage", (account) => {
            account.bot.serverAddress = String(request.server_address);
            return value(this.#botResponse(session.user, account));
          }),
        );
      case "set_bot_mode":
        return this.#signedIn((session) =>
          this.#botWrite(session, args.id, "control", (account, level) => {
            const mode = MODES.find((one) => one.id === String(request.mode_id));
            if (mode === undefined) {
              return api(422, "validation_failed", [
                { path: "mode_id", message: "isn't a mode you can use" },
              ]);
            }
            if (mode.unlisted_command && level !== "manage") {
              return api(403, "forbidden");
            }
            account.bot.modeId = mode.id;
            return value(this.#botResponse(session.user, account));
          }),
        );
      case "start_bot":
      case "stop_bot":
        return this.#signedIn((session) =>
          this.#botWrite(session, args.id, "control", (account) => {
            this.#want(account.bot, command === "start_bot");
            return value(this.#botResponse(session.user, account));
          }),
        );
      case "restart_bot":
      case "reset_bot":
      case "resume_bot":
        return this.#signedIn((session) =>
          this.#botWrite(session, args.id, "control", (account) =>
            this.#command(session.user, account, command),
          ),
        );
      case "bot_chat":
        return this.#signedIn((session) => {
          const account = this.#accounts.find((one) => one.bot.id === String(args.id));
          const level = account === undefined ? null : this.#level(session.user, account);
          return account === undefined || level === null
            ? api(404, "not_found")
            : value(this.#chatPage(session.user, account, level, args.before, args.limit));
        });
      case "send_chat":
        return this.#signedIn((session) =>
          this.#botWrite(session, args.id, "control", (account, level) =>
            this.#send(session.user, account, level, String(request.message), "chat"),
          ),
        );
      case "send_command":
        return this.#steppedUp((session) =>
          this.#botWrite(session, args.id, "control", (account, level) =>
            this.#send(session.user, account, level, String(request.message), "command"),
          ),
        );
      case "approve_account":
        return this.#review(args.id, true);
      case "refuse_account":
        return this.#review(args.id, false);
      case "account_grants":
        return this.#signedIn((session) =>
          this.#managed(session, args.id, (account) =>
            value({ items: this.#grantsOf(account) } satisfies GrantPage),
          ),
        );
      case "create_grant":
        return this.#createGrant(args.id, request);
      case "change_grant":
        return this.#changeGrant(args.id, args.user, request);
      case "revoke_grant":
        return this.#steppedUp((session) =>
          this.#managed(session, args.id, (account) =>
            account.grants.delete(String(args.user)) ? value() : api(404, "not_found"),
          ),
        );
      default:
        throw new Error(`the fake doesn't know ${command}`);
    }
  }
}
