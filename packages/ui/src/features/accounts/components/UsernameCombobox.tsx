// "Share with"'s field (Plan.md P11.11; ADR-0020, U110, S127): an ARIA 1.2 combobox, which suggests
// users as a name is typed, so one can be picked instead of typed exactly. The focus stays in the
// field, which names the active option, and the list sits below it in the page, so nothing floats and
// no library is needed. Picking only fills the field: the form sends only on Share, or Enter with no
// option active, and typing an exact name and pressing Share works whatever the suggestions do. A
// read that fails or is limited shows nothing, as the field needs none of it, and Share has its own
// answers. Asks after a pause in the typing, as each read spends the user's limit.
import { cn } from "cn";
import {
  type ChangeEventHandler,
  type FocusEventHandler,
  type KeyboardEvent,
  type Ref,
  useEffect,
  useId,
  useState,
} from "react";

import { useGrantCandidates } from "../../../api/queries";
import type { ControlProps } from "../../../components/FormField";
import { Input } from "../../../components/ui/input";
import { MATCHING_USERS } from "../texts";

/** How long, in milliseconds, the typing pauses before the server is asked. */
const PAUSE_MS = 250;

/** `value`, once it has held still for `PAUSE_MS`. */
function usePaused(value: string): string {
  const [paused, setPaused] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => {
      setPaused(value);
    }, PAUSE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [value]);
  return paused;
}

/** The field; `searching` is whether suggestions may be asked for at all. */
export function UsernameCombobox({
  account,
  searching,
  value,
  name,
  onChange,
  onBlur,
  onPick,
  inputRef,
  ...control
}: ControlProps & {
  account: string;
  searching: boolean;
  value: string;
  name: string;
  onChange: ChangeEventHandler<HTMLInputElement>;
  onBlur: FocusEventHandler<HTMLInputElement>;
  inputRef: Ref<HTMLInputElement>;
  onPick: (username: string) => void;
}) {
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState<number | null>(null);
  const paused = usePaused(value);
  const asked = useGrantCandidates(account, paused, searching && open);
  const start = value.toLowerCase();
  // What's shown always matches what's typed, however old the answer.
  const names = (asked.data?.usernames ?? []).filter((one) => one.startsWith(start));
  const shown = open && searching && value !== "" && names.length > 0;
  const optionId = (index: number) => `${listId}-${index}`;
  const current = shown && active !== null && active < names.length ? active : null;

  function pick(username: string): void {
    onPick(username);
    setOpen(false);
    setActive(null);
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>): void {
    if (!shown) {
      return;
    }
    const last = names.length - 1;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive(current === null || current === last ? 0 : current + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive(current === null || current === 0 ? last : current - 1);
    } else if (event.key === "Escape") {
      event.preventDefault();
      setOpen(false);
      setActive(null);
    } else if (event.key === "Enter" && current !== null) {
      const chosen = names.at(current);
      if (chosen !== undefined) {
        // It only fills the field: this Enter never sends the form.
        event.preventDefault();
        pick(chosen);
      }
    }
  }

  return (
    <div className="flex flex-col gap-1">
      <Input
        {...control}
        ref={inputRef}
        name={name}
        value={value}
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={shown}
        aria-controls={shown ? listId : undefined}
        aria-activedescendant={current === null ? undefined : optionId(current)}
        autoComplete="off"
        spellCheck={false}
        className="font-mono"
        onChange={(event) => {
          setOpen(true);
          setActive(null);
          void onChange(event);
        }}
        onBlur={(event) => {
          setOpen(false);
          setActive(null);
          void onBlur(event);
        }}
        onKeyDown={onKeyDown}
      />
      {shown ? (
        <div
          id={listId}
          role="listbox"
          aria-label={MATCHING_USERS}
          className="flex flex-col rounded-lg border bg-popover p-1 text-popover-foreground"
        >
          {names.map((one, index) => (
            // The focus stays in the field, which the keys above go through, so an option takes none.
            <div
              key={one}
              id={optionId(index)}
              role="option"
              tabIndex={-1}
              aria-selected={index === current}
              className={cn(
                "cursor-default rounded-md px-2 py-1 font-mono text-sm break-all hover:bg-accent",
                index === current && "bg-accent text-accent-foreground",
              )}
              onMouseDown={(event) => {
                // Keeps the focus in the field, whose blur would close the list before a click came.
                event.preventDefault();
                pick(one);
              }}
            >
              {one}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
