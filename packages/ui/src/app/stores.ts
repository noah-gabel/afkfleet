// The app's UI-only state that code outside React sets (Plan.md P8.6, ADR-0017): small
// zustand stores, created once per App (createAppStores), so two Apps (or two tests) never
// share state. Components read them with `useStore`; the watched client (watched.ts) and
// the runtime set them.
//
// - `session`: whether the user is signed in, and a generation that every sign-in bumps.
//   A call's `no_session` ends the session only while it's signed in and only if the call
//   started in the current generation, and the flip happens once: so a session end is
//   handled once, never at launch, inside AuthFlow or during a deliberate logout, and an
//   answer from before a re-login can't end the new session.
// - `connection`: the banner's state. Its epoch is bumped each time the banner goes up, and
//   only a call that started after that can take it down again.
// - `notices`: the warnings Home's user dismissed, for the sign-in (the session's
//   generation) they dismissed them in; a new sign-in shows them again (Plan.md P8.7).
// - `live`: the live events layer's (live.ts) hint for the Header while a reconnect waits or
//   after the watch stopped (U30), and what it's asked from outside: a new watch, after a
//   server profile save that changed nothing (P11.8, S61). Also what the chat's send box
//   reads (S70, R115): a count of the moments updates may have been missed, and whether the
//   current watch is live.
// - `record`: the live record (api/liveRecord.ts, S71) that the timeline and the notices
//   show, which the live layer feeds and clears; and the bot whose screen is open (R109).
// - `theme`: light or dark, from what this PC remembers and what Windows says (theme.ts, S92).
// - `back`: whether a step on screen has taken the frame's Back away, as leaving it would lose
//   what it shows for good (U112).
// - `update`: the app's updates (ADR-0022, U27, S27): whether the start's one check ran, the
//   last check's answer, an install under way or failed, and whether Later put the notice away.
//   It lives as long as the App, never in the query cache, which a logout clears and a resync
//   invalidates: either would check GitHub again.
import { createStore, type StoreApi } from "zustand/vanilla";
import {
  acknowledge,
  dismiss,
  down,
  drop,
  emptyRecord,
  known,
  type LiveRecord,
  observe,
} from "../api/liveRecord";
import type { BotResponse } from "../generated/BotResponse";
import type { CommandError } from "../generated/CommandError";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import type { UpdateCheck } from "../generated/UpdateCheck";
import {
  readThemeChoice,
  resolveTheme,
  saveThemeChoice,
  type ThemeState,
  windowsTheme,
} from "./theme";

/** Where the user's session stands, as far as the UI knows. */
export type SessionPhase = "starting" | "signed_out" | "signed_in" | "logging_out";

/** The session store; see the module comment. */
export interface SessionState {
  readonly phase: SessionPhase;
  /** Bumped by every sign-in. */
  readonly generation: number;
  /** A session started: signed in, with a new generation. */
  signIn(): void;
  /** No session (launch found none, a logout or a server switch ended it). */
  signOut(): void;
  /** A deliberate logout is under way: no `no_session` ends anything now. */
  beginLogout(): void;
  /** The logout failed: still signed in, in the same generation. */
  cancelLogout(): void;
  /**
   * Ends the session for a `no_session` answer to a call that started in `generation`:
   * true, once, if the session was signed in and in that generation.
   */
  endIfSignedIn(generation: number): boolean;
}

/** What the banner shows. */
export type ConnectionStatus = "ok" | "down" | "busy" | "certificate";

/** The connection store; see the module comment. */
export interface ConnectionState {
  readonly status: ConnectionStatus;
  /** Bumped each time the banner goes up. */
  readonly epoch: number;
  /** How long a busy server asked to wait, in seconds. */
  readonly retryAfterSecs: number | undefined;
  /** The last check's failure text, when it was neither of the banner's own states. */
  readonly detail: string | undefined;
  /** The server can't be reached (`down`) or its certificate is refused. */
  fail(status: "down" | "certificate"): void;
  /** The server answered busy (or rate-limited the check): wait `retryAfterSecs`. */
  busy(retryAfterSecs: number): void;
  /** A check failed some other way: the banner stays, with `detail`. */
  checkFailed(detail: string): void;
  /**
   * A call that started at `epoch` succeeded: true if that took the banner down, which
   * happens only for a call that started after the banner went up.
   */
  recover(epoch: number): boolean;
}

/** A warning Home shows about the user's account. */
export type HomeWarning = "failedCodes" | "recoveryCodes";

/** The notices store; see the module comment. */
export interface NoticesState {
  /** The warnings dismissed in the sign-in `generation`. */
  readonly dismissed: { readonly generation: number; readonly warnings: readonly HomeWarning[] };
  /** Hides `warning` on Home until the next sign-in after `generation`. */
  dismiss(warning: HomeWarning, generation: number): void;
}

/** What the Header says about the live events (U30). */
export type LiveHint =
  /** Nothing: live, or a reconnect that doesn't wait. */
  | { readonly kind: "none" }
  /** A reconnect waits `secs` seconds, until `until` (ms since the epoch); `tooMany` after
   * `too_many_connections`. */
  | {
      readonly kind: "waiting";
      readonly secs: number;
      readonly until: number;
      readonly tooMany: boolean;
    }
  /**
   * The watch stopped with `error`, which isn't a session's end; `null` when it stopped by
   * itself while signed in, with no save running (S64's broken channel, R91).
   */
  | { readonly kind: "stopped"; readonly error: CommandError | null };

/** The live events store; see the module comment. */
export interface LiveState {
  /** The Header's hint. */
  readonly hint: LiveHint;
  /** Shows `hint`. */
  setHint(hint: LiveHint): void;
  /** How many new watches were asked for. */
  readonly restarts: number;
  /** Asks for a new watch of the current session. */
  restart(): void;
  /**
   * Moved whenever live updates may have been missed: a watch's start, the current watch's
   * end and every refetch (S70, R115). A pending chat line's mark lasts while it stays.
   */
  readonly losses: number;
  /** Live updates may have been missed. */
  lost(): void;
  /** The current watch is live: from its refetch until its disconnect or its end (R115). */
  readonly connected: boolean;
  /** Says whether the current watch is live; the same value again changes nothing. */
  setConnected(connected: boolean): void;
}

/** The live record's store; see the module comment. */
export interface RecordState {
  /** The record. */
  readonly record: LiveRecord;
  /** The bot whose screen is open: its notices are acknowledged as they come (R109). */
  readonly open: string | null;
  /** The current watch's `update` arrived at `now`. */
  observe(update: LiveUpdateMessage, now: number): void;
  /** The current watch ended at `now`. */
  down(now: number): void;
  /** A read answered `bots`: they're known where the record doesn't know them yet. */
  known(bots: readonly BotResponse[]): void;
  /** Bot `botId` is gone: its read answered 404. */
  drop(botId: string): void;
  /** Notice `id` was dismissed. */
  dismiss(id: number): void;
  /** Bot `botId`'s screen opened, acknowledging its notices; null when it closed. */
  setOpen(botId: string | null): void;
  /** A sign-out or a new sign-in: nothing is kept. */
  clear(): void;
}

/** The Back control's store; see the module comment. */
export interface BackState {
  /** A step on screen has taken Back away. */
  readonly held: boolean;
  /** Says whether a step on screen takes Back away. */
  hold(held: boolean): void;
}

/** Where an install stands: none, under way (the app exits at its end), or failed. */
export type InstallPhase = "idle" | "installing" | "failed";

/** The updates' store; see the module comment. */
export interface UpdateState {
  /** The start's check was asked: it runs once per App. */
  readonly started: boolean;
  /** The last check's answer; null before one answered, or after one failed. */
  readonly check: UpdateCheck | null;
  /** A check runs. */
  readonly checking: boolean;
  /** Why the last check the user asked for failed; the start's fails quietly. */
  readonly checkFailure: CommandError | null;
  readonly install: InstallPhase;
  /** Why the last install failed. */
  readonly installFailure: CommandError | null;
  /** Later put the notice away, until the next start. */
  readonly later: boolean;
  /** Marks the start's check as asked: true the first time only. */
  start(): boolean;
  /** A check starts. */
  beginCheck(): void;
  /** A check answered `check`. */
  checked(check: UpdateCheck): void;
  /** A check failed: `failure` when the user asked for it, null when it was the start's. */
  checkFailed(failure: CommandError | null): void;
  /** An install starts. */
  beginInstall(): void;
  /** An install failed with `failure`. */
  installFailed(failure: CommandError): void;
  /** Later: the notice goes away until the next start, a failed install with it. */
  putAway(): void;
}

/** The stores of one App. */
export interface AppStores {
  readonly session: StoreApi<SessionState>;
  readonly connection: StoreApi<ConnectionState>;
  readonly notices: StoreApi<NoticesState>;
  readonly live: StoreApi<LiveState>;
  readonly record: StoreApi<RecordState>;
  readonly theme: StoreApi<ThemeState>;
  readonly back: StoreApi<BackState>;
  readonly update: StoreApi<UpdateState>;
}

/** Fresh stores for one App. */
export function createAppStores(): AppStores {
  const choice = readThemeChoice();
  const windows = windowsTheme();
  return {
    update: createStore<UpdateState>()((set, get) => ({
      started: false,
      check: null,
      checking: false,
      checkFailure: null,
      install: "idle",
      installFailure: null,
      later: false,
      start() {
        if (get().started) {
          return false;
        }
        set({ started: true });
        return true;
      },
      beginCheck() {
        set({ checking: true, checkFailure: null });
      },
      checked(check) {
        set({ checking: false, check, checkFailure: null });
      },
      checkFailed(failure) {
        set({ checking: false, check: null, checkFailure: failure });
      },
      beginInstall() {
        set({ install: "installing", installFailure: null, later: false });
      },
      installFailed(failure) {
        set({ install: "failed", installFailure: failure });
      },
      putAway() {
        set({ later: true, install: get().install === "failed" ? "idle" : get().install });
      },
    })),
    back: createStore<BackState>()((set) => ({
      held: false,
      hold(held) {
        set({ held });
      },
    })),
    theme: createStore<ThemeState>()((set, get) => ({
      choice,
      windows,
      theme: resolveTheme(choice, windows),
      sent: null,
      inFlight: null,
      choose(next) {
        saveThemeChoice(next);
        set({ choice: next, theme: resolveTheme(next, get().windows) });
      },
      windowsChanged(said) {
        set({ windows: said, theme: resolveTheme(get().choice, said) });
      },
    })),
    session: createStore<SessionState>()((set, get) => ({
      phase: "starting",
      generation: 0,
      signIn() {
        set({ phase: "signed_in", generation: get().generation + 1 });
      },
      signOut() {
        set({ phase: "signed_out" });
      },
      beginLogout() {
        set({ phase: "logging_out" });
      },
      cancelLogout() {
        if (get().phase === "logging_out") {
          set({ phase: "signed_in" });
        }
      },
      endIfSignedIn(generation) {
        const state = get();
        if (state.phase !== "signed_in" || state.generation !== generation) {
          return false;
        }
        set({ phase: "signed_out" });
        return true;
      },
    })),
    connection: createStore<ConnectionState>()((set, get) => {
      /** The banner's new state; the epoch moves only when it goes up. */
      const raise = (
        status: Exclude<ConnectionStatus, "ok">,
        retryAfterSecs: number | undefined,
      ) => {
        const state = get();
        set({
          status,
          retryAfterSecs,
          detail: undefined,
          epoch: state.status === "ok" ? state.epoch + 1 : state.epoch,
        });
      };
      return {
        status: "ok",
        epoch: 0,
        retryAfterSecs: undefined,
        detail: undefined,
        fail(status) {
          raise(status, undefined);
        },
        busy(retryAfterSecs) {
          raise("busy", retryAfterSecs);
        },
        checkFailed(detail) {
          if (get().status !== "ok") {
            set({ detail });
          }
        },
        recover(epoch) {
          const state = get();
          if (state.status === "ok" || epoch < state.epoch) {
            return false;
          }
          set({ status: "ok", retryAfterSecs: undefined, detail: undefined });
          return true;
        },
      };
    }),
    notices: createStore<NoticesState>()((set, get) => ({
      dismissed: { generation: 0, warnings: [] },
      dismiss(warning, generation) {
        const { dismissed } = get();
        const earlier = dismissed.generation === generation ? dismissed.warnings : [];
        if (!earlier.includes(warning)) {
          set({ dismissed: { generation, warnings: [...earlier, warning] } });
        }
      },
    })),
    live: createStore<LiveState>()((set, get) => ({
      hint: { kind: "none" },
      setHint(hint) {
        set({ hint });
      },
      restarts: 0,
      restart() {
        set({ restarts: get().restarts + 1 });
      },
      losses: 0,
      lost() {
        set({ losses: get().losses + 1 });
      },
      connected: false,
      setConnected(connected) {
        // Only a change notifies: the live layer follows this store, and sets it as it does.
        if (get().connected !== connected) {
          set({ connected });
        }
      },
    })),
    record: createStore<RecordState>()((set, get) => ({
      record: emptyRecord(),
      open: null,
      observe(update, now) {
        set({ record: observe(get().record, update, now, get().open) });
      },
      down(now) {
        set({ record: down(get().record, now) });
      },
      known(bots) {
        set({ record: known(get().record, bots) });
      },
      drop(botId) {
        set({ record: drop(get().record, botId) });
      },
      dismiss(id) {
        set({ record: dismiss(get().record, id) });
      },
      setOpen(botId) {
        const { record } = get();
        set({ open: botId, record: botId === null ? record : acknowledge(record, botId) });
      },
      clear() {
        // The IDs go on growing, so the toaster never takes a new notice for one it showed.
        set({ record: { ...emptyRecord(), sequence: get().record.sequence } });
      },
    })),
  };
}
