// Cards that open what they show, on Home and Accounts (Plan.md P11.11; ADR-0020, U35, U88, U106,
// R210): the whole card is one link, stretched from its name over the card. What acts on its own, a
// button or a message whose text can be selected, sits above the link.

/** The cards' grid: three columns at 1200x800, fewer as the window narrows (U91). */
export const CARD_GRID = "grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(18rem,1fr))]";

/** A card that opens something: its hover marks the whole of it. */
export const LINKED_CARD = "relative h-full gap-2 p-3 transition-colors hover:border-primary";

/** The name's link, stretched over the card. */
export const STRETCHED_LINK = "after:absolute after:inset-0 after:content-['']";

/** What sits above the stretched link. */
export const ABOVE_LINK = "relative z-10";

/** A card's name, in the mono font: at most 16 characters, and a clip would cut its focus outline. */
export const CARD_NAME = "min-w-0 font-mono text-sm font-semibold break-all";
