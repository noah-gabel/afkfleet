// The recovery codes (Plan.md P8.7, ADR-0016): the 10 codes `mfa_confirm` answered, shown
// once, as selectable text, with "Copy all". Continuing needs the "I've saved these codes"
// box; the sign-in flow then logs in again, since the code that confirmed the
// authenticator can't log in, and the profile's replacement finishes. The codes live only
// in the flow's state, dropped when this step ends.
import { useState } from "react";

import { Button } from "../../../components/ui/button";
import { copyWithToast } from "../../../lib/clipboard";
import { AuthCard } from "./AuthCard";

/** The toast after "Copy all". */
export const COPIED =
  "Recovery codes copied. Paste them into a password manager or another safe place; Windows' clipboard history may keep a copy.";
/** The toast when copying failed. */
export const COPY_FAILED = "Couldn't copy. Select the codes and copy them by hand.";

/** The step's own texts. */
export interface CodesTexts {
  readonly title: string;
  readonly intro: string;
  readonly continueLabel: string;
}

/** The first login's codes. */
export const SIGN_IN_CODES: CodesTexts = {
  title: "Recovery codes",
  intro:
    "Each of these codes works once instead of a code from your app. They won't be shown again.",
  continueLabel: "Continue",
};

/** The recovery codes step. */
export function RecoveryCodesStep({
  codes,
  texts = SIGN_IN_CODES,
  pending,
  onContinue,
}: {
  codes: readonly string[];
  texts?: CodesTexts;
  /** The flow's call (the login that follows) is running. */
  pending: boolean;
  onContinue: () => Promise<void>;
}) {
  const [saved, setSaved] = useState(false);

  return (
    <AuthCard title={texts.title} intro={texts.intro}>
      <ul
        aria-label="Recovery codes"
        className="grid grid-cols-2 gap-1 rounded-lg border bg-muted p-3 font-mono text-sm select-text"
      >
        {codes.map((code) => (
          <li key={code}>{code}</li>
        ))}
      </ul>
      <Button
        type="button"
        variant="outline"
        className="self-start"
        onClick={() => {
          void copyWithToast(codes.join("\n"), COPIED, COPY_FAILED);
        }}
      >
        Copy all
      </Button>
      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          className="size-4 accent-primary"
          checked={saved}
          onChange={(event) => setSaved(event.target.checked)}
        />
        I've saved these codes
      </label>
      <Button
        type="button"
        disabled={!saved || pending}
        onClick={() => {
          void onContinue();
        }}
      >
        {texts.continueLabel}
      </Button>
    </AuthCard>
  );
}
