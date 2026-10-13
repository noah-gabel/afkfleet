// The e2e suite's fake backend (Plan.md P8.9, ADR-0017): the desktop app's Rust side and the
// server in one, answering the 20 commands of src-tauri/tests/ipc-contract.json with the
// generated types, the Rust side's fixed `CommandError` messages and the server's error
// codes. It keeps the state one test needs, fresh per test: users with their passwords and
// authenticators, invites, the one session the app holds, the pending login or setup step,
// and the session's step-up.
//
// An authenticator here has one valid code at a time (`code(username)`), which a successful
// use replaces, like a TOTP code that can't be used twice. A pending setup expects
// `setupCode()`. Everything is a placeholder (CLAUDE.md).
import type { ApiErrorDetails } from "@afkfleet/ui/generated/ApiErrorDetails";
import type { CommandError } from "@afkfleet/ui/generated/CommandError";
import type { CommandErrorCode } from "@afkfleet/ui/generated/CommandErrorCode";
import type { CreatedInviteResponse } from "@afkfleet/ui/generated/CreatedInviteResponse";
import type { ErrorCode } from "@afkfleet/ui/generated/ErrorCode";
import type { InvitePage } from "@afkfleet/ui/generated/InvitePage";
import type { InviteResponse } from "@afkfleet/ui/generated/InviteResponse";
import type { LoginStepResponse } from "@afkfleet/ui/generated/LoginStepResponse";
import type { LogoutResponse } from "@afkfleet/ui/generated/LogoutResponse";
import type { MeResponse } from "@afkfleet/ui/generated/MeResponse";
import type { MfaConfirmResponse } from "@afkfleet/ui/generated/MfaConfirmResponse";
import type { MfaEnrollResponse } from "@afkfleet/ui/generated/MfaEnrollResponse";
import type { RegisterResponse } from "@afkfleet/ui/generated/RegisterResponse";
import type { ServerProfileState } from "@afkfleet/ui/generated/ServerProfileState";
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
};

/** The server's messages (fleet-api-types' `ErrorCode::message`). */
const API_MESSAGES: Partial<Record<ErrorCode, string>> = {
  invalid_credentials: "The username, password or code is wrong.",
  forbidden: "You don't have permission to do this.",
  step_up_required: "Enter your password again to do this.",
  not_found: "The resource doesn't exist.",
  conflict: "The request conflicts with the current state.",
  validation_failed: "Some fields are invalid.",
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

/** A pending login or setup step. */
type Pending =
  | { readonly kind: "mfa"; readonly user: User; tries: number }
  | { readonly kind: "setup"; readonly user: User; readonly replace: boolean; enrolled: boolean };

/** The fake backend of one test; see the module comment. */
export class FakeBackend {
  readonly #users: User[] = [];
  readonly #invites: Invite[] = [];
  #session: Session | null = null;
  #pending: Pending | null = null;
  #ids = 100;
  #codes = 0;

  constructor() {
    this.#addUser(OWNER, "owner", true);
    this.#addUser(NEW_USER, "member", false);
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

  /** Runs `command` with `args`, as the app's Rust side and the server would. */
  handle(command: string, args: Record<string, unknown>): Outcome {
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
      default:
        throw new Error(`the fake doesn't know ${command}`);
    }
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
}
