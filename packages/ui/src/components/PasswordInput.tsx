// A password field with a "show" toggle (Plan.md P8.7): the login and register forms, and
// group E's step-up and password change. The toggle is a plain button (it never submits)
// whose `aria-pressed` says whether the password shows.
import { EyeIcon, EyeOffIcon } from "lucide-react";
import { type ComponentProps, useState } from "react";

import { Input } from "./ui/input";

/** A password input; `toggleLabel` names its toggle ("Show password"). */
export function PasswordInput({
  toggleLabel,
  className,
  ...props
}: Omit<ComponentProps<typeof Input>, "type"> & { toggleLabel: string }) {
  const [shown, setShown] = useState(false);
  return (
    <div className="relative">
      <Input {...props} type={shown ? "text" : "password"} className={`pr-9 ${className ?? ""}`} />
      <button
        type="button"
        aria-label={toggleLabel}
        aria-pressed={shown}
        onClick={() => setShown((value) => !value)}
        className="absolute inset-y-0 right-0 flex w-8 items-center justify-center rounded-r-lg text-muted-foreground hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none"
      >
        {shown ? (
          <EyeOffIcon className="size-4" aria-hidden />
        ) : (
          <EyeIcon className="size-4" aria-hidden />
        )}
      </button>
    </div>
  );
}
