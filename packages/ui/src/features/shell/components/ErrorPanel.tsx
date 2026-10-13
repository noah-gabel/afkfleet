// The error screen (Plan.md P8.6, ADR-0017), shown by the app's error boundary and as the
// router's error component. A render error is a bug, so the screen says so plainly. An
// API error shows describeError's text; any other error only its name (TypeError, …),
// never its message or stack, which could quote data.
import { ApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { Button } from "../../../components/ui/button";
import { Card, CardContent, CardFooter, CardHeader } from "../../../components/ui/card";

/** Reloads the page, which starts the app again from its launch. */
function reload(): void {
  window.location.reload();
}

/** The error screen for `error`. */
export function ErrorPanel({
  error,
  onReset,
  onRestart = reload,
}: {
  error: unknown;
  /** "Try again": render what failed once more. */
  onReset: () => void;
  /** "Restart the app"; reloads the page unless replaced. */
  onRestart?: () => void;
}) {
  const detail =
    error instanceof ApiError
      ? describeError(error.error)
      : error instanceof Error
        ? error.name
        : "Error";
  return (
    <main className="flex flex-1 items-center justify-center p-8">
      <Card className="w-full max-w-md">
        <CardHeader>
          <h1 className="text-lg font-semibold">Something went wrong in the app.</h1>
        </CardHeader>
        <CardContent>
          <p className="text-muted-foreground">{detail}</p>
        </CardContent>
        <CardFooter className="gap-2">
          <Button type="button" onClick={onReset}>
            Try again
          </Button>
          <Button type="button" variant="outline" onClick={onRestart}>
            Restart the app
          </Button>
        </CardFooter>
      </Card>
    </main>
  );
}
