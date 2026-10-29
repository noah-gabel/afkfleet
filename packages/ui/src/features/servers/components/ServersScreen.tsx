// The Minecraft servers' list (Plan.md P11.9; ADR-0020, U57, R136): "Add a server", then
// every server's settings, newest first, each as a card that opens its own screen, with its
// server's key and how many conflict texts it holds. It lists only the rows that exist, never
// servers taken from bots, which would show where others' bots play (S75). Every page is read
// at once: at most 100 rows, 10 a page.
import { Link } from "@tanstack/react-router";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useMe, useServerSettings } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { Card, CardContent, CardHeader } from "../../../components/ui/card";
import type { ServerSettingsResponse } from "../../../generated/ServerSettingsResponse";
import { serverSettingsAccess } from "../../admin/permissions";
import {
  ADD_SERVER,
  LOADING_SERVERS,
  NO_SERVERS,
  openServer,
  SERVERS_HEADING,
  SERVERS_INTRO,
  serversFailed,
  settingsCount,
} from "../texts";

/** Every server's settings. */
export function ServersScreen() {
  const query = useServerSettings();
  const { data: user } = useMe();
  const configure = user !== undefined && serverSettingsAccess(user.role).configure;
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <h1 className="text-2xl font-semibold tracking-tight">{SERVERS_HEADING}</h1>
        {configure ? (
          <Button asChild>
            <Link to="/admin/servers/new">{ADD_SERVER}</Link>
          </Button>
        ) : null}
      </div>
      <p className="text-muted-foreground">{SERVERS_INTRO}</p>
      <Body query={query} />
    </main>
  );
}

/** The list's state: failed, loading, empty, or the cards. */
function Body({ query }: { query: ReturnType<typeof useServerSettings> }) {
  if (query.error !== null && query.data === undefined) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {serversFailed(describeError(toApiError(query.error).error))}
      </p>
    );
  }
  if (query.data === undefined) {
    return <p className="text-sm text-muted-foreground">{LOADING_SERVERS}</p>;
  }
  if (query.data.length === 0) {
    return <p>{NO_SERVERS}</p>;
  }
  return (
    <ul aria-label={SERVERS_HEADING} className="flex flex-col gap-3">
      {query.data.map((row) => (
        <li key={row.id}>
          <ServerCard row={row} />
        </li>
      ))}
    </ul>
  );
}

/** One server's card: the whole card is its link, as a bot's on Home is. */
function ServerCard({ row }: { row: ServerSettingsResponse }) {
  return (
    <Card className="relative gap-1 py-4 transition-colors hover:bg-muted/40">
      <CardHeader>
        <h2 className="text-lg font-semibold">
          <Link
            to="/admin/servers/$id"
            params={{ id: row.id }}
            aria-label={openServer(row.server)}
            className="after:absolute after:inset-0 after:content-['']"
          >
            {row.server}
          </Link>
        </h2>
      </CardHeader>
      <CardContent>
        <p className="text-sm text-muted-foreground">
          {settingsCount(row.conflict_texts.length, row.chat_formats.length)}
        </p>
      </CardContent>
    </Card>
  );
}
