// One labelled form field (Plan.md P8.7): the label, the control, an optional hint and the
// field's error, with the control's `aria-describedby` and `aria-invalid` set from them. The
// label's ID is `{id}-label`, so a control beside the field, such as a list entry's Remove, can
// be described by it. A field whose error comes without the user leaving it, such as a preview's
// refusal, asks for `errorRole`, so a screen reader hears it.
import type { ReactNode } from "react";

import { FieldMessage } from "./FieldMessage";
import { Label } from "./ui/label";

/** What the control needs from its field. */
export interface ControlProps {
  readonly id: string;
  readonly "aria-invalid": boolean;
  readonly "aria-describedby": string | undefined;
}

/** A field around the control `children` renders with `ControlProps`. */
export function FormField({
  id,
  label,
  hint,
  error,
  errorRole,
  children,
}: {
  id: string;
  label: string;
  hint?: string;
  error: string | undefined;
  errorRole?: "alert";
  children: (props: ControlProps) => ReactNode;
}) {
  const described = [
    ...(hint === undefined ? [] : [`${id}-hint`]),
    ...(error === undefined ? [] : [`${id}-error`]),
  ];
  return (
    <div className="flex flex-col gap-1.5">
      <Label id={`${id}-label`} htmlFor={id}>
        {label}
      </Label>
      {children({
        id,
        "aria-invalid": error !== undefined,
        "aria-describedby": described.length === 0 ? undefined : described.join(" "),
      })}
      {hint === undefined ? null : (
        <p id={`${id}-hint`} className="text-xs text-muted-foreground">
          {hint}
        </p>
      )}
      <FieldMessage id={`${id}-error`} message={error} role={errorRole} />
    </div>
  );
}
