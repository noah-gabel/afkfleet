// What a bot sees, made readable (Plan.md P16.20): the empty slots as ranges of numbers. Pure.

/** `numbers`, in order, as ranges: `0–3, 5, 7–8`. */
export function ranges(numbers: readonly number[]): string {
  const parts: string[] = [];
  let start: number | undefined;
  let last: number | undefined;
  const close = () => {
    if (start !== undefined && last !== undefined) {
      parts.push(start === last ? `${start}` : `${start}–${last}`);
    }
  };
  for (const number of numbers) {
    if (last !== undefined && number === last + 1) {
      last = number;
      continue;
    }
    close();
    start = number;
    last = number;
  }
  close();
  return parts.join(", ");
}
