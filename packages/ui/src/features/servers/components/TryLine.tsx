// "Try a line" (Plan.md P11.9; ADR-0020, U58, R148): a line from the server's chat, tried on
// the editor's formats as they are now, saved or not. The answer says which format reads which
// sender, counted from 1, or that none fits. It's kept with the formats and the line it
// answered and shows only while both are unchanged, so any change drops it and a late answer
// never shows against changed input. A refusal at `chat_formats[i]` shows at that format
// (through the editor), one at `line` under the box, and anything else as its text there.
// Try stays usable while "Confirm it's you" is up, since it changes nothing. It sits inside
// the editor's form, so Enter in the line tries rather than saving.
import { useRef, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { CommandError } from "../../../generated/CommandError";
import { useAlive } from "../../../hooks/useStepGuard";
import { fieldErrorsOf } from "../../../lib/formErrors";
import { usePreviewChatFormats } from "../hooks/usePreviewChatFormats";
import { formatReads, NO_FORMAT_FITS, TRY, TRY_LINE, TRY_LINE_PLACEHOLDER } from "../texts";

/** What was asked, and what to show for it. */
interface Answered {
  readonly formats: readonly string[];
  readonly line: string;
  /** The answer. */
  readonly said?: string;
  /** A refusal's text under the box. */
  readonly failed?: string;
  /** A refusal at the line. */
  readonly lineError?: string;
}

/** Whether `asked` is still what's on the screen. */
function same(
  asked: { formats: readonly string[]; line: string },
  formats: readonly string[],
  line: string,
) {
  return (
    asked.line === line &&
    asked.formats.length === formats.length &&
    asked.formats.every((format, index) => format === formats[index])
  );
}

/**
 * The box for `formats`, the editor's now. `clearFormatErrors` and `showFormatErrors` reach the
 * editor's format fields: a try clears what an earlier one put there, and puts a refusal at the
 * format it names, true if any landed.
 */
export function TryLine({
  id,
  formats,
  clearFormatErrors,
  showFormatErrors,
}: {
  id: string;
  formats: readonly string[];
  clearFormatErrors: () => void;
  showFormatErrors: (error: CommandError) => boolean;
}) {
  const preview = usePreviewChatFormats();
  const alive = useAlive();
  const [line, setLine] = useState("");
  const [answered, setAnswered] = useState<Answered | null>(null);
  // What's on the screen when an answer comes: a late one for other input is dropped.
  const now = useRef({ formats, line });
  now.current = { formats, line };

  async function tryIt(): Promise<void> {
    const asked = { formats: [...formats], line };
    setAnswered(null);
    clearFormatErrors();
    let said: string;
    try {
      const found = await preview.mutateAsync({ chat_formats: asked.formats, line: asked.line });
      said =
        found.sender === undefined || found.format === undefined
          ? NO_FORMAT_FITS
          : formatReads(found.format + 1, found.sender);
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (!alive.current || !same(asked, now.current.formats, now.current.line)) {
        return;
      }
      const lineError = fieldErrorsOf(error)
        .filter((field) => field.path === "line")
        .map((field) => field.message);
      const placed = showFormatErrors(error);
      setAnswered(
        lineError.length > 0
          ? { ...asked, lineError: lineError.join("\n") }
          : { ...asked, ...(placed ? {} : { failed: describeError(error) }) },
      );
      return;
    }
    if (alive.current && same(asked, now.current.formats, now.current.line)) {
      setAnswered({ ...asked, said });
    }
  }

  const shown = answered !== null && same(answered, formats, line) ? answered : null;
  return (
    <div className="flex flex-col gap-2">
      <FormField id={id} label={TRY_LINE} error={shown?.lineError}>
        {(control) => (
          <Input
            {...control}
            autoComplete="off"
            placeholder={TRY_LINE_PLACEHOLDER}
            value={line}
            onChange={(event) => setLine(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                // Inside the editor's form: Enter tries the line, never saves.
                event.preventDefault();
                if (!preview.isPending) {
                  void tryIt();
                }
              }
            }}
          />
        )}
      </FormField>
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="self-start"
        disabled={preview.isPending}
        onClick={() => void tryIt()}
      >
        {TRY}
      </Button>
      {shown?.said === undefined ? null : (
        <p role="status" className="text-sm">
          {shown.said}
        </p>
      )}
      {shown?.failed === undefined ? null : (
        <p role="alert" className="text-sm text-destructive">
          {shown.failed}
        </p>
      )}
    </div>
  );
}
