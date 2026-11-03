// The audit log's screen (Plan.md P11.9, P11.11; ADR-0020, U78, U80, U81, U122, S47, S52, S85, S89,
// S132, R173–R175), for Admins and the Owner: every entry a page at a time, newest first, or one
// target's. An entry's time, who did it, what, on what, how it ended, from which address, and its
// details, all plain text (R174). Entries never wait for the names (S85).
//
// Every column matters here, so in a small box the table stacks each entry into a labelled block
// rather than dropping any (U122): one <table>, which CSS turns into blocks below a 784 px box,
// its heads only for screen readers then, and each cell a label and its value. The label is real
// text, so the sweep measures it, and aria-hidden, as the head names the cell; a cell is never
// hidden, so every entry keeps its seven cells in order and each its head (S132). The labels'
// column is as wide as the widest, DETAILS, with room, so values line up from entry to entry
// whichever labels each has. 784 px of box is under the box beside the wide sidebar, so the
// window's width alone decides the form (R228).
//
// Nothing here carries a secret, and the server built each entry the same for every viewer (R65):
// reads are queries, kept in memory until the session's end empties the cache (S87).
import { Link } from "@tanstack/react-router";
import { type ReactNode, useMemo } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { type AuditFilter, useAudit, useUserNames } from "../../../api/queries";
import { ScreenState } from "../../../components/ScreenState";
import { Button } from "../../../components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../../../components/ui/table";
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
  LOADING_AUDIT,
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

/** The stacked form's classes, below a 784 px box, written out whole for Tailwind (S132). */
const STACK = {
  table: "@max-[48.875rem]/table:block",
  head: "@max-[48.875rem]/table:sr-only",
  body: "@max-[48.875rem]/table:block",
  row: "@max-[48.875rem]/table:grid @max-[48.875rem]/table:grid-cols-[4.5rem_minmax(0,1fr)] @max-[48.875rem]/table:gap-x-3 @max-[48.875rem]/table:py-2",
  cell: "@max-[48.875rem]/table:col-span-2 @max-[48.875rem]/table:grid @max-[48.875rem]/table:grid-cols-subgrid @max-[48.875rem]/table:py-0.5",
  label:
    "hidden font-mono text-[0.6875rem] font-semibold tracking-wider text-muted-foreground uppercase @max-[48.875rem]/table:block",
};

/** The users' names by ID, as far as they could be read. */
type Names = ReadonlyMap<string, string>;

/** A cell of `column`, its label shown only when stacked, and none when it holds nothing. */
function Cell({ column, children }: { column: number; children: ReactNode }) {
  const empty = children === null || children === undefined || children === "";
  return (
    <TableCell className={`align-top ${STACK.cell}`}>
      {empty ? null : (
        <span data-slot="cell-label" aria-hidden="true" className={STACK.label}>
          {COLUMNS[column]}
        </span>
      )}
      {empty ? null : <div className="min-w-0 wrap-anywhere">{children}</div>}
    </TableCell>
  );
}

/** A user's ID: their name with the whole ID on hover and for screen readers, or the ID (U80). */
function UserName({ id, names }: { id: string; names: Names }) {
  const name = names.get(id);
  if (name === undefined) {
    return <span className="font-mono wrap-anywhere">{id}</span>;
  }
  return (
    <span title={id} className="font-mono wrap-anywhere">
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
        <span className="font-mono wrap-anywhere">{target.id}</span>
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
  const lines = detailLines(entry);
  if (lines.length === 0) {
    return null;
  }
  return (
    <ul className="flex flex-col gap-0.5">
      {lines.map((line) => (
        <li key={`${line.label}:${line.value}`} className="wrap-anywhere">
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
  const on = <On entry={entry} names={names} filter={filter} />;
  const details = <Details entry={entry} names={names} />;
  return (
    <TableRow className={STACK.row}>
      <Cell column={0}>{formatTime(entry.at)}</Cell>
      <Cell column={1}>
        <Who entry={entry} names={names} />
      </Cell>
      <Cell column={2}>
        <span className="wrap-anywhere">{actionLabel(entry.action)}</span>
      </Cell>
      <Cell column={3}>{entry.target === undefined ? null : on}</Cell>
      <Cell column={4}>{outcomeLabel(entry.outcome)}</Cell>
      <Cell column={5}>
        {entry.ip === undefined ? null : <span className="font-mono">{entry.ip}</span>}
      </Cell>
      <Cell column={6}>{detailLines(entry).length === 0 ? null : details}</Cell>
    </TableRow>
  );
}

/** The line that says the list is narrowed to `filter`, with the way back. */
function FilterLine({ filter, names }: { filter: AuditFilter; names: Names }) {
  return (
    <div className="flex flex-col gap-1 text-sm">
      <p className="wrap-anywhere">
        {showingOnly(filter.kind as AuditTargetKind)}{" "}
        {filter.kind === "user" ? <UserName id={filter.id} names={names} /> : filter.id}.
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
    <main className="flex flex-1 flex-col gap-4 p-4">
      <h1 className="text-lg font-semibold tracking-tight">{AUDIT_HEADING}</h1>
      <p className="text-sm text-muted-foreground">{AUDIT_INTRO}</p>
      {filter === undefined ? null : <FilterLine filter={filter} names={names} />}
      {users.isError ? <p className="text-sm text-muted-foreground">{NAMES_FAILED}</p> : null}
      {audit.isError ? (
        <ScreenState kind="error">
          {auditFailed(describeError(toApiError(audit.error).error))}
        </ScreenState>
      ) : null}
      {audit.isPending ? <ScreenState kind="loading">{LOADING_AUDIT}</ScreenState> : null}
      {audit.isSuccess && rows.length === 0 ? (
        <ScreenState kind="empty">{filter === undefined ? AUDIT_EMPTY : FILTER_EMPTY}</ScreenState>
      ) : null}
      {rows.length === 0 ? null : (
        <Table aria-label={AUDIT_HEADING} className={STACK.table}>
          <TableHeader className={STACK.head}>
            <TableRow>
              {COLUMNS.map((column) => (
                <TableHead key={column}>{column}</TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody className={STACK.body}>
            {rows.map((row) =>
              row.kind === "entry" ? (
                <EntryRow key={row.id} entry={row.entry} names={names} filter={filter} />
              ) : (
                <TableRow key={row.id} className={STACK.row}>
                  <TableCell
                    colSpan={COLUMNS.length}
                    className={`italic text-destructive ${STACK.cell}`}
                  >
                    <div className="w-0 min-w-full wrap-anywhere @max-[48.875rem]/table:col-span-2">
                      {unreadableEntry(row.id)}
                    </div>
                  </TableCell>
                </TableRow>
              ),
            )}
          </TableBody>
        </Table>
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
          onClick={() => {
            void audit.refetch();
            void users.refetch();
          }}
        >
          {LOAD_AGAIN}
        </Button>
      </div>
    </main>
  );
}
