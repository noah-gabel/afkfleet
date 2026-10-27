// Home's notice lines (Plan.md P11.9; ADR-0020, U35, U42): one compact line per notice of the
// live record, with Open to its bot's screen and Dismiss, until dismissed or until that
// screen is opened.
import { Link } from "@tanstack/react-router";

import { useRecord } from "../../../app/context";
import { NoticeLine } from "../../../components/NoticeLine";
import { Button } from "../../../components/ui/button";
import { noticeLine, OPEN } from "../texts";

/** Home's notice lines. */
export function BotNotices() {
  const notices = useRecord((state) => state.record.notices);
  const dismiss = useRecord((state) => state.dismiss);
  return notices.map((notice) => (
    <NoticeLine
      key={notice.id}
      action={
        <Button asChild size="xs" variant="outline">
          <Link to="/bots/$id" params={{ id: notice.botId }}>
            {OPEN}
          </Link>
        </Button>
      }
      onDismiss={() => {
        dismiss(notice.id);
      }}
    >
      {noticeLine(notice)}
    </NoticeLine>
  ));
}
