// The router's error component (Plan.md P8.6): an error thrown while loading or rendering
// a route shows the error screen; "Try again" resets it and loads the route again.
import { type ErrorComponentProps, useRouter } from "@tanstack/react-router";

import { ErrorPanel } from "./ErrorPanel";

/** The error screen for a route's error. */
export function RouteError({ error, reset }: ErrorComponentProps) {
  const router = useRouter();
  return (
    <ErrorPanel
      error={error}
      onReset={() => {
        reset();
        void router.invalidate();
      }}
    />
  );
}
