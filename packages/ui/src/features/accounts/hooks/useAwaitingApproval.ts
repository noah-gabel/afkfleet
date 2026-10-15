// The links waiting for the signed-in user's approval (Plan.md P9.8, S5 in ADR-0018's
// group F): one query behind the nav's badge, Home's alert and the Accounts screen's waiting
// section. Only Admins and the Owner read it, through the nav's own gate; Members never send
// the request.
import { useQuery } from "@tanstack/react-query";

import { useApiClient } from "../../../api/context";
import { awaitingQuery, useMe } from "../../../api/queries";
import { privilegeOf } from "../../shell/nav";

/** Whether the user reviews links, and the links waiting for them. */
export function useAwaitingApproval() {
  const { data: user } = useMe();
  const reviewer = user !== undefined && privilegeOf(user.role) === "admin";
  const query = useQuery(awaitingQuery(useApiClient(), reviewer));
  return { reviewer, query };
}
