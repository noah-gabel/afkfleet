// The queries more than one screen shares (Plan.md P8.6): the user and the server profile;
// and the lists the server answers in pages, newest first, which a screen shows a page at a
// time with "Show more" (P8.7, P8.8). No query or mutation may carry a secret in its request
// or its answer (a password, a TOTP secret or QR code, recovery codes, an invite code):
// those calls are direct calls, so nothing secret ever sits in TanStack Query's caches
// (CLAUDE.md, ADR-0017). A Microsoft link's start answers its user code, so it's a direct
// call too; its status carries none and is a query (features/accounts).
import {
  type DataTag,
  type InfiniteData,
  infiniteQueryOptions,
  type QueryClient,
  type QueryKey,
  queryOptions,
  skipToken,
  useInfiniteQuery,
  useQuery,
} from "@tanstack/react-query";

import type { AccountPage } from "../generated/AccountPage";
import type { AccountResponse } from "../generated/AccountResponse";
import type { AgentPage } from "../generated/AgentPage";
import type { AuditPage } from "../generated/AuditPage";
import type { BotPage } from "../generated/BotPage";
import type { BotResponse } from "../generated/BotResponse";
import type { ChatPage } from "../generated/ChatPage";
import type { ChatSendResponse } from "../generated/ChatSendResponse";
import type { GrantPage } from "../generated/GrantPage";
import type { HeldGrantPage } from "../generated/HeldGrantPage";
import type { InvitePage } from "../generated/InvitePage";
import type { ModePage } from "../generated/ModePage";
import type { ServerSettingsResponse } from "../generated/ServerSettingsResponse";
import type { SessionPage } from "../generated/SessionPage";
import type { UserLabel } from "../generated/UserLabel";
import type { UserPage } from "../generated/UserPage";
import type { ApiClient } from "./ApiClient";
import { ApiError } from "./ApiError";
import { answered, type ChatOutcomes } from "./chatOutcomes";
import { useApiClient } from "./context";
import { PAGE_MAX, SERVER_SETTINGS_PAGE_MAX } from "./limits";

/** Why a read of every page failed when the server's pages didn't move on (R99). */
export const PAGES_DONT_MOVE_ON = "The server sent a list this app doesn't understand.";

/**
 * How long `me` stays fresh. A role never changes under a live session: the change revokes every
 * session of the user, which ends this one at its next call (ADR-0020, S87, R181).
 */
export const ME_STALE_MS = 60_000;

/** The signed-in user. */
export function meQuery(api: ApiClient) {
  return queryOptions({ queryKey: ["me"], queryFn: () => api.me(), staleTime: ME_STALE_MS });
}

/** The server profile; only the app changes it, and invalidates it when it does. */
export function serverProfileQuery(api: ApiClient) {
  return queryOptions({
    queryKey: ["serverProfile"],
    queryFn: () => api.serverProfile(),
    staleTime: Number.POSITIVE_INFINITY,
  });
}

/** The signed-in user, through the app's client. */
export function useMe() {
  return useQuery(meQuery(useApiClient()));
}

/** The server profile, through the app's client. */
export function useServerProfile() {
  return useQuery(serverProfileQuery(useApiClient()));
}

/** The user's sessions: every page loaded so far. */
export const SESSIONS_KEY = ["sessions"] as const;

/** The user's live sessions, a page at a time. */
export function sessionsQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: SESSIONS_KEY,
    queryFn: ({ pageParam }) => api.sessions(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: SessionPage) => page.next,
  });
}

/** The user's sessions, through the app's client. */
export function useSessions() {
  return useInfiniteQuery(sessionsQuery(useApiClient()));
}

/** The users (Admins and the Owner): every page loaded so far. */
export const USERS_KEY = ["users"] as const;

/** The users, a page at a time. */
export function usersQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: USERS_KEY,
    queryFn: ({ pageParam }) => api.users(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: UserPage) => page.next,
  });
}

/** The users, through the app's client. */
export function useUsers() {
  return useInfiniteQuery(usersQuery(useApiClient()));
}

/**
 * Every user's ID and username, read through every page once: the audit's names (ADR-0020,
 * S85). Under `USERS_KEY`, so the users screen's own invalidation reaches it (R176).
 */
export const USER_NAMES_KEY = [...USERS_KEY, "names"] as const;

/** Every user's label; one failed page fails it all, and the audit shows IDs instead. */
export function userNamesQuery(api: ApiClient) {
  return queryOptions({
    queryKey: USER_NAMES_KEY,
    queryFn: async ({ signal }): Promise<UserLabel[]> => {
      const read = await everyPage((before) => api.users(before, PAGE_MAX), signal);
      return read.pages.flatMap((page) => page.items.map(({ id, username }) => ({ id, username })));
    },
  });
}

/** Every user's label, through the app's client. */
export function useUserNames() {
  return useQuery(userNamesQuery(useApiClient()));
}

/** The key of the grants user `id` holds (the Owner's review; ADR-0020, R176). */
export function heldGrantsKey(id: string) {
  return [...USERS_KEY, "held", id] as const;
}

/** The grants user `id` holds, dormant ones included, a page at a time. */
export function heldGrantsQuery(api: ApiClient, id: string) {
  return infiniteQueryOptions({
    queryKey: heldGrantsKey(id),
    queryFn: ({ pageParam }) => api.userGrants(id, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: HeldGrantPage) => page.next,
  });
}

/** The grants user `id` holds, through the app's client. */
export function useHeldGrants(id: string) {
  return useInfiniteQuery(heldGrantsQuery(useApiClient(), id));
}

/** The key of user `id`'s accounts that a grant shares (the Owner's review; R176). */
export function sharedAccountsKey(id: string) {
  return [...USERS_KEY, "shared", id] as const;
}

/** User `id`'s accounts that a grant shares, a page at a time. */
export function sharedAccountsQuery(api: ApiClient, id: string) {
  return infiniteQueryOptions({
    queryKey: sharedAccountsKey(id),
    queryFn: ({ pageParam }) => api.sharedAccounts(id, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: AccountPage) => page.next,
  });
}

/** User `id`'s accounts that a grant shares, through the app's client. */
export function useSharedAccounts(id: string) {
  return useInfiniteQuery(sharedAccountsQuery(useApiClient(), id));
}

/** The invites, in every state, without their codes: every page loaded so far. */
export const INVITES_KEY = ["invites"] as const;

/** The invites, a page at a time. */
export function invitesQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: INVITES_KEY,
    queryFn: ({ pageParam }) => api.invites(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: InvitePage) => page.next,
  });
}

/** The invites, through the app's client. */
export function useInvites() {
  return useInfiniteQuery(invitesQuery(useApiClient()));
}

/** Where every account query's key starts, so one invalidation reaches them all. */
export const ACCOUNTS_KEY = ["accounts"] as const;

/** The accounts the user may see, every page loaded so far. */
export const ACCOUNT_LIST_KEY = ["accounts", "list"] as const;

/** The accounts the user may see, a page at a time, newest first. */
export function accountsQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: ACCOUNT_LIST_KEY,
    queryFn: ({ pageParam }) => api.accounts(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: AccountPage) => page.next,
  });
}

/** The accounts, through the app's client. */
export function useAccounts() {
  return useInfiniteQuery(accountsQuery(useApiClient()));
}

/** The links waiting for the user's approval: all of them, for the count. */
export const AWAITING_KEY = ["accounts", "awaiting"] as const;

/** A list's pages, read one after another, with the cursor each was asked with. */
interface EveryPage<P> {
  readonly pages: P[];
  readonly pageParams: (string | undefined)[];
}

/**
 * Every page `read` answers, newest first, until a page has no `next`. One failed page fails
 * the read, and so does a `next` that doesn't move past the cursor it was asked with (an ID's
 * text sorts by its time): the cursors only fall, so none is asked twice and a server bug
 * never loops (R99). Once `signal` aborts, no further page is asked for.
 */
async function everyPage<P extends { readonly next?: string }>(
  read: (before: string | undefined) => Promise<P>,
  signal: AbortSignal,
): Promise<EveryPage<P>> {
  const pages: P[] = [];
  const pageParams: (string | undefined)[] = [];
  let before: string | undefined;
  do {
    if (pages.length > 0) {
      signal.throwIfAborted();
    }
    const page = await read(before);
    pages.push(page);
    pageParams.push(before);
    const next = page.next;
    if (next !== undefined && before !== undefined && next >= before) {
      throw new ApiError({ code: "unexpected_response", message: PAGES_DONT_MOVE_ON });
    }
    before = next;
  } while (before !== undefined);
  return { pages, pageParams };
}

/** Every page of the links waiting for the user's approval; one failed page fails it all. */
async function everyAwaiting(api: ApiClient, signal: AbortSignal): Promise<AccountResponse[]> {
  const read = await everyPage((before) => api.accountsAwaitingApproval(before, PAGE_MAX), signal);
  return read.pages.flatMap((page) => page.items);
}

/**
 * The links waiting for the user's approval (S5 in ADR-0018's group F): read only for
 * Admins and the Owner (`enabled`), again when a screen that shows them mounts, after the
 * user's own account writes (`ACCOUNTS_KEY`), and when the window regains focus; no timer.
 */
export function awaitingQuery(api: ApiClient, enabled: boolean) {
  return queryOptions({
    queryKey: AWAITING_KEY,
    queryFn: ({ signal }) => everyAwaiting(api, signal),
    enabled,
    refetchOnWindowFocus: true,
  });
}

/** One account's key. */
export function accountKey(id: string) {
  return ["accounts", "one", id] as const;
}

/** One account, as the user sees it. */
export function accountQuery(api: ApiClient, id: string) {
  return queryOptions({ queryKey: accountKey(id), queryFn: () => api.account(id) });
}

/** One account, through the app's client. */
export function useAccount(id: string) {
  return useQuery(accountQuery(useApiClient(), id));
}

/** The key of who else has access to account `id`. */
export function grantsKey(id: string) {
  return ["accounts", "grants", id] as const;
}

/** Who else has access to account `id`, a page at a time. */
export function grantsQuery(api: ApiClient, id: string) {
  return infiniteQueryOptions({
    queryKey: grantsKey(id),
    queryFn: ({ pageParam }) => api.accountGrants(id, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: GrantPage) => page.next,
  });
}

/** Who else has access to account `id`, through the app's client. */
export function useGrants(id: string, enabled: boolean) {
  return useInfiniteQuery({ ...grantsQuery(useApiClient(), id), enabled });
}

// The bots', their chat's and the modes' keys (P11.8, P11.9; ADR-0020, S60, R90): the live
// events patch whatever these hold. Each is tagged with the data its query holds, so a query
// or a live patch of another shape doesn't compile.

/** `key`, tagged with the data `T` its query holds. */
function tagged<T>(): <K extends QueryKey>(key: K) => DataTag<K, T> {
  return <K extends QueryKey>(key: K) => key as DataTag<K, T>;
}

/** The bots the user may see, every page loaded so far. */
export const BOTS_LIST_KEY = tagged<InfiniteData<BotPage, string | undefined>>()([
  "bots",
  "list",
] as const);

/** One bot's key. */
export function botKey(id: string) {
  return tagged<BotResponse>()(["bots", "one", id] as const);
}

/** The key of one bot's chat, every page loaded so far. */
export function chatKey(id: string) {
  return tagged<InfiniteData<ChatPage, number | undefined>>()(["bots", "chat", id] as const);
}

/**
 * The key of one bot's chat outcomes (S70): what came of the lines sent, by chat request,
 * never text. Its query is never fetched; the live patches and the send box write it.
 */
export function chatOutcomesKey(id: string) {
  return tagged<ChatOutcomes>()(["bots", "chat-outcomes", id] as const);
}

/** The modes the user may see: every page, read at once (S68). */
export const MODES_LIST_KEY = tagged<InfiniteData<ModePage, string | undefined>>()([
  "modes",
  "list",
] as const);

/** The bots the user may see, a page at a time, newest first. */
export function botsQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: BOTS_LIST_KEY,
    queryFn: ({ pageParam }) => api.bots(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: BotPage) => page.next,
  });
}

/** The bots, through the app's client. */
export function useBots() {
  return useInfiniteQuery(botsQuery(useApiClient()));
}

/** One bot, as the user sees it. */
export function botQuery(api: ApiClient, id: string) {
  return queryOptions({ queryKey: botKey(id), queryFn: () => api.bot(id) });
}

/** One bot, through the app's client. */
export function useBot(id: string) {
  return useQuery(botQuery(useApiClient(), id));
}

/** One target of the audit log: its kind and its ID (ADR-0020, S89). */
export interface AuditFilter {
  readonly kind: string;
  readonly id: string;
}

/** Where every audit page's key starts. */
export const AUDIT_KEY = ["audit"] as const;

/** The key of the audit log's pages loaded so far, the whole log's or one target's. */
export function auditKey(filter?: AuditFilter) {
  return filter === undefined
    ? ([...AUDIT_KEY, "all"] as const)
    : ([...AUDIT_KEY, "target", filter.kind, filter.id] as const);
}

/** The audit log, or one target's entries, a page at a time, newest first. */
export function auditQuery(api: ApiClient, filter?: AuditFilter) {
  return infiniteQueryOptions({
    queryKey: auditKey(filter),
    queryFn: async ({ pageParam }) =>
      movedOn(await api.audit(pageParam, undefined, filter?.kind, filter?.id), pageParam),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (page: AuditPage) => page.next,
  });
}

/** The audit log, through the app's client. */
export function useAudit(filter?: AuditFilter) {
  return useInfiniteQuery(auditQuery(useApiClient(), filter));
}

/** The agents: every page loaded so far. */
export const AGENTS_KEY = ["agents"] as const;

/**
 * The agents, a page at a time, newest first. Nothing on the socket carries them, so they're read
 * again when the screen mounts, after its own writes and when the window regains focus (R177).
 */
export function agentsQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: AGENTS_KEY,
    queryFn: ({ pageParam }) => api.agents(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: AgentPage) => page.next,
    refetchOnWindowFocus: true,
  });
}

/** The agents, through the app's client. */
export function useAgents() {
  return useInfiniteQuery(agentsQuery(useApiClient()));
}

/** One bot's chat, a page at a time, newest first. */
export function chatQuery(api: ApiClient, id: string) {
  return infiniteQueryOptions({
    queryKey: chatKey(id),
    queryFn: async ({ pageParam }) => movedOn(await api.botChat(id, pageParam), pageParam),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (page: ChatPage) => page.next,
  });
}

/**
 * `page`, read with the cursor `before`, if its `next` moves past it (R120, R99's rule for
 * the chat and the audit log): otherwise the read fails, so a server bug never loops.
 */
function movedOn<P extends { readonly next?: number }>(page: P, before: number | undefined): P {
  if (page.next !== undefined && before !== undefined && page.next >= before) {
    throw new ApiError({ code: "unexpected_response", message: PAGES_DONT_MOVE_ON });
  }
  return page;
}

/** One bot's chat, through the app's client. */
export function useChat(id: string) {
  return useInfiniteQuery(chatQuery(useApiClient(), id));
}

/**
 * One bot's chat outcomes (S70, R121): empty until written, never fetched, kept while the
 * session lasts; the session's end empties the cache, and a `bot_removed` resets them.
 */
export function chatOutcomesQuery(id: string) {
  return queryOptions({
    queryKey: chatOutcomesKey(id),
    queryFn: skipToken,
    initialData: {} as ChatOutcomes,
    gcTime: Number.POSITIVE_INFINITY,
    staleTime: Number.POSITIVE_INFINITY,
  });
}

/** One bot's chat outcomes, as its screen holds them. */
export function useChatOutcomes(id: string): ChatOutcomes {
  return useQuery(chatOutcomesQuery(id)).data ?? NO_OUTCOMES;
}

/** No outcomes. */
const NO_OUTCOMES: ChatOutcomes = {};

/**
 * Links a send's `answer` into bot `id`'s outcomes, the send begun at loss count `since`;
 * only into outcomes a screen holds, so an answer after the session's end builds nothing.
 */
export function recordAnswer(
  queryClient: QueryClient,
  id: string,
  answer: ChatSendResponse,
  since: number,
): void {
  queryClient.setQueryData(chatOutcomesKey(id), (outcomes) =>
    outcomes === undefined ? undefined : answered(outcomes, answer, since),
  );
}

/**
 * Every bot the user may see, read through every page now, for an answer that must know them all
 * (ADR-0020, U66, R169). One failed page fails it all. Not a query: nothing here is kept, and the
 * bots' own list, a page at a time, stays as it is.
 */
export async function readEveryBot(api: ApiClient): Promise<BotResponse[]> {
  const read = await everyPage(
    (before) => api.bots(before, PAGE_MAX),
    new AbortController().signal,
  );
  return read.pages.flatMap((page) => page.items);
}

/** Every page of the modes the user may see, as one list's pages; one failed page fails it all. */
function everyMode(
  api: ApiClient,
  signal: AbortSignal,
): Promise<InfiniteData<ModePage, string | undefined>> {
  return everyPage((before) => api.modes(before, PAGE_MAX), signal);
}

/** Every mode the user may see (S68 in ADR-0020): the names the bots' screens show. */
export function modesQuery(api: ApiClient) {
  return queryOptions({
    queryKey: MODES_LIST_KEY,
    queryFn: ({ signal }) => everyMode(api, signal),
  });
}

/** Every mode, through the app's client. */
export function useModes() {
  return useQuery(modesQuery(useApiClient()));
}

/** The servers' settings: every page, read at once (ADR-0020, R136). */
export const SERVER_SETTINGS_KEY = ["server-settings"] as const;

/** Every page of the servers' settings, newest first; one failed page fails it all. */
async function everyServerSettings(
  api: ApiClient,
  signal: AbortSignal,
): Promise<ServerSettingsResponse[]> {
  const read = await everyPage(
    (before) => api.serverSettings(before, SERVER_SETTINGS_PAGE_MAX),
    signal,
  );
  return read.pages.flatMap((page) => page.items);
}

/**
 * Every server's settings (Admins and the Owner, S75): at most 100 rows, read 10 a page, so a
 * page of rows at their caps fits a client's answer cap. Nothing on the socket carries them,
 * so the screens read them again when they mount and after their own writes.
 */
export function serverSettingsQuery(api: ApiClient) {
  return queryOptions({
    queryKey: SERVER_SETTINGS_KEY,
    queryFn: ({ signal }) => everyServerSettings(api, signal),
  });
}

/** Every server's settings, through the app's client. */
export function useServerSettings() {
  return useQuery(serverSettingsQuery(useApiClient()));
}
