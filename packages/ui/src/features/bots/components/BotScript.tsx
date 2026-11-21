// A bot's script on its screen (Plan.md P15.26; ADR-0044, E9, E10; ADR-0045): where it stands, what
// stopped or refused it, with its hook and its place and a link to that line of its mode, and its
// log, newest last. All of it arrives on the live events and lives in the app for the session
// (api/scriptReports.ts): the server stores none of it, so a status from before the app connected
// shows only once the script says it again, at the bot's next join. Every text is the agent's,
// cleaned twice on its way, and shows as plain text.
//
// The card shows for a bot on a scripted mode, and for any bot its script reported on.
import { Link } from "@tanstack/react-router";
import { useId } from "react";

import { useBotScript } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
import type { BotResponse } from "../../../generated/BotResponse";
import type { ModeResponse } from "../../../generated/ModeResponse";
import type { ScriptStatus } from "../../../generated/ScriptStatus";
import {
  inHook,
  SCRIPT_HEADING,
  SCRIPT_LOG_EMPTY,
  SCRIPT_LOG_HEADING,
  SCRIPT_LOG_KIND_TEXT,
  SCRIPT_NO_STATUS,
  SCRIPT_PROBLEM_TEXT,
  SCRIPT_STATE_TEXT,
  SHOW_IN_SCRIPT,
  scriptPlace,
} from "../texts";

/** `value`'s text in `texts`, or the value itself for one this version doesn't know. */
function textOf<K extends string>(texts: Readonly<Record<K, string>>, value: string): string {
  return Object.hasOwn(texts, value) ? texts[value as K] : value;
}

/** Where a problem is: its hook and its place, as far as each is known. */
function whereText(hook: string | undefined, line: number | undefined, column: number | undefined) {
  const place = line === undefined ? undefined : scriptPlace(line, column);
  if (hook === undefined) {
    return place;
  }
  return place === undefined ? inHook(hook) : `${inHook(hook)}, ${place.toLowerCase()}`;
}

/** The status, and what stopped or refused the script, with the link to its line. */
function StatusPart({
  status,
  modeId,
  linked,
}: {
  status: ScriptStatus;
  modeId: string;
  linked: boolean;
}) {
  const problem = status.problem;
  const where =
    problem === undefined ? undefined : whereText(problem.hook, problem.line, problem.column);
  return (
    <div className="flex flex-col gap-1">
      <p className="text-sm font-semibold">{textOf(SCRIPT_STATE_TEXT, status.state)}</p>
      {problem === undefined ? null : (
        <>
          <p className="text-sm">{textOf(SCRIPT_PROBLEM_TEXT, problem.kind)}</p>
          {where === undefined ? null : <p className="text-sm text-muted-foreground">{where}</p>}
          <p className="font-mono text-sm break-words">{problem.message}</p>
          {linked && problem.line !== undefined ? (
            <Button asChild variant="link" size="sm" className="self-start px-0">
              <Link
                to="/modes/$id"
                params={{ id: modeId }}
                search={{
                  line: problem.line,
                  ...(problem.column === undefined ? {} : { column: problem.column }),
                }}
              >
                {SHOW_IN_SCRIPT}
              </Link>
            </Button>
          ) : null}
        </>
      )}
    </div>
  );
}

/** `bot`'s script card, `mode` its mode when the viewer may see it; nothing for a step mode's bot. */
export function BotScript({ bot, mode }: { bot: BotResponse; mode: ModeResponse | undefined }) {
  const record = useBotScript(bot.id);
  const headingId = useId();
  const logId = useId();
  if (mode?.script === undefined && record.status === null && record.log.length === 0) {
    return null;
  }
  return (
    <section aria-labelledby={headingId}>
      <Card className="gap-3 p-3">
        <h2 id={headingId} className="text-base font-semibold">
          {SCRIPT_HEADING}
        </h2>
        {record.status === null ? (
          <p className="text-sm text-muted-foreground">{SCRIPT_NO_STATUS}</p>
        ) : (
          <StatusPart status={record.status} modeId={bot.mode_id} linked={mode !== undefined} />
        )}
        <h3 id={logId} className="text-sm font-semibold">
          {SCRIPT_LOG_HEADING}
        </h3>
        {record.log.length === 0 ? (
          <p className="text-sm text-muted-foreground">{SCRIPT_LOG_EMPTY}</p>
        ) : (
          <ul aria-labelledby={logId} className="flex flex-col gap-1 font-mono text-sm">
            {record.log.map((line) => (
              <li key={line.id} className="flex gap-2">
                <span className="shrink-0 text-muted-foreground">
                  {textOf(SCRIPT_LOG_KIND_TEXT, line.kind)}
                </span>
                <span className="min-w-0 break-words">{line.text}</span>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </section>
  );
}
