// The notices' toasts (Plan.md P11.9; ADR-0020, U42, R110): one toast per notice as it comes,
// on any screen, with Open to its bot's screen. It reads the live record's toasts, so the live
// layer never reaches the toaster; the toasts from before it mounted aren't shown again.
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useRef } from "react";
import { toast } from "sonner";

import { useRecord } from "../../../app/context";
import { noticeToast, OPEN } from "../texts";

/** Toasts each new notice of the live record. */
export function LiveNotices() {
  const toasts = useRecord((state) => state.record.toasts);
  const navigate = useNavigate();
  /** The last toast shown: the record's IDs only grow, across its clears too. */
  const shown = useRef<number | null>(null);

  useEffect(() => {
    const latest = toasts.at(-1)?.id ?? 0;
    const since = shown.current ?? latest;
    for (const notice of toasts) {
      if (notice.id > since) {
        const show = notice.kind === "failed" ? toast.error : toast.warning;
        show(noticeToast(notice), {
          action: {
            label: OPEN,
            onClick: () => {
              void navigate({ to: "/bots/$id", params: { id: notice.botId } });
            },
          },
        });
      }
    }
    shown.current = Math.max(since, latest);
  }, [toasts, navigate]);

  return null;
}
