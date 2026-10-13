// A field's error under it (Plan.md P8.7): its input points here with
// `aria-describedby` and sets `aria-invalid`, so a screen reader reads the rule with
// the field. The text is the rule the field broke (the zod schema's, or the Rust side's
// `fields`), never the value; several rules come one per line.

/** The message `id` names, or nothing without one. */
export function FieldMessage({ id, message }: { id: string; message: string | undefined }) {
  if (message === undefined) {
    return null;
  }
  return (
    <p id={id} className="text-sm whitespace-pre-line text-destructive">
      {message}
    </p>
  );
}
