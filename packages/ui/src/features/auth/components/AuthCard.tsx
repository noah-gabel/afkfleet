// The frame of every sign-in step (Plan.md P8.7): a card with the step's heading, which
// takes the focus when the step appears, so a screen reader announces where the flow is.
import { type ReactNode, useEffect, useRef } from "react";

import { Card, CardContent, CardHeader } from "../../../components/ui/card";

/** A step's card titled `title`. */
export function AuthCard({
  title,
  intro,
  children,
}: {
  title: string;
  intro?: ReactNode;
  children: ReactNode;
}) {
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    heading.current?.focus();
  }, []);
  return (
    <main className="flex flex-1 items-center justify-center p-8">
      <Card className="w-full max-w-md">
        <CardHeader>
          <h1 ref={heading} tabIndex={-1} className="text-xl font-semibold outline-none">
            {title}
          </h1>
          {intro === undefined ? null : (
            <div className="text-sm text-muted-foreground">{intro}</div>
          )}
        </CardHeader>
        <CardContent className="flex flex-col gap-4">{children}</CardContent>
      </Card>
    </main>
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
