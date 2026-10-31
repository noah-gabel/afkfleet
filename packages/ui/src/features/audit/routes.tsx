// The audit log's route (Plan.md P11.9; ADR-0020, U78, U81, R175), under the admin layout, whose
// `requireAdmin` lets in exactly who may read it (admin/permissions.ts). The filter is the
// route's search, so a link can open the log narrowed to one target: a kind and an ID, both or
// neither, and anything else is the full log. The server stays the authority on both.
import { createRoute } from "@tanstack/react-router";

import { auditTargetId, auditTargetKind } from "../../api/schemas";
import { adminRoute } from "../admin/routes";
import { AuditScreen } from "./components/AuditScreen";

/** The audit route's search: its target, or nothing. */
export type AuditSearch = { readonly kind: string; readonly id: string } | Record<string, never>;

/** The search `search` asks for; see the module comment. */
export function auditSearch(search: Record<string, unknown>): AuditSearch {
  const kind = auditTargetKind.safeParse(search.kind);
  const id = auditTargetId.safeParse(search.id);
  return kind.success && id.success ? { kind: kind.data, id: id.data } : {};
}

/** `/admin/audit`. */
export const auditRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "audit",
  validateSearch: auditSearch,
  component: function AuditRoute() {
    // The router keeps a search's keys its `validateSearch` left out, so a malformed target would
    // still come through `useSearch`: only what passes the check here narrows the log.
    const search = auditSearch(auditRoute.useSearch());
    return search.kind === undefined || search.id === undefined ? (
      <AuditScreen />
    ) : (
      <AuditScreen filter={{ kind: search.kind, id: search.id }} />
    );
  },
});
