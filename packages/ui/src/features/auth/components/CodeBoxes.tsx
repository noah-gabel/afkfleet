// The six boxes of a 2FA code (Plan.md P11.11; ADR-0020, U86, U99, S100). There's one real input,
// holding the code and painting its own digits in the mono font, spaced by `ch` onto six empty cells
// beneath it. So there's one accessible name and one tab stop, caret, selection, arrows and Backspace
// are the browser's, autofill sees one `one-time-code` field, and no second copy of the code exists.
//
// A wrong code costs one of three tries, so what may fill the boxes is decided by `classifyCode`, in
// the event's handler, never an effect, which StrictMode would run twice. Nothing is ever filtered
// down to digits: a recovery code holds some, and they could add up to six.
//
// `locked` makes it read-only rather than disabled, which would drop the focus.
//
// The field is wider than the cells, so an outline around it would draw a box bigger than they are.
// So it's marked `data-focus-indicator="cells"`, which styles.css's one focus rule leaves alone, and
// the focus shows in the cells: all six on `--ring`, and the one the next digit goes to thicker, as
// the colour alone changes by under 3:1 (ADR-0020, U103, S121). The sweep reads them through the mark.

import { cn } from "cn";
import type { ClipboardEvent, Ref } from "react";

import type { ControlProps } from "../../../components/FormField";
import { classifyCode, DIGITS_ONLY, NOT_A_CODE } from "../schemas";

/** How many digits a code has. */
export const CODE_DIGITS = 6;

/** The cells, drawn under the input. */
const CELLS = Array.from({ length: CODE_DIGITS }, (_, index) => index);

/** The boxes; `value` is the digits so far, kept by the form. */
export function CodeBoxes({
  value,
  locked,
  inputRef,
  onChange,
  onFull,
  onRecovery,
  onRefused,
  ...control
}: ControlProps & {
  value: string;
  locked: boolean;
  inputRef: Ref<HTMLInputElement>;
  onChange: (digits: string) => void;
  /** All six are there: send them. */
  onFull: (digits: string) => void;
  /** A whole recovery code came in; without this, the step takes none. */
  onRecovery?: (code: string) => void;
  /** What to tell the user about input left out, or `undefined` once there's nothing to say. */
  onRefused: (message: string | undefined) => void;
}) {
  const refuse = (text: string, typed: boolean) => {
    const letters = typed && onRecovery !== undefined && /\p{L}/u.test(text);
    onRefused(letters ? DIGITS_ONLY : NOT_A_CODE);
  };

  const onInput = (text: string, typed: boolean) => {
    const read = classifyCode(text);
    if (read.kind === "digits") {
      onRefused(undefined);
      if (read.digits !== value) {
        onChange(read.digits);
        if (read.digits.length === CODE_DIGITS) {
          onFull(read.digits);
        }
      }
    } else if (read.kind === "recovery" && onRecovery !== undefined) {
      onRecovery(read.code);
    } else {
      refuse(text, typed);
    }
  };

  const onPaste = (event: ClipboardEvent<HTMLInputElement>) => {
    const pasted = event.clipboardData.getData("text");
    const read = classifyCode(pasted);
    if (locked) {
      event.preventDefault();
    } else if (read.kind === "digits" && read.digits.length === CODE_DIGITS) {
      // A whole code replaces what's there, and sends even if it's the same one again.
      event.preventDefault();
      onRefused(undefined);
      onChange(read.digits);
      onFull(read.digits);
    } else if (read.kind !== "digits") {
      event.preventDefault();
      onInput(pasted, false);
    }
    // A part goes in at the caret, by the browser, and comes to `onChange` below.
  };

  return (
    <div data-slot="code-boxes" className="group relative mx-auto h-12 w-82 max-w-full">
      <div aria-hidden className="absolute inset-y-0 left-6 flex gap-2">
        {CELLS.map((cell) => (
          <div
            key={cell}
            data-slot="code-cell"
            className={cn(
              "w-10 rounded-lg border border-input group-focus-within:border-ring",
              cell === Math.min(value.length, CODE_DIGITS - 1) && "group-focus-within:border-2",
            )}
          />
        ))}
      </div>
      <input
        {...control}
        ref={inputRef}
        type="text"
        name="code"
        inputMode="numeric"
        autoComplete="one-time-code"
        spellCheck={false}
        data-focus-indicator="cells"
        readOnly={locked}
        value={value}
        onChange={(event) => onInput(event.target.value, true)}
        onPaste={onPaste}
        className="absolute inset-0 size-full rounded-lg bg-transparent outline-none pl-[calc(1.5rem+(2.5rem-1ch)/2)] font-mono text-xl text-foreground [letter-spacing:calc(3rem-1ch)]"
      />
    </div>
  );
}
