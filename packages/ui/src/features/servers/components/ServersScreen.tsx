// The Minecraft servers' list (Plan.md P11.9, P11.11; ADR-0020, U57, U119, R136): "Add server" at
// the top right for whoever may configure, then every server's settings, newest first, as a grid of
// cards, as Home's: each card opens its own screen and shows its server's key and how many conflict
// texts and chat formats it holds, only the parts it has. It lists only the rows that exist, never
// servers taken from bots, which would show where others' bots play (S75). Every page is read at
// once: at most 100 rows, 10 a page.
import { Link } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useMe, useServerSettings } from "../../../api/queries";
import { Facts } from "../../../components/Facts";
import { CARD_GRID, CARD_NAME, LINKED_CARD, STRETCHED_LINK } from "../../../components/linkedCard";
import { ScreenState } from "../../../components/ScreenState";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
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
  settingsFacts,
} from "../texts";

/** The way to a new server's settings, with its icon. */
function AddServerLink() {
  return (
    <Button asChild size="sm">
      <Link to="/admin/servers/new">
        <PlusIcon aria-hidden="true" />
        {ADD_SERVER}
      </Link>
    </Button>
  );
}

/** Every server's settings. */
export function ServersScreen() {
  const query = useServerSettings();
  const { data: user } = useMe();
  const configure = user !== undefined && serverSettingsAccess(user.role).configure;
  return (
    <main className="flex flex-1 flex-col gap-4 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-lg font-semibold tracking-tight">{SERVERS_HEADING}</h1>
        {/* Hidden while loading, and while the empty state offers its own (as U108). */}
        {configure && query.data !== undefined && query.data.length > 0 ? <AddServerLink /> : null}
      </div>
      <p className="text-sm text-muted-foreground">{SERVERS_INTRO}</p>
      <Body query={query} configure={configure} />
    </main>
  );
}

/** The list's state: failed, loading, empty, or the cards. */
function Body({
  query,
  configure,
}: {
  query: ReturnType<typeof useServerSettings>;
  configure: boolean;
}) {
  if (query.error !== null && query.data === undefined) {
    return (
      <ScreenState kind="error">
        {serversFailed(describeError(toApiError(query.error).error))}
      </ScreenState>
    );
  }
  if (query.data === undefined) {
    return <ScreenState kind="loading">{LOADING_SERVERS}</ScreenState>;
  }
  if (query.data.length === 0) {
    return (
      <ScreenState kind="empty" action={configure ? <AddServerLink /> : undefined}>
        {NO_SERVERS}
      </ScreenState>
    );
  }
  return (
    <ul aria-label={SERVERS_HEADING} className={CARD_GRID}>
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
    <Card className={LINKED_CARD}>
      <h2 className={CARD_NAME}>
        <Link
          to="/admin/servers/$id"
          params={{ id: row.id }}
          aria-label={openServer(row.server)}
          className={STRETCHED_LINK}
        >
          {row.server}
        </Link>
      </h2>
      <Facts
        facts={settingsFacts(row.conflict_texts.length, row.chat_formats.length)}
        place="card"
      />
    </Card>
  );
}
