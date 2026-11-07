// The frame of every sign-in step (Plan.md P8.7, P11.11): a card with the step's heading, which
// takes the focus when the step appears, so a screen reader announces where the flow is. A step whose
// control should take the focus instead says so. The sign-in steps show the brand above the card; the
// same card inside the signed-in frame, on Profile's flows and the link screen, doesn't need it. A
// footer sits at the bottom of the screen, below the card: the login screen's app version (U37).
import { type ReactNode, useEffect, useRef } from "react";

import { Logo, Wordmark } from "../../../components/Brand";
import { Card, CardContent, CardHeader } from "../../../components/ui/card";

/** A step's card titled `title`, with `footer` at the bottom of the screen below it. */
export function AuthCard({
  title,
  intro,
  brand = false,
  autoFocusHeading = true,
  footer,
  children,
}: {
  title: string;
  intro?: ReactNode;
  brand?: boolean;
  autoFocusHeading?: boolean;
  footer?: ReactNode;
  children: ReactNode;
}) {
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    if (autoFocusHeading) {
      heading.current?.focus();
    }
  }, [autoFocusHeading]);
  return (
    <main className="flex flex-1 flex-col items-center p-6">
      <div className="flex w-full flex-1 flex-col items-center justify-center gap-4">
        {brand ? <Brand /> : null}
        <Card className="w-full max-w-md">
          <CardHeader>
            <h1
              ref={heading}
              tabIndex={-1}
              className="text-lg font-semibold tracking-tight outline-none"
            >
              {title}
            </h1>
            {intro === undefined ? null : (
              <div className="text-sm text-muted-foreground">{intro}</div>
            )}
          </CardHeader>
          <CardContent className="flex flex-col gap-3">{children}</CardContent>
        </Card>
      </div>
      {footer}
    </main>
  );
}

/** The logo and the wordmark, above a sign-in step. */
export function Brand() {
  return (
    <div className="flex items-center gap-2 text-lg tracking-tight">
      <Logo size={28} />
      <span>
        <Wordmark />
      </span>
    </div>
  );
}

/** Where a server's address reads as data, in an intro. */
export function ServerLine({ origin }: { origin: string }) {
  return (
    <span>
      Server: <span className="font-mono">{origin}</span>
    </span>
  );
}

/** A form-level error, read out when it appears. */
export function FormError({ message }: { message: string | undefined }) {
  if (message === undefined) {
    return null;
  }
  return (
    <p role="alert" className="text-sm text-destructive">
      {message}
    </p>
  );
}
