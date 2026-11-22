// What a conversion to a script says differs, step by step, with each step named as the editor
// names it: its section's heading and its number there (Plan.md P15.27; ADR-0046). The app's Rust
// side numbers a note's step by its place in the request's steps, which `stepsOf` builds section
// after section, so the same walk names it.
import type { ConversionNote } from "../../generated/ConversionNote";
import { type ModeValues, SECTIONS, type Section } from "./modeForm";
import { NOTE_TEXT, SECTION_TEXT, stepLabel, stepPlace } from "./texts";

/** Each request step's name, in the request's order. */
export function placesOf(values: Pick<ModeValues, Section>): readonly string[] {
  return SECTIONS.flatMap((section) =>
    values[section].map((_, index) => stepPlace(SECTION_TEXT[section].heading, index + 1)),
  );
}

/** `place` inside a sentence: its heading's first letter small. */
function inSentence(place: string): string {
  return place.charAt(0).toLowerCase() + place.slice(1);
}

/** `note` as a line of the notice, its step named from `places`. */
export function noteLine(note: ConversionNote, places: readonly string[]): string {
  const name = (step: number) => places[step] ?? stepLabel(step + 1);
  const everyMs = note.every_ms ?? 0;
  const text = (() => {
    switch (note.kind) {
      case "swing_skips_entities":
        return NOTE_TEXT.swing_skips_entities();
      case "slower":
        return NOTE_TEXT.slower(everyMs);
      case "hold_repeats":
        return NOTE_TEXT.hold_repeats();
      case "mount_skips_occupied":
        return NOTE_TEXT.mount_skips_occupied();
      case "gap_capped":
        return NOTE_TEXT.gap_capped();
      case "shares_limit":
        return NOTE_TEXT.shares_limit(everyMs, inSentence(name(note.other ?? note.step)));
    }
  })();
  return `${name(note.step)}: ${text}`;
}
