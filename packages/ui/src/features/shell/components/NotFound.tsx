// What a route that doesn't exist, or that the user's role may not see, shows (Plan.md
// P8.6): the same answer for both, like the server's 404.
import { Link } from "@tanstack/react-router";

/** The not-found screen. */
export function NotFound() {
  return (
    <main className="flex flex-1 flex-col items-center justify-center gap-3 p-8">
      <p>This page doesn't exist or isn't available to you.</p>
      <Link to="/home" className="text-sm font-medium underline underline-offset-4">
        Go to Home
      </Link>
    </main>
  );
}
