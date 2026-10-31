// The audit log's screen (Plan.md P11.9; ADR-0020, U78, U80, U81, S47, S52, S85, S89,
// R173–R175), for Admins and the Owner: every entry a page at a time, newest first, or one
// target's. An entry's time, who did it, what, on what, how it ended, from which address, and
// its details, all plain text (R174). Entries never wait for the names (S85).
//
// Nothing here carries a secret, and the server built each entry the same for every viewer (R65):
// reads are queries, kept in memory until the session's end empties the cache (S87).
import { Link } from "@tanstack/react-router";
import { useMemo } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { type AuditFilter, useAudit, useUserNames } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import type { AuditEntryResponse } from "../../../generated/AuditEntryResponse";
import type { AuditTargetKind } from "../../../generated/AuditTargetKind";
import { formatTime } from "../../../lib/time";
import { auditRows, detailLines } from "../rows";
import {
  AUDIT_EMPTY,
  AUDIT_HEADING,
  AUDIT_INTRO,
  actionLabel,
  auditFailed,
  COLUMNS,
  FILTER_EMPTY,
  FILTER_ON_USER,
  LOAD_AGAIN,
  NAMES_FAILED,
  NO_USER,
  ONLY_THIS_TARGET,
  onlyTarget,
  outcomeLabel,
  SHOW_EVERYTHING,
  SHOW_MORE,
  showingOnly,
  THE_SERVER,
  targetKindLabel,
  unreadableEntry,
} from "../texts";

/** The users' names by ID, as far as they could be read. */
type Names = ReadonlyMap<string, string>;

/** A user's ID: their name with the whole ID on hover and for screen readers, or the ID (U80). */
function UserName({ id, names }: { id: string; names: Names }) {
  const name = names.get(id);
  if (name === undefined) {
    return <span className="break-all">{id}</span>;
  }
  return (
    <span title={id}>
      {name}
      <span className="sr-only"> ({id})</span>
    </span>
  );
}

/** Who an entry names: the server's own has no name at all (ADR-0020, R69). */
function Who({ entry, names }: { entry: AuditEntryResponse; names: Names }) {
  if (entry.actor !== undefined) {
    return <UserName id={entry.actor} names={names} />;
  }
  return entry.action === "retention.purge" ? (
    <span className="text-muted-foreground italic">{THE_SERVER}</span>
  ) : (
    <span className="text-muted-foreground">{NO_USER}</span>
  );
}

/** How a target reads in a link's name or the filter's line. */
function targetWho(kind: AuditTargetKind, id: string, names: Names): string {
  return kind === "user" ? (names.get(id) ?? id) : id;
}

/** What an entry touched, with its way to that target's entries unless they're what's shown. */
function On({
  entry,
  names,
  filter,
}: {
  entry: AuditEntryResponse;
  names: Names;
  filter: AuditFilter | undefined;
}) {
  const target = entry.target;
  if (target === undefined) {
    return null;
  }
  const shown = filter?.kind === target.kind && filter.id === target.id;
  return (
    <div className="flex flex-col items-start gap-1">
      <span>{targetKindLabel(target.kind)}</span>
      {target.kind === "user" ? (
        <UserName id={target.id} names={names} />
      ) : (
        <span className="break-all">{target.id}</span>
      )}
      {shown ? null : (
        <Button asChild size="sm" variant="link" className="h-auto p-0">
          <Link
            to="/admin/audit"
            search={{ kind: target.kind, id: target.id }}
            aria-label={onlyTarget(target.kind, targetWho(target.kind, target.id, names))}
          >
            {ONLY_THIS_TARGET}
          </Link>
        </Button>
      )}
    </div>
  );
}

/** An entry's details, one line each, plain text however long or strange (R174). */
function Details({ entry, names }: { entry: AuditEntryResponse; names: Names }) {
  return (
    <ul className="flex flex-col gap-0.5">
      {detailLines(entry).map((line) => (
        <li key={`${line.label}:${line.value}`} className="max-w-full overflow-hidden break-words">
          {line.user === undefined ? (
            line.label === "" ? (
              line.value
            ) : (
              `${line.label}: ${line.value}`
            )
          ) : (
            <>
              {line.label}: <UserName id={line.user} names={names} />
            </>
          )}
        </li>
      ))}
    </ul>
  );
}

/** One entry's row. */
function EntryRow({
  entry,
  names,
  filter,
}: {
  entry: AuditEntryResponse;
  names: Names;
  filter: AuditFilter | undefined;
}) {
  return (
    <tr className="border-t align-top">
      <td className="py-2 pr-3">{formatTime(entry.at)}</td>
      <td className="py-2 pr-3">
        <Who entry={entry} names={names} />
      </td>
      <td className="py-2 pr-3">{actionLabel(entry.action)}</td>
      <td className="py-2 pr-3">
        <On entry={entry} names={names} filter={filter} />
      </td>
      <td className="py-2 pr-3">{outcomeLabel(entry.outcome)}</td>
      <td className="py-2 pr-3">{entry.ip ?? ""}</td>
      <td className="py-2">
        <Details entry={entry} names={names} />
      </td>
    </tr>
  );
}

/** The line that says the list is narrowed to `filter`, with the way back. */
function FilterLine({ filter, names }: { filter: AuditFilter; names: Names }) {
  return (
    <div className="flex flex-col gap-1 text-sm">
      <p className="break-all">
        {showingOnly(
          filter.kind as AuditTargetKind,
          targetWho(filter.kind as AuditTargetKind, filter.id, names),
        )}
      </p>
      {filter.kind === "user" ? <p className="text-muted-foreground">{FILTER_ON_USER}</p> : null}
      <Button asChild variant="link" className="h-auto self-start p-0">
        <Link to="/admin/audit" search={{}}>
          {SHOW_EVERYTHING}
        </Link>
      </Button>
    </div>
  );
}

/** `/admin/audit`: the whole log, or `filter`'s target. */
export function AuditScreen({ filter }: { filter?: AuditFilter }) {
  const audit = useAudit(filter);
  const users = useUserNames();
  const names: Names = useMemo(
    () => new Map((users.data ?? []).map((user) => [user.id, user.username])),
    [users.data],
  );
  const rows = useMemo(() => auditRows(audit.data?.pages ?? []), [audit.data]);
  return (
    <main className="flex flex-col gap-4 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">{AUDIT_HEADING}</h1>
      <p className="text-sm text-muted-foreground">{AUDIT_INTRO}</p>
      {filter === undefined ? null : <FilterLine filter={filter} names={names} />}
      {users.isError ? <p className="text-sm text-muted-foreground">{NAMES_FAILED}</p> : null}
      {audit.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {auditFailed(describeError(toApiError(audit.error).error))}
        </p>
      ) : null}
      {audit.isSuccess && rows.length === 0 ? (
        <p>{filter === undefined ? AUDIT_EMPTY : FILTER_EMPTY}</p>
      ) : (
        <table aria-label={AUDIT_HEADING} className="w-full text-left text-sm">
          <thead className="text-muted-foreground">
            <tr>
              {COLUMNS.map((column) => (
                <th key={column} className="py-1 pr-3 font-medium">
                  {column}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) =>
              row.kind === "entry" ? (
                <EntryRow key={row.id} entry={row.entry} names={names} filter={filter} />
              ) : (
                <tr key={row.id} className="border-t">
                  <td colSpan={COLUMNS.length} className="py-2 italic text-destructive">
                    {unreadableEntry(row.id)}
                  </td>
                </tr>
              ),
            )}
          </tbody>
        </table>
      )}
      <div className="flex gap-2">
        {audit.hasNextPage ? (
          <Button
            type="button"
            variant="outline"
            disabled={audit.isFetchingNextPage}
            onClick={() => void audit.fetchNextPage()}
          >
            {SHOW_MORE}
          </Button>
        ) : null}
        <Button
          type="button"
          variant="outline"
          disabled={audit.isFetching}
          onClick={() => void audit.refetch()}
        >
          {LOAD_AGAIN}
        </Button>
      </div>
    </main>
  );
}
